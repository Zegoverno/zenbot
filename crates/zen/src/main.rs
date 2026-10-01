//! zen — command-line interface to a running zenbot kernel (zend).
//! Designed for agents first: every command supports --json and exits non-zero on failure.

mod client;
mod editor;
mod md;
mod tui;

use std::io::{IsTerminal, Read, Write};

use client::{dim, short, tool_summary, Client, Ws};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

#[derive(Parser)]
#[command(name = "zen", version, about = "zenbot in your terminal. Run `zen` for an interactive session, or use the commands below for scripts.")]
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
    /// Continue the most recent session
    #[arg(short = 'c', long = "continue")]
    cont: bool,
    /// Resume a session (opens a picker when no id is given)
    #[arg(short = 'r', long, num_args = 0..=1, default_missing_value = "")]
    resume: Option<String>,
    /// Model for a new session
    #[arg(short, long)]
    model: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
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
    /// Interactive session in the terminal (same as running `zen` with no command)
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
    /// Sign in to the model engines: Claude Code and Codex (default), or one of: claude, codex, pi
    Login {
        /// Which engine to sign in to (default: every engine that isn't signed in yet)
        which: Option<String>,
    },
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
                                turn.input_tokens += u["input"].as_i64().unwrap_or(0) + u["cacheRead"].as_i64().unwrap_or(0) + u["cacheWrite"].as_i64().unwrap_or(0);
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
                    "usage" if started => {
                        turn.input_tokens += ev["input"].as_i64().unwrap_or(0);
                        turn.output_tokens += ev["output"].as_i64().unwrap_or(0);
                        turn.cost += ev["cost"].as_f64().unwrap_or(0.0);
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

/// Read piped stdin. With a prompt argument, stdin is optional: if nothing arrives quickly
/// (e.g. an open but idle pipe), carry on without it instead of blocking.
fn read_stdin(optional: bool) -> String {
    if std::io::stdin().is_terminal() {
        return String::new();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        let _ = tx.send(s);
    });
    if optional {
        rx.recv_timeout(std::time::Duration::from_millis(300)).unwrap_or_default()
    } else {
        rx.recv().unwrap_or_default()
    }
}

fn read_prompt(prompt: Option<String>) -> Result<String> {
    let needs_stdin = matches!(prompt.as_deref(), None | Some("-"));
    let piped = read_stdin(!needs_stdin);
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

/// Values from ~/.zenbot/env (written by the installer).
fn zen_env() -> std::collections::HashMap<String, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    std::fs::read_to_string(format!("{home}/.zenbot/env"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn zen_path() -> String {
    format!("{}:{}", zen_env().get("PATH").cloned().unwrap_or_default(), std::env::var("PATH").unwrap_or_default())
}

fn signed_in(engine: &str) -> bool {
    let run = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).env("PATH", zen_path()).output().ok();
    match engine {
        "claude" => run("claude", &["auth", "status"]).is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("\"loggedIn\": true")),
        "codex" => run("codex", &["login", "status"]).is_some_and(|o| o.status.success()),
        _ => false,
    }
}

fn run_login(cmd: &str, args: &[&str], dir: Option<&str>) -> Result<()> {
    let mut c = std::process::Command::new(cmd);
    c.args(args).env("PATH", zen_path());
    if let Some(d) = dir {
        c.current_dir(d);
    }
    let status = c.status().with_context(|| format!("running {cmd} (is it installed?)"))?;
    if !status.success() {
        bail!("{cmd} sign-in failed");
    }
    Ok(())
}

fn login(which: Option<&str>) -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let targets: Vec<&str> = match which {
        Some(w) => vec![w],
        None => ["claude", "codex"].into_iter().filter(|e| !signed_in(e)).collect(),
    };
    if targets.is_empty() {
        eprintln!("Claude Code and Codex are both signed in. (Use `zen login pi` for Pi's direct ChatGPT sign-in.)");
        return Ok(());
    }
    for t in targets {
        match t {
            "claude" => {
                eprintln!("\n== Claude (your Claude subscription)\nOpen the link, approve, and paste the code back here if asked.\n");
                run_login("claude", &["auth", "login", "--claudeai"], None)?;
            }
            "codex" => {
                eprintln!("\n== Codex (your ChatGPT subscription)\nOpen the link and enter the code shown.\n");
                run_login("codex", &["login", "--device-auth"], None)?;
            }
            "pi" => {
                let mind = std::env::var("ZEN_MIND_DIR").ok().or_else(|| zen_env().get("ZEN_MIND_DIR").cloned()).unwrap_or(format!("{home}/zenbot/packages/mind"));
                let cli = format!("{mind}/node_modules/@earendil-works/pi-ai/dist/cli.js");
                if !std::path::Path::new(&cli).exists() {
                    bail!("Pi isn't installed; install with ZEN_WORKERS=engine,pi ./install.sh");
                }
                let dir = format!("{home}/.zenbot");
                std::fs::create_dir_all(&dir)?;
                eprintln!("\n== Pi (direct ChatGPT sign-in)\nOpen the link and approve. Your browser then lands on a 127.0.0.1 page that won't load; copy that full address and paste it here.\n");
                run_login("node", &[&cli, "login", "openai"], Some(&dir))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(format!("{dir}/auth.json"), std::fs::Permissions::from_mode(0o600));
                }
            }
            other => bail!("unknown engine `{other}`; use claude, codex or pi"),
        }
    }
    eprintln!("\nSigned in. Run `zen` to start.");
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
    if let Some(Cmd::Login { which }) = &cli.cmd {
        return login(which.as_deref());
    }
    let c = Client::new(cli.url, cli.token)?;
    let out = |v: &Value| println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    let Some(cmd) = cli.cmd else {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            // Piped use without a command behaves like `zen ask`.
            return ask(&c, cli.json, None, None, cli.model, false).await;
        }
        let start = match (cli.cont, cli.resume) {
            (_, Some(id)) if !id.is_empty() => tui::Start::Resume(Some(id)),
            (_, Some(_)) => tui::Start::Resume(None),
            (true, None) => tui::Start::Continue,
            _ => tui::Start::New,
        };
        return tui::run(c, start, cli.model).await;
    };
    match cmd {
        Cmd::Ask { prompt, session, model, quiet } => ask(&c, cli.json, prompt, session, model, quiet).await?,
        Cmd::Chat { session, model } => {
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                let start = session.map(|s| tui::Start::Resume(Some(s))).unwrap_or(tui::Start::New);
                tui::run(c, start, model).await?
            } else {
                chat(&c, session, model).await?
            }
        }
        Cmd::Login { .. } => unreachable!(),
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
            let any_signed_in = models["authenticated"].as_object().is_some_and(|a| a.values().any(|v| v == true));
            let status = json!({
                "url": c.url,
                "ok": health["ok"] == true && any_signed_in,
                "workers": health["workers"],
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
                for (name, ok) in health["workers"].as_object().into_iter().flatten() {
                    println!("{:<9} {}", format!("worker:{name}"), mark(*ok == true));
                }
                for (name, ok) in models["authenticated"].as_object().into_iter().flatten() {
                    println!("{:<9} {}", name, if *ok == true { "signed in" } else { "NOT signed in" });
                }
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
