//! zen — command-line interface to a running zenbot kernel (zend).
//! Designed for agents first: every command supports --json and exits non-zero on failure.

use std::io::{IsTerminal, Read, Write};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

#[derive(Parser)]
#[command(name = "zen", version, about = "Talk to zenbot from the terminal (or from other agents).")]
struct Cli {
    /// Kernel URL
    #[arg(long, env = "ZEN_URL", default_value = "http://127.0.0.1:8100", global = true)]
    url: String,
    /// Access token (default: contents of ~/.zenbot/token)
    #[arg(long, env = "ZEN_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,
    /// Print machine-readable JSON
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one task and print the answer. Reads the prompt from stdin when omitted or "-".
    Ask {
        /// The prompt
        prompt: Option<String>,
        /// Continue an existing session (id or unique prefix); default creates a new one
        #[arg(short, long)]
        session: Option<String>,
        /// Model for a new session, e.g. openai/gpt-6.1-sol
        #[arg(short, long)]
        model: Option<String>,
        /// Don't show tool activity on stderr
        #[arg(short, long)]
        quiet: bool,
    },
    /// Interactive session in the terminal
    Chat {
        /// Continue an existing session (id or unique prefix)
        #[arg(short, long)]
        session: Option<String>,
        /// Model for a new session
        #[arg(short, long)]
        model: Option<String>,
    },
    /// Manage sessions
    #[command(subcommand)]
    Sessions(SessionsCmd),
    /// List available models
    Models,
    /// Check that the kernel, database, worker and model sign-in are healthy
    Status,
}

#[derive(Subcommand)]
enum SessionsCmd {
    /// List sessions
    Ls {
        /// Show archived sessions instead
        #[arg(long)]
        archived: bool,
    },
    /// Create a session
    New {
        #[arg(short, long)]
        model: Option<String>,
        #[arg(short, long)]
        title: Option<String>,
    },
    /// Show a session's messages
    Show { id: String },
    /// Archive a session
    Archive { id: String },
    /// Restore an archived session
    Restore { id: String },
    /// Rename a session
    Rename { id: String, title: String },
}

struct Client {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl Client {
    fn new(url: String, token: Option<String>) -> Result<Self> {
        let token = match token {
            Some(t) => t,
            None => {
                let home = std::env::var("HOME").context("HOME not set")?;
                std::fs::read_to_string(format!("{home}/.zenbot/token"))
                    .context("no token: set ZEN_TOKEN or create ~/.zenbot/token")?
                    .trim()
                    .to_string()
            }
        };
        Ok(Client { http: reqwest::Client::new(), url: url.trim_end_matches('/').to_string(), token })
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self.http.request(method, format!("{}{}", self.url, path)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.with_context(|| format!("cannot reach zenbot at {}", self.url))?;
        let status = res.status();
        let data: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{}: {}", status, data["error"].as_str().unwrap_or("request failed"));
        }
        Ok(data)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.call(reqwest::Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }

    async fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.call(reqwest::Method::PATCH, path, Some(body)).await
    }

    /// Resolve a full id or a unique prefix, searching active and archived sessions.
    async fn resolve(&self, id: &str) -> Result<String> {
        let mut all = self.get("/api/sessions?archived=false").await?.as_array().cloned().unwrap_or_default();
        all.extend(self.get("/api/sessions?archived=true").await?.as_array().cloned().unwrap_or_default());
        let matches: Vec<&Value> = all.iter().filter(|s| s["id"].as_str().is_some_and(|x| x.starts_with(id))).collect();
        match matches.len() {
            1 => Ok(matches[0]["id"].as_str().unwrap().to_string()),
            0 => bail!("no session matches `{id}`"),
            n => bail!("`{id}` matches {n} sessions; use more characters"),
        }
    }

    async fn new_session(&self, model: Option<String>) -> Result<String> {
        let s = self.post("/api/sessions", json!({ "model": model })).await?;
        Ok(s["id"].as_str().context("bad session response")?.to_string())
    }

    async fn connect(&self, id: &str) -> Result<Ws> {
        let ws_url = format!("{}/api/sessions/{id}/ws?token={}", self.url.replacen("http", "ws", 1), self.token);
        let (ws, _) = tokio_tungstenite::connect_async(ws_url).await.context("opening session stream")?;
        Ok(ws)
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn dim(s: &str) -> String {
    if std::io::stderr().is_terminal() {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn tool_summary(name: &str, args: &Value) -> String {
    let detail = args["command"]
        .as_str()
        .or(args["path"].as_str())
        .map(str::to_string)
        .or_else(|| args["from"].as_str().map(|f| format!("{f} → {}", args["to"].as_str().unwrap_or(""))))
        .unwrap_or_else(|| args.to_string());
    let detail: String = detail.lines().next().unwrap_or("").chars().take(120).collect();
    format!("{name} {detail}")
}

/// Outcome of one turn, collected from the session stream.
#[derive(Default)]
struct Turn {
    text: String,
    tools: Vec<Value>,
    input_tokens: i64,
    output_tokens: i64,
    cost: f64,
    model: String,
    error: Option<String>,
}

/// Send a prompt and stream the turn. `show` prints live text to stdout and tools to stderr.
async fn run_turn(ws: &mut Ws, prompt: &str, show: bool, show_tools: bool) -> Result<Turn> {
    ws.send(Message::text(json!({ "type": "prompt", "text": prompt }).to_string())).await?;
    let mut turn = Turn::default();
    let mut started = false;
    let mut streamed = false;
    let mut stdout = std::io::stdout();
    loop {
        tokio::select! {
            msg = ws.next() => {
                let Some(msg) = msg else { bail!("connection to zenbot closed") };
                let Message::Text(text) = msg? else { continue };
                let ev: Value = serde_json::from_str(&text)?;
                match ev["type"].as_str().unwrap_or("") {
                    "message" => {
                        let m = &ev["message"];
                        match m["role"].as_str() {
                            Some("user") if m["content"].as_str() == Some(prompt) => started = true,
                            Some("assistant") if started => {
                                let text: String = m["content"].as_array().into_iter().flatten()
                                    .filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("");
                                if !text.is_empty() {
                                    if !turn.text.is_empty() { turn.text.push_str("\n\n"); }
                                    turn.text.push_str(&text);
                                    if show && !streamed { print!("{text}"); }
                                    if show { println!(); }
                                }
                                streamed = false;
                                let u = &m["usage"];
                                turn.input_tokens += u["input"].as_i64().unwrap_or(0);
                                turn.output_tokens += u["output"].as_i64().unwrap_or(0);
                                turn.cost += u["cost"]["total"].as_f64().unwrap_or(0.0);
                                turn.model = m["model"].as_str().unwrap_or("").to_string();
                                if m["stopReason"] == "error" {
                                    turn.error = m["errorMessage"].as_str().map(str::to_string);
                                }
                            }
                            _ => {}
                        }
                    }
                    "delta" if started && show => {
                        print!("{}", ev["delta"].as_str().unwrap_or(""));
                        stdout.flush().ok();
                        streamed = true;
                    }
                    "tool_start" if started => {
                        if show_tools { eprintln!("{}", dim(&format!("  ▸ {}", tool_summary(ev["name"].as_str().unwrap_or(""), &ev["args"])))); }
                        turn.tools.push(json!({ "id": ev["call_id"], "name": ev["name"], "args": ev["args"] }));
                    }
                    "tool_end" if started => {
                        if let Some(t) = turn.tools.iter_mut().find(|t| t["id"] == ev["call_id"]) {
                            t["is_error"] = ev["is_error"].clone();
                            t["ms"] = ev["ms"].clone();
                        }
                        if show_tools && ev["is_error"] == true { eprintln!("{}", dim("    (failed)")); }
                    }
                    "end" if started => {
                        if let Some(e) = ev["error"].as_str() { turn.error = Some(e.to_string()); }
                        return Ok(turn);
                    }
                    "error" => bail!("{}", ev["error"].as_str().unwrap_or("error")),
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                ws.send(Message::text(json!({ "type": "abort" }).to_string())).await.ok();
                eprintln!("{}", dim("  (stopping…)"));
            }
        }
    }
}

fn turn_json(session: &str, t: &Turn) -> Value {
    json!({
        "session_id": session,
        "text": t.text,
        "model": t.model,
        "tools": t.tools,
        "usage": { "input_tokens": t.input_tokens, "output_tokens": t.output_tokens, "cost_usd_api_equivalent": t.cost },
        "error": t.error,
    })
}

fn read_prompt(prompt: Option<String>) -> Result<String> {
    let piped = if !std::io::stdin().is_terminal() {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        String::new()
    };
    let text = match prompt.as_deref() {
        None | Some("-") => piped,
        Some(p) if piped.trim().is_empty() => p.to_string(),
        Some(p) => format!("{p}\n\n{piped}"),
    };
    if text.trim().is_empty() {
        bail!("empty prompt: pass it as an argument or on stdin");
    }
    Ok(text)
}

async fn ask(c: &Client, json_out: bool, prompt: Option<String>, session: Option<String>, model: Option<String>, quiet: bool) -> Result<()> {
    let text = read_prompt(prompt)?;
    let id = match session {
        Some(s) => c.resolve(&s).await?,
        None => c.new_session(model).await?,
    };
    let mut ws = c.connect(&id).await?;
    let turn = run_turn(&mut ws, &text, !json_out, !json_out && !quiet).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&turn_json(&id, &turn))?);
    } else if !quiet {
        eprintln!("{}", dim(&format!("  {} · {} tokens · session {}", turn.model, turn.input_tokens + turn.output_tokens, short(&id))));
    }
    if let Some(e) = turn.error {
        if !json_out {
            eprintln!("error: {e}");
        }
        std::process::exit(1);
    }
    Ok(())
}

async fn chat(c: &Client, session: Option<String>, model: Option<String>) -> Result<()> {
    let id = match session {
        Some(s) => c.resolve(&s).await?,
        None => c.new_session(model).await?,
    };
    let info = c.get(&format!("/api/sessions/{id}")).await?;
    eprintln!("{}", dim(&format!("zenbot · {} · session {} · Ctrl-C stops a turn, Ctrl-D exits", info["model"].as_str().unwrap_or(""), short(&id))));
    let mut ws = c.connect(&id).await?;
    let stdin = std::io::stdin();
    loop {
        eprint!("\n› ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            eprintln!();
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/exit" || line == "/quit" {
            break;
        }
        println!();
        let turn = run_turn(&mut ws, line, true, true).await?;
        if let Some(e) = turn.error {
            eprintln!("error: {e}");
        }
    }
    eprintln!("{}", dim(&format!("session {id}")));
    Ok(())
}

fn print_sessions(list: &Value) {
    let rows = list.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("No sessions.");
        return;
    }
    for s in rows {
        let model = s["model"].as_str().unwrap_or("");
        println!(
            "{}  {:<16}  {}  {}",
            short(s["id"].as_str().unwrap_or("")),
            model.split('/').next_back().unwrap_or(model),
            s["updated_at"].as_str().unwrap_or("").get(..16).unwrap_or("").replace('T', " "),
            s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(untitled)")
        );
    }
}

fn print_messages(s: &Value) {
    println!("{}  ({})", s["title"].as_str().unwrap_or(""), s["model"].as_str().unwrap_or(""));
    for m in s["messages"].as_array().into_iter().flatten() {
        match m["role"].as_str() {
            Some("user") => println!("\n› {}", m["content"].as_str().unwrap_or("")),
            Some("assistant") => {
                for c in m["content"].as_array().into_iter().flatten() {
                    match c["type"].as_str() {
                        Some("text") => println!("\n{}", c["text"].as_str().unwrap_or("")),
                        Some("toolCall") => println!("{}", dim(&format!("  ▸ {}", tool_summary(c["name"].as_str().unwrap_or(""), &c["arguments"])))),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let c = Client::new(cli.url, cli.token)?;
    let out = |v: &Value| println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    match cli.cmd {
        Cmd::Ask { prompt, session, model, quiet } => ask(&c, cli.json, prompt, session, model, quiet).await?,
        Cmd::Chat { session, model } => chat(&c, session, model).await?,
        Cmd::Models => {
            let m = c.get("/api/models").await?;
            if cli.json {
                out(&m);
            } else {
                for x in m["models"].as_array().into_iter().flatten() {
                    let id = x["id"].as_str().unwrap_or("");
                    println!("{}{}", id, if Some(id) == m["default"].as_str() { "  (default)" } else { "" });
                }
            }
        }
        Cmd::Status => {
            let health: Value = reqwest::get(format!("{}/health", c.url)).await.with_context(|| format!("cannot reach zenbot at {}", c.url))?.json().await?;
            let models = c.get("/api/models").await?;
            let status = json!({
                "url": c.url,
                "ok": health["ok"] == true && models["authenticated"]["openai"] == true,
                "kernel": true,
                "database": health["db"],
                "worker": health["mind"],
                "signed_in": models["authenticated"],
                "default_model": models["default"],
            });
            if cli.json {
                out(&status);
            } else {
                let mark = |b: bool| if b { "ok" } else { "FAIL" };
                println!("kernel    {}  ({})", mark(true), c.url);
                println!("database  {}", mark(health["db"] == true));
                println!("worker    {}", mark(health["mind"] == true));
                println!("chatgpt   {}", if models["authenticated"]["openai"] == true { "signed in" } else { "NOT signed in" });
                println!("model     {}", models["default"].as_str().unwrap_or(""));
            }
            if status["ok"] != true {
                std::process::exit(1);
            }
        }
        Cmd::Sessions(cmd) => match cmd {
            SessionsCmd::Ls { archived } => {
                let list = c.get(&format!("/api/sessions?archived={archived}")).await?;
                if cli.json { out(&list) } else { print_sessions(&list) }
            }
            SessionsCmd::New { model, title } => {
                let s = c.post("/api/sessions", json!({ "model": model, "title": title })).await?;
                if cli.json { out(&s) } else { println!("{}", s["id"].as_str().unwrap_or("")) }
            }
            SessionsCmd::Show { id } => {
                let id = c.resolve(&id).await?;
                let s = c.get(&format!("/api/sessions/{id}")).await?;
                if cli.json { out(&s) } else { print_messages(&s) }
            }
            SessionsCmd::Archive { id } => {
                let id = c.resolve(&id).await?;
                let s = c.patch(&format!("/api/sessions/{id}"), json!({ "archived": true })).await?;
                if cli.json { out(&s) } else { println!("archived {}", short(&id)) }
            }
            SessionsCmd::Restore { id } => {
                let id = c.resolve(&id).await?;
                let s = c.patch(&format!("/api/sessions/{id}"), json!({ "archived": false })).await?;
                if cli.json { out(&s) } else { println!("restored {}", short(&id)) }
            }
            SessionsCmd::Rename { id, title } => {
                let id = c.resolve(&id).await?;
                let s = c.patch(&format!("/api/sessions/{id}"), json!({ "title": title })).await?;
                if cli.json { out(&s) } else { println!("renamed {}", short(&id)) }
            }
        },
    }
    Ok(())
}
