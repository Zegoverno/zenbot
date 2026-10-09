//! zen — command-line interface to a running zenbot kernel (zend).
//! Designed for agents first: every command supports --json and exits non-zero on failure.

mod client;
mod editor;
mod md;
mod screen;
mod tui;

use std::io::{IsTerminal, Read, Write};

use client::{assistant_text, describe_update, dim, enc, record_total, short, tool_summary, usage_total, zen_home, Client, NewSession, Ws};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use zen_proto::text_of;

#[derive(Parser)]
#[command(name = "zen", version, about = "zenbot in your terminal. Run `zen` for an interactive session, or use the commands below for scripts.")]
struct Cli {
    /// Kernel URL
    #[arg(long, env = "ZEN_URL", default_value = "http://127.0.0.1:8100", global = true)]
    url: String,
    /// Access token (default: contents of $ZEN_HOME/token, ~/.zenbot/token)
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
    /// Thinking level for a new session, e.g. high (`zen models` lists each model's levels)
    #[arg(short, long)]
    effort: Option<String>,
    /// Inline terminal app (no full-screen clearing; the input follows the conversation)
    #[arg(long, env = "ZEN_INLINE", global = true)]
    inline: bool,
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
        /// Thinking level for a new session, e.g. high
        #[arg(short, long)]
        effort: Option<String>,
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
        /// Thinking level for a new session
        #[arg(short, long)]
        effort: Option<String>,
    },
    /// Manage sessions
    #[command(subcommand)]
    Sessions(SessionsCmd),
    /// List available models
    Models,
    /// Sign in to the model engines: Claude Code and Codex (default), or one of: claude, codex
    Login {
        /// Which engine to sign in to (default: every engine that isn't signed in yet)
        which: Option<String>,
    },
    /// Check that the kernel, database, worker and model sign-in are healthy
    Status,
    /// zenbot's skills (active and drafts) with how often they're used, and the tools it made
    Skills {
        #[command(subcommand)]
        cmd: Option<ReviewCmd>,
    },
    /// The routing policy for subagents (which model per kind of work) and the evidence for it
    Policy {
        #[command(subcommand)]
        cmd: Option<PolicyCmd>,
    },
    /// Approve or reject a tool zenbot made (`zen skills` lists them)
    Tools {
        #[command(subcommand)]
        cmd: ReviewCmd,
    },
    /// Show zenbot's memory (short-term by default) and the last sleep; `zen memory sleep` tidies it now
    Memory {
        #[command(subcommand)]
        cmd: Option<MemoryCmd>,
        /// Which memories: short (default), archived or all
        #[arg(long, default_value = "short")]
        tier: String,
    },
    /// Suggested next prompts: what became of them (accepted, edited, declined) by prompt version, and the latest
    Suggestions,
    /// Scheduled jobs: the kernel's own (sleep, engines) and agent jobs; `zen jobs` lists them
    Jobs {
        #[command(subcommand)]
        cmd: Option<JobsCmd>,
    },
    /// Update zenbot to the latest version on GitHub (main) and restart it
    Upgrade {
        /// Only check whether an update is available
        #[arg(long)]
        check: bool,
    },
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
        effort: Option<String>,
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
    /// Record your decision on a session's work so far: accept, more, reshape or drop
    Decide {
        id: String,
        decision: String,
        /// Why, in a few words
        #[arg(short, long)]
        note: Option<String>,
    },
}

#[derive(Subcommand)]
enum PolicyCmd {
    /// Route a kind of work (understand, shape, build, verify, maintain, reflect, reach, bet, default) to a model
    Set {
        kind: String,
        model: String,
        /// Other models to try now and then, comma-separated
        #[arg(long)]
        candidates: Option<String>,
        /// Share of subtasks that try a candidate (default 0.1)
        #[arg(long)]
        explore: Option<f64>,
    },
    /// Go back to the policy before the latest change
    Undo,
}

#[derive(Subcommand)]
enum ReviewCmd {
    /// Accept: activate a draft skill (domain/name), or let a tool use the network
    Accept { name: String },
    /// Reject: archive a draft skill, or stop a tool from running
    Reject { name: String },
}

impl ReviewCmd {
    /// The name and the decision to send.
    fn parts(self) -> (String, &'static str) {
        match self {
            ReviewCmd::Accept { name } => (name, "accept"),
            ReviewCmd::Reject { name } => (name, "reject"),
        }
    }
}

#[derive(Subcommand)]
enum JobsCmd {
    /// Create an agent job: a prompt run in a fresh session on a schedule
    Add {
        /// Lowercase letters, digits and hyphens, e.g. morning-brief
        name: String,
        /// When: a cron expression (`0 7 * * 1-5`), `every 2h`, `at 2026-10-12 07:00` or `in 30m`
        #[arg(short, long)]
        schedule: String,
        /// The task, self-contained
        #[arg(short, long)]
        prompt: String,
        /// IANA timezone the schedule is read in (default America/Sao_Paulo)
        #[arg(long)]
        tz: Option<String>,
        /// Instruction parts besides the soul, comma-separated: identity, agents, user, memory, skills, project (default user,memory)
        #[arg(long)]
        context: Option<String>,
        /// Skills loaded into its instructions, comma-separated (domain/name)
        #[arg(long)]
        skills: Option<String>,
        /// Model (default: the default model when it runs)
        #[arg(short, long)]
        model: Option<String>,
        /// Directory it works in (default: the workspace)
        #[arg(long)]
        dir: Option<String>,
    },
    /// Change an agent job's schedule, prompt, timezone, context, skills or model
    Set {
        name: String,
        #[arg(short, long)]
        schedule: Option<String>,
        #[arg(short, long)]
        prompt: Option<String>,
        #[arg(long)]
        tz: Option<String>,
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        skills: Option<String>,
        #[arg(short, long)]
        model: Option<String>,
    },
    /// Pause a job
    Pause { name: String },
    /// Resume a paused job (also approves one the agent created)
    Resume { name: String },
    /// Remove an agent job (its runs stay on record)
    Rm { name: String },
    /// Run a job now
    Run { name: String },
    /// Recent runs and their reports (of one job, or all)
    Runs {
        name: Option<String>,
        /// How many
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: i64,
    },
}

#[derive(Subcommand)]
enum MemoryCmd {
    /// Tidy short-term memory now (what the nightly sleep does)
    Sleep,
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
    effort: Option<String>,
    /// The kernel's record of the turn (harness, engine, totals), sent with its end, plus the
    /// records of child turns it ran (a verifier's).
    record: Value,
    /// Each turn's record, in order (verifier turns included).
    records: Vec<Value>,
    error: Option<String>,
}

/// Add a turn's record to the totals of a chain of turns. A cache break counts if any turn had an
/// unexpected one (`history`, `miss`).
fn add_record(total: &mut Value, r: &Value) {
    if total.is_null() {
        *total = r.clone();
        return;
    }
    for k in ["duration_ms", "model_calls", "tool_calls", "tool_errors", "input_tokens", "output_tokens", "cache_read", "cache_write"] {
        total[k] = json!(total[k].as_i64().unwrap_or(0) + r[k].as_i64().unwrap_or(0));
    }
    total["cost_usd"] = json!(total["cost_usd"].as_f64().unwrap_or(0.0) + r["cost_usd"].as_f64().unwrap_or(0.0));
    let unexpected = |v: &Value| matches!(v.as_str(), Some("history" | "miss"));
    if !unexpected(&total["cache_break"]) && unexpected(&r["cache_break"]) {
        total["cache_break"] = r["cache_break"].clone();
    }
    total["context_tokens"] = r["context_tokens"].clone();
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
                            Some("user") if text_of(&m["content"]) == prompt => started = true,
                            Some("assistant") if started => {
                                let text = assistant_text(m);
                                if !text.is_empty() {
                                    if !turn.text.is_empty() { turn.text.push_str("\n\n"); }
                                    turn.text.push_str(&text);
                                    if show && !streamed { print!("{}", for_stdout(&text)); }
                                    if show { println!(); }
                                }
                                streamed = false;
                                let u = &m["usage"];
                                let output = u["output"].as_i64().unwrap_or(0);
                                turn.input_tokens += usage_total(u) - output;
                                turn.output_tokens += output;
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
                        print!("{}", for_stdout(ev["delta"].as_str().unwrap_or("")));
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
                    // The turn starts with the first `busy` after the prompt was sent, or with the
                    // prompt's echo, whichever comes first (the echo may not match exactly).
                    "busy" => {
                        started = true;
                        turn.effort = ev["effort"].as_str().map(str::to_string);
                    }
                    "end" | "child_end" if started => {
                        if let Some(e) = ev["error"].as_str() { turn.error = Some(e.to_string()); }
                        let r = &ev["turn"];
                        if r.is_object() {
                            // The kernel's totals cover the whole turn, side calls included.
                            turn.records.push(r.clone());
                            add_record(&mut turn.record, r);
                            let t = &turn.record;
                            turn.output_tokens = t["output_tokens"].as_i64().unwrap_or(0);
                            turn.input_tokens = record_total(t) - turn.output_tokens;
                            turn.cost = t["cost_usd"].as_f64().unwrap_or(turn.cost);
                        }
                        // A turn may be followed by another the kernel starts itself (`next`): wait for `idle`.
                        if ev["type"] == "end" && ev["next"] != true {
                            return Ok(turn);
                        }
                    }
                    "idle" if started => return Ok(turn),
                    "questions" if started => {
                        let qs: Vec<String> = ev["questions"].as_array().into_iter().flatten().map(|q| {
                            let opts: Vec<&str> = q["options"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                            format!("{}\n  {}", q["question"].as_str().unwrap_or(""), opts.join(" / "))
                        }).collect();
                        let text = qs.join("\n");
                        if show { println!("\n── Questions\n{}\n", for_stdout(&text)); }
                        if !turn.text.is_empty() { turn.text.push_str("\n\n"); }
                        turn.text.push_str(&format!("Questions:\n{text}"));
                    }
                    "status" if started && show_tools => {
                        eprintln!("{}", dim(&format!("  · {}", ev["text"].as_str().unwrap_or(""))));
                    }
                    "resync" if started && ev["busy"] == false => {
                        turn.error.get_or_insert_with(|| "missed the end of the turn (client fell behind); see `zen sessions show`".into());
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

/// A model's thinking levels for `zen models`, the default in brackets: "  effort: low [high] max".
fn effort_list(model: &Value) -> String {
    let default = model["default_effort"].as_str();
    let levels: Vec<String> = model["efforts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|e| if Some(e) == default { format!("[{e}]") } else { e.to_string() })
        .collect();
    if levels.is_empty() { String::new() } else { format!("  effort: {}", levels.join(" ")) }
}

fn turn_json(session: &str, t: &Turn) -> Value {
    json!({
        "session_id": session,
        "text": t.text,
        "model": t.model,
        "effort": t.effort,
        "turn": t.record,
        "turns": t.records,
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

async fn ask(c: &Client, json_out: bool, prompt: Option<String>, session: Option<String>, new: NewSession, quiet: bool) -> Result<()> {
    let text = read_prompt(prompt)?;
    let id = match session {
        Some(s) => c.resolve(&s).await?,
        None => c.new_session(new.model, new.effort).await?,
    };
    let mut ws = c.connect(&id).await?;
    let turn = run_turn(&mut ws, &text, !json_out, !json_out && !quiet).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&turn_json(&id, &turn))?);
    } else if !quiet {
        let effort = turn.effort.as_deref().map(|e| format!(" · {e}")).unwrap_or_default();
        eprintln!("{}", dim(&format!("  {}{effort} · {} tokens · session {}", turn.model, turn.input_tokens + turn.output_tokens, short(&id))));
    }
    if let Some(e) = turn.error {
        if !json_out {
            eprintln!("error: {}", for_stderr(&e));
        }
        std::process::exit(1);
    }
    Ok(())
}

async fn chat(c: &Client, session: Option<String>, new: NewSession) -> Result<()> {
    let id = match session {
        Some(s) => c.resolve(&s).await?,
        None => c.new_session(new.model, new.effort).await?,
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
            eprintln!("error: {}", for_stderr(&e));
        }
    }
    eprintln!("{}", dim(&format!("session {id}")));
    Ok(())
}

/// Values from `<zen home>/env` (written by the installer), read once.
fn zen_env() -> &'static std::collections::HashMap<String, String> {
    static ENV: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| parse_env(&std::fs::read_to_string(zen_home().join("env")).unwrap_or_default()))
}

/// `KEY=value` lines as a shell would read them: comments and blank lines skipped, an `export `
/// prefix and matching quotes around the value removed.
fn parse_env(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("export ").unwrap_or(l).split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let unquoted = [('"', '"'), ('\'', '\'')].iter().find_map(|(a, b)| v.strip_prefix(*a)?.strip_suffix(*b));
            (k.trim().to_string(), unquoted.unwrap_or(v).to_string())
        })
        .collect()
}

fn zen_path() -> String {
    format!("{}:{}", zen_env().get("PATH").cloned().unwrap_or_default(), std::env::var("PATH").unwrap_or_default())
}

/// "Name <email>" that commits in the zenbot checkout will carry, if git has one configured.
fn git_identity() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let repo = zen_env().get("ZEN_REPO").cloned().unwrap_or(format!("{home}/zenbot"));
    let get = |key: &str| {
        let out = std::process::Command::new("git").args(["-C", &repo, "config", key]).output().ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!v.is_empty()).then_some(v)
    };
    Some(format!("{} <{}>", get("user.name")?, get("user.email")?))
}

/// The engines' state as `scripts/update-engines.sh` last left it (`<zen home>/engines.json`).
fn engines_state() -> Value {
    std::fs::read_to_string(zen_home().join("engines.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

/// The `zen status` line for the engines: "claude 2.1.291, codex 0.160.1 (checked …)",
/// with any engine not left up to date (rolled back, skipped, failed) named with its status.
fn engines_line(state: &Value) -> Option<String> {
    let engines = state["engines"].as_object().filter(|e| !e.is_empty())?;
    let mut names: Vec<&String> = engines.keys().filter(|n| n.as_str() != "pi").collect();
    names.sort_by_key(|n| (["claude", "codex"].iter().position(|k| k == n).unwrap_or(9), n.to_string()));
    let versions: Vec<String> = names.iter().map(|n| format!("{n} {}", engines[*n]["version"].as_str().filter(|v| !v.is_empty()).unwrap_or("?"))).collect();
    let problems: Vec<String> = names
        .iter()
        .filter_map(|n| {
            let s = engines[*n]["status"].as_str().unwrap_or("");
            (!(s == "up to date" || s.starts_with("updated"))).then(|| format!("{n}: {s}"))
        })
        .collect();
    let checked = state["checked"].as_str().map(|c| c.replacen('T', " ", 1).trim_end_matches('Z').to_string() + " UTC").unwrap_or_default();
    let mut line = format!("{}  (checked {checked}", versions.join(", "));
    if !problems.is_empty() {
        line += &format!("; {}", problems.join("; "));
    }
    Some(line + ")")
}

/// Whether a model engine (Claude Code, Codex) is signed in, from `/api/models`. OpenRouter is
/// listed there too but only serves System One and embeddings, so it can't run a turn.
pub(crate) fn engine_signed_in(models: &Value) -> bool {
    models["authenticated"].as_object().is_some_and(|a| a.iter().any(|(n, v)| n != "openrouter" && *v == true))
}

fn signed_in(engine: &str) -> bool {
    let run = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).env("PATH", zen_path()).output().ok();
    match engine {
        "claude" => run("claude", &["auth", "status"]).is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("\"loggedIn\": true")),
        "codex" => run("codex", &["login", "status"]).is_some_and(|o| o.status.success()),
        _ => false,
    }
}

fn run_login(cmd: &str, args: &[&str]) -> Result<()> {
    let mut c = std::process::Command::new(cmd);
    c.args(args).env("PATH", zen_path());
    let status = c.status().with_context(|| format!("running {cmd} (is it installed?)"))?;
    if !status.success() {
        bail!("{cmd} sign-in failed");
    }
    Ok(())
}

fn login(which: Option<&str>) -> Result<()> {
    let targets: Vec<&str> = match which {
        Some(w) => vec![w],
        None => ["claude", "codex"].into_iter().filter(|e| !signed_in(e)).collect(),
    };
    if targets.is_empty() {
        eprintln!("Claude Code and Codex are both signed in.");
        return Ok(());
    }
    for t in targets {
        match t {
            "claude" => {
                eprintln!("\n== Claude (your Claude subscription)\nOpen the link, approve, and paste the code back here if asked.\n");
                run_login("claude", &["auth", "login", "--claudeai"])?;
            }
            "codex" => {
                eprintln!("\n== Codex (your ChatGPT subscription)\nOpen the link and enter the code shown.\n");
                run_login("codex", &["login", "--device-auth"])?;
            }
            other => bail!("unknown engine `{other}`; use claude or codex"),
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
            for_stdout(model.split('/').next_back().unwrap_or(model)),
            s["updated_at"].as_str().unwrap_or("").get(..16).unwrap_or("").replace('T', " "),
            for_stdout(s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(untitled)"))
        );
    }
}

/// Text for stdout: sanitized when it is a terminal (escape sequences in a model's reply or a
/// tool's output must not reach it), as is when piped.
fn for_stdout(s: &str) -> std::borrow::Cow<'_, str> {
    if std::io::stdout().is_terminal() { md::sanitize(s) } else { s.into() }
}

fn for_stderr(s: &str) -> std::borrow::Cow<'_, str> {
    if std::io::stderr().is_terminal() { md::sanitize(s) } else { s.into() }
}

fn print_messages(s: &Value) {
    println!("{}  ({})", for_stdout(s["title"].as_str().unwrap_or("")), for_stdout(s["model"].as_str().unwrap_or("")));
    for m in s["messages"].as_array().into_iter().flatten() {
        match m["role"].as_str() {
            Some("user") => println!("\n› {}", for_stdout(&text_of(&m["content"]))),
            Some("assistant") => {
                for c in m["content"].as_array().into_iter().flatten() {
                    match c["type"].as_str() {
                        Some("text") => println!("\n{}", for_stdout(c["text"].as_str().unwrap_or(""))),
                        Some("toolCall") => println!("{}", dim(&format!("  ▸ {}", tool_summary(c["name"].as_str().unwrap_or(""), &c["arguments"])))),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        let message = format!("{e:#}");
        eprintln!("error: {}", for_stderr(&message));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Some(Cmd::Login { which }) = &cli.cmd {
        return login(which.as_deref());
    }
    let c = Client::new(cli.url, cli.token)?;
    let json = cli.json;
    let out = |v: &Value| emit(true, v, String::new);
    let Some(cmd) = cli.cmd else {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            // Piped use without a command behaves like `zen ask`.
            return ask(&c, json, None, None, NewSession { model: cli.model, effort: cli.effort }, false).await;
        }
        let start = match (cli.cont, cli.resume) {
            (_, Some(id)) if !id.is_empty() => tui::Start::Resume(Some(id)),
            (_, Some(_)) => tui::Start::Resume(None),
            (true, None) => tui::Start::Continue,
            // A model or thinking level asked for means a new session on it; plain `zen` is the board.
            _ if cli.model.is_some() || cli.effort.is_some() => tui::Start::New,
            _ => tui::Start::Board,
        };
        return tui::run(c, start, NewSession { model: cli.model, effort: cli.effort }, cli.inline).await;
    };
    match cmd {
        Cmd::Ask { prompt, session, model, effort, quiet } => ask(&c, json, prompt, session, NewSession { model, effort }, quiet).await?,
        Cmd::Chat { session, model, effort } => {
            let new = NewSession { model, effort };
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                let start = session.map(|s| tui::Start::Resume(Some(s))).unwrap_or(tui::Start::New);
                tui::run(c, start, new, cli.inline).await?
            } else {
                chat(&c, session, new).await?
            }
        }
        Cmd::Login { .. } => unreachable!(),
        Cmd::Models => {
            let m = c.get("/api/models").await?;
            if json {
                out(&m);
            } else {
                for x in m["models"].as_array().into_iter().flatten() {
                    let id = x["id"].as_str().unwrap_or("");
                    let default = if Some(id) == m["default"].as_str() { "  (default)" } else { "" };
                    println!("{}{default}{}", for_stdout(id), dim(&effort_list(x)));
                }
            }
        }
        Cmd::Upgrade { check } => {
            if check {
                let v = c.get("/api/version?refresh=true").await?;
                if json {
                    out(&v);
                } else {
                    println!("{}", for_stdout(&describe_update(&v)));
                    for x in v["commits"].as_array().into_iter().flatten() {
                        println!("  · {}", for_stdout(x.as_str().unwrap_or("")));
                    }
                }
            } else {
                let msg = c.upgrade(|l| eprintln!("{}", dim(&l))).await?;
                emit(json, &json!({ "ok": true, "message": msg }), || msg.clone());
            }
        }
        Cmd::Status => {
            let health = c.health().await?;
            let models = c.get("/api/models").await?;
            let any_signed_in = engine_signed_in(&models);
            let version = c.get("/api/version").await.unwrap_or(Value::Null);
            let git_identity = git_identity();
            let engines = engines_state();
            let memory = c.get("/api/memory").await.unwrap_or(Value::Null);
            let jobs = c.get("/api/jobs").await.unwrap_or(Value::Null);
            let status = json!({
                "memory": memory_summary(&memory),
                "jobs": jobs,
                "engines": engines,
                "git_identity": git_identity,
                "commit": health["commit"],
                "update": version,
                "url": c.url,
                "ok": health["ok"] == true && any_signed_in,
                "workers": health["workers"],
                "kernel": true,
                "database": health["db"],
                "worker": health["mind"],
                "signed_in": models["authenticated"],
                "default_model": models["default"],
                "scorer": models["scorer"],
            });
            if json {
                out(&status);
            } else {
                let mark = |b: bool| if b { "ok" } else { "FAIL" };
                println!("kernel    {}  ({})", mark(true), for_stdout(&c.url));
                println!("database  {}", mark(health["db"] == true));
                for (name, ok) in health["workers"].as_object().into_iter().flatten() {
                    println!("{:<9} {}", format!("worker:{}", for_stdout(name)), mark(*ok == true));
                }
                // OpenRouter is only used for live scoring; it's shown with the scorer below.
                for (name, ok) in models["authenticated"].as_object().into_iter().flatten().filter(|(n, _)| *n != "openrouter") {
                    println!("{:<9} {}", for_stdout(name), if *ok == true { "signed in" } else { "NOT signed in" });
                }
                match engines_line(&engines) {
                    Some(line) => println!("engines   {}", for_stdout(&line)),
                    None => println!("engines   not checked yet (the `engines` job runs scripts/update-engines.sh daily)"),
                }
                println!("model     {}", for_stdout(models["default"].as_str().unwrap_or("")));
                if !memory.is_null() {
                    println!("memory    {}", for_stdout(&memory_line(&memory)));
                }
                if !jobs.is_null() {
                    println!("jobs      {}", for_stdout(&jobs_summary(&jobs)));
                }
                match models["scorer"].as_str() {
                    None => println!("scorer    off (set ZEN_S1_MODEL to score sessions)"),
                    Some(s) if s.starts_with("openrouter/") && models["authenticated"]["openrouter"] != true => {
                        println!("scorer    {}  (NO key: set OPENROUTER_API_KEY)", for_stdout(s))
                    }
                    Some(s) => println!("scorer    {}", for_stdout(s)),
                }
                match &git_identity {
                    Some(id) => println!("git       {}", for_stdout(id)),
                    None => println!("git       no identity: zen's commits get a placeholder author (git config --global user.name/user.email)"),
                }
                let commit = for_stdout(health["commit"].as_str().filter(|c| !c.is_empty()).unwrap_or("unknown"));
                match (version["available"].as_bool(), version["latest"].as_str()) {
                    (Some(true), Some(latest)) => println!(
                        "version   {commit}  (update available: {}, {} new commit{}; run `zen upgrade`)",
                        for_stdout(latest),
                        version["behind"],
                        if version["behind"] == 1 { "" } else { "s" }
                    ),
                    (Some(false), _) => println!("version   {commit}  (up to date)"),
                    _ => println!("version   {commit}"),
                }
            }
            if status["ok"] != true {
                std::process::exit(1);
            }
        }
        Cmd::Memory { cmd: Some(MemoryCmd::Sleep), .. } => {
            let r = c.post("/api/memory/sleep", json!({})).await?;
            if json {
                out(&r);
            } else {
                println!("{}", for_stdout(&sleep_counts(&r)));
                if let Some(note) = r["note"].as_str() {
                    println!("{}", for_stdout(note));
                }
            }
        }
        Cmd::Skills { cmd: None } => {
            let s = c.get("/api/skills").await?;
            if json {
                out(&s);
            } else {
                for x in s["skills"].as_array().into_iter().flatten() {
                    let draft = if x["status"] == "draft" { " (draft)" } else { "" };
                    let last = x["last_load"].as_str().map(|t| format!(", last {}", t.get(..10).unwrap_or(t))).unwrap_or_default();
                    println!(
                        "{}{draft}  {}",
                        for_stdout(x["skill"].as_str().unwrap_or("")),
                        dim(&format!("{} loads{last}; accepted {} of {} judged sessions", x["loads"], x["sessions_accepted"], x["sessions_judged"]))
                    );
                }
                for t in s["tools"].as_array().into_iter().flatten() {
                    let state = if t["approved"] == true { "approved" } else { "sandboxed until approved" };
                    println!("tool made_{}  {}", for_stdout(t["tool"].as_str().unwrap_or("")), dim(state));
                }
            }
        }
        Cmd::Skills { cmd: Some(r) } => {
            let (name, decision) = r.parts();
            let res = c.post("/api/skills/review", json!({ "name": name, "decision": decision })).await?;
            emit(json, &res, || res["result"].as_str().unwrap_or("").to_string());
        }
        Cmd::Policy { cmd: None } => {
            let p = c.get("/api/policy").await?;
            if json {
                out(&p);
            } else {
                println!("policy v{}: {}", p["version"], for_stdout(&serde_json::to_string(&p["policy"]).unwrap_or_default()));
                for s in p["stats"].as_array().into_iter().flatten() {
                    println!(
                        "  {:<10} {:<34} {}",
                        for_stdout(s["kind"].as_str().unwrap_or("?")),
                        for_stdout(s["model"].as_str().unwrap_or("?")),
                        dim(&format!("{} subtasks, {} of {} judged accepted, ${:.3} each", s["subtasks"], s["accepted"], s["judged"], s["mean_cost"].as_f64().unwrap_or(0.0)))
                    );
                }
                for s in p["suggestions"].as_array().into_iter().flatten() {
                    println!("suggests {} -> {} ({})", for_stdout(s["kind"].as_str().unwrap_or("")), for_stdout(s["model"].as_str().unwrap_or("")), for_stdout(s["why"].as_str().unwrap_or("")));
                }
            }
        }
        Cmd::Policy { cmd: Some(PolicyCmd::Undo) } => {
            let r = c.post("/api/policy/undo", json!({})).await?;
            emit(json, &r, || format!("policy v{}", r["version"]));
        }
        Cmd::Policy { cmd: Some(PolicyCmd::Set { kind, model, candidates, explore }) } => {
            let mut p = c.get("/api/policy").await?["policy"].clone();
            if !p["routes"].is_object() {
                p["routes"] = json!({});
            }
            let mut route = json!({ "model": model });
            if let Some(cands) = candidates {
                route["candidates"] = json!(cands.split(',').map(str::trim).filter(|x| !x.is_empty()).collect::<Vec<_>>());
            }
            p["routes"][&kind] = route;
            if let Some(e) = explore {
                p["explore"] = json!(e);
            }
            let r = c.post("/api/policy", json!({ "policy": p, "reason": format!("owner: {kind} -> {model}") })).await?;
            emit(json, &r, || format!("policy v{}", r["version"]));
        }
        Cmd::Tools { cmd } => {
            let (name, decision) = cmd.parts();
            let res = c.post(&format!("/api/tools/{}/review", enc(name.trim_start_matches("made_"))), json!({ "decision": decision })).await?;
            emit(json, &res, || res["result"].as_str().unwrap_or("").to_string());
        }
        Cmd::Jobs { cmd: None } => {
            let r = c.get("/api/jobs").await?;
            if json {
                out(&r);
            } else {
                if r["scheduler"] == false {
                    println!("{}", dim("the scheduler is off in this kernel (ZEN_JOBS=0)"));
                }
                for j in r["jobs"].as_array().into_iter().flatten() {
                    println!("{}", for_stdout(&job_line(j)));
                }
            }
        }
        Cmd::Jobs { cmd: Some(cmd) } => match cmd {
            JobsCmd::Add { name, schedule, prompt, tz, context, skills, model, dir } => {
                let body = json!({ "name": name, "schedule": schedule, "prompt": prompt, "tz": tz, "context": context, "skills": skills, "model": model, "workspace": dir });
                let j = c.post("/api/jobs", body).await?;
                emit(json, &j, || job_line(&j));
            }
            JobsCmd::Set { name, schedule, prompt, tz, context, skills, model } => {
                let body = json!({ "schedule": schedule, "prompt": prompt, "tz": tz, "context": context, "skills": skills, "model": model });
                let j = c.patch(&format!("/api/jobs/{}", enc(&name)), body).await?;
                emit(json, &j, || job_line(&j));
            }
            JobsCmd::Pause { name } => {
                let j = c.patch(&format!("/api/jobs/{}", enc(&name)), json!({ "enabled": false })).await?;
                emit(json, &j, || job_line(&j));
            }
            JobsCmd::Resume { name } => {
                let j = c.patch(&format!("/api/jobs/{}", enc(&name)), json!({ "enabled": true })).await?;
                emit(json, &j, || job_line(&j));
            }
            JobsCmd::Rm { name } => {
                let r = c.delete(&format!("/api/jobs/{}", enc(&name))).await?;
                emit(json, &r, || format!("removed {name}"));
            }
            JobsCmd::Run { name } => {
                let r = c.post(&format!("/api/jobs/{}/run", enc(&name)), json!({})).await?;
                emit(json, &r, || format!("started run {} of {name}; see `zen jobs runs {name}`", r["run"]));
            }
            JobsCmd::Runs { name, limit } => {
                let q = name.as_deref().map(|n| format!("&job={}", enc(n))).unwrap_or_default();
                let r = c.get(&format!("/api/jobs/runs?limit={limit}{q}")).await?;
                if json {
                    out(&r);
                } else {
                    for x in r.as_array().into_iter().flatten() {
                        let when = x["started_at"].as_str().map(|t| t.get(..16).unwrap_or(t).replace('T', " ")).unwrap_or_default();
                        let session = x["session"].as_str().map(|s| format!(", session {}", &s[..8])).unwrap_or_default();
                        println!("{} {}  {}", for_stdout(x["job"].as_str().unwrap_or("")), when, dim(&format!("{} ({}{session})", x["status"].as_str().unwrap_or(""), x["trigger"].as_str().unwrap_or(""))));
                        if let Some(body) = x["output"].as_str().or(x["error"].as_str()) {
                            for line in body.lines().take(12) {
                                println!("  {}", for_stdout(line));
                            }
                        }
                    }
                }
            }
        },
        Cmd::Suggestions => {
            let st = c.get("/api/suggestions").await?;
            if json {
                out(&st);
            } else {
                print_suggestions(&st);
            }
        }
        Cmd::Memory { cmd: None, tier } => {
            let m = c.get(&format!("/api/memory?tier={}", enc(&tier))).await?;
            if json {
                out(&m);
            } else {
                for x in m["memories"].as_array().into_iter().flatten() {
                    let tier = if x["tier"] == "short" { String::new() } else { format!(", {}", x["tier"].as_str().unwrap_or("")) };
                    println!("{:<6} {}  {}", for_stdout(x["id"].as_str().unwrap_or("")), for_stdout(x["text"].as_str().unwrap_or("")), dim(&format!("({}{tier})", x["source"].as_str().unwrap_or(""))));
                }
                println!("{}", dim(&memory_line(&m)));
            }
        }
        Cmd::Sessions(cmd) => match cmd {
            SessionsCmd::Ls { archived } => {
                let list = c.get(&format!("/api/sessions?archived={archived}")).await?;
                if json { out(&list) } else { print_sessions(&list) }
            }
            SessionsCmd::New { model, effort, title } => {
                let s = c.post("/api/sessions", json!({ "model": model, "effort": effort, "title": title })).await?;
                emit(json, &s, || s["id"].as_str().unwrap_or("").to_string());
            }
            SessionsCmd::Show { id } => {
                let (_, s) = session_op(&c, &id, reqwest::Method::GET, None).await?;
                if json { out(&s) } else { print_messages(&s) }
            }
            SessionsCmd::Archive { id } => {
                let (id, s) = session_op(&c, &id, reqwest::Method::PATCH, Some(json!({ "archived": true }))).await?;
                emit(json, &s, || format!("archived {}", short(&id)));
            }
            SessionsCmd::Restore { id } => {
                let (id, s) = session_op(&c, &id, reqwest::Method::PATCH, Some(json!({ "archived": false }))).await?;
                emit(json, &s, || format!("restored {}", short(&id)));
            }
            SessionsCmd::Decide { id, decision, note } => {
                let id = c.resolve(&id).await?;
                let d = c.post(&format!("/api/sessions/{id}/decision"), json!({ "decision": decision, "note": note })).await?;
                emit(json, &d, || format!("recorded {} for {}", d["decision"].as_str().unwrap_or(""), short(&id)));
            }
            SessionsCmd::Rename { id, title } => {
                let (id, s) = session_op(&c, &id, reqwest::Method::PATCH, Some(json!({ "title": title }))).await?;
                emit(json, &s, || format!("renamed {}", short(&id)));
            }
        },
    }
    Ok(())
}

/// Print `v` as JSON with `--json`, else the human-readable `text`.
fn emit(json: bool, v: &Value, text: impl FnOnce() -> String) {
    if json {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    } else {
        println!("{}", for_stdout(&text()));
    }
}

/// Resolve a session id or unique prefix, then call `/api/sessions/<id>` with `method`.
async fn session_op(c: &Client, id: &str, method: reqwest::Method, body: Option<Value>) -> Result<(String, Value)> {
    let id = c.resolve(id).await?;
    let v = c.call(method, &format!("/api/sessions/{id}"), body).await?;
    Ok((id, v))
}

/// What a sleep did, in one line.
fn sleep_counts(r: &Value) -> String {
    if let Some(e) = r["error"].as_str() {
        return format!("failed: {e}");
    }
    let n = |k: &str| r[k].as_i64().unwrap_or(0);
    let scorer = r["scorer"].as_str().map(|s| format!(" (judged by {s})")).unwrap_or_else(|| " (by recency: no System One model)".into());
    format!("{} entries: {} kept, {} archived, {} promoted{scorer}", n("entries"), n("kept"), n("dropped"), n("promoted"))
}

/// Short-term memory's size and the last sleep, from `/api/memory`.
fn memory_summary(m: &Value) -> Value {
    let list = m["memories"].as_array().cloned().unwrap_or_default();
    let chars: usize = list.iter().map(|x| x["text"].as_str().map_or(0, str::len) + 10).sum();
    json!({ "entries": list.len(), "chars": chars, "size": m["size"], "last_sleep": m["last_sleep"] })
}

/// The jobs in a few words: how many, the next run, and anything failing or paused.
fn jobs_summary(r: &Value) -> String {
    let all: Vec<&Value> = r["jobs"].as_array().into_iter().flatten().collect();
    if r["scheduler"] == false {
        return format!("{} jobs; the scheduler is OFF in this kernel (ZEN_JOBS=0)", all.len());
    }
    let paused = all.iter().filter(|j| j["enabled"] != true).count();
    let failing: Vec<&str> = all.iter().filter(|j| j["last_status"] == "error").filter_map(|j| j["name"].as_str()).collect();
    let next = all.iter().filter(|j| j["enabled"] == true).filter_map(|j| Some((j["next_run_at"].as_str()?, j["name"].as_str()?))).min();
    let mut s = format!("{} jobs ({paused} paused)", all.len());
    if let Some((t, n)) = next {
        s.push_str(&format!("; next: {n} at {} UTC", t.get(..16).unwrap_or(t).replace('T', " ")));
    }
    if !failing.is_empty() {
        s.push_str(&format!("; last run FAILED: {} (`zen jobs runs`)", failing.join(", ")));
    }
    s
}

/// One job on one line: name, kind, schedule, and when it runs next (or why it's paused).
fn job_line(j: &Value) -> String {
    let t = |k: &str| j[k].as_str().map(|t| t.get(..16).unwrap_or(t).replace('T', " ")).unwrap_or_default();
    let state = if j["enabled"] == true {
        match j["next_run_at"].as_str() {
            Some(_) => format!("next {} UTC", t("next_run_at")),
            None => "nothing ahead".into(),
        }
    } else {
        format!("paused: {}", j["paused_reason"].as_str().unwrap_or("paused"))
    };
    let last = j["last_status"].as_str().map(|s| format!("; last {s} {} UTC", t("last_run_at"))).unwrap_or_default();
    let when = format!("{} ({})", j["schedule"].as_str().unwrap_or(""), j["tz"].as_str().unwrap_or(""));
    format!("{:<16} {:<6} {:<34} {state}{last}", j["name"].as_str().unwrap_or(""), j["kind"].as_str().unwrap_or(""), when)
}

fn memory_line(m: &Value) -> String {
    let s = memory_summary(m);
    let sleep = &s["last_sleep"];
    let last = if sleep.is_null() {
        "no sleep yet".to_string()
    } else {
        let when = sleep["ended_at"].as_str().or(sleep["started_at"].as_str()).unwrap_or("").get(..16).unwrap_or("").replace('T', " ");
        format!("last sleep {when} UTC: {}", sleep_counts(sleep))
    };
    format!("{} entries, {}/{} characters; {last}", s["entries"], s["chars"], s["size"])
}

/// `zen suggestions`: outcomes per prompt version and model, then the latest suggestions.
fn print_suggestions(st: &Value) {
    let n = |v: &Value| v.as_i64().unwrap_or(0);
    for v in st["by_version"].as_array().into_iter().flatten() {
        let decided = n(&v["accepted"]) + n(&v["edited"]) + n(&v["declined"]);
        let rate = if decided > 0 { format!("{:.0}% taken", 100.0 * (n(&v["accepted"]) + n(&v["edited"])) as f64 / decided as f64) } else { "no outcomes yet".into() };
        println!(
            "{} {}  {} shown: {} accepted, {} edited, {} declined, {} unseen, {} open  ({rate}; {:.1}s, ${:.4})",
            for_stdout(v["prompt_version"].as_str().unwrap_or("")),
            dim(&for_stdout(v["model"].as_str().unwrap_or(""))),
            n(&v["shown"]), n(&v["accepted"]), n(&v["edited"]), n(&v["declined"]), n(&v["unseen"]), n(&v["open"]),
            v["latency_ms"].as_f64().unwrap_or(0.0) / 1000.0,
            v["cost_usd"].as_f64().unwrap_or(0.0),
        );
    }
    if st["by_version"].as_array().is_none_or(|a| a.is_empty()) {
        println!("no suggestions yet");
        return;
    }
    println!();
    for r in st["recent"].as_array().into_iter().flatten().take(10) {
        let outcome = r["outcome"].as_str().unwrap_or("open");
        let fin = r["final"].as_str().map(|f| format!(" → {}", for_stdout(&zen_proto::head(f, 60)))).unwrap_or_default();
        println!("{:<9} {}{}", outcome, for_stdout(&zen_proto::head(r["suggested"].as_str().unwrap_or(""), 60)), dim(&fin));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_openrouter_key_alone_is_not_a_signed_in_engine() {
        assert!(!engine_signed_in(&json!({ "authenticated": { "openrouter": true, "claude": false } })));
        assert!(engine_signed_in(&json!({ "authenticated": { "openrouter": false, "codex": true } })));
        assert!(!engine_signed_in(&json!({})));
    }

    #[test]
    fn memory_line_shows_size_and_the_last_sleep() {
        let m = json!({ "size": 4000, "memories": [{ "text": "abc" }, { "text": "defgh" }],
            "last_sleep": { "ended_at": "2026-10-07T04:01:02Z", "entries": 3, "kept": 2, "dropped": 1, "promoted": 0, "proposed": 0, "scorer": null } });
        assert_eq!(memory_line(&m), "2 entries, 28/4000 characters; last sleep 2026-10-07 04:01 UTC: 3 entries: 2 kept, 1 archived, 0 promoted (by recency: no System One model)");
        assert!(memory_line(&json!({ "size": 4000, "memories": [], "last_sleep": null })).ends_with("no sleep yet"));
    }

    #[test]
    fn the_env_file_is_read_like_a_shell_would() {
        let env = parse_env("# zenbot\nexport PATH=\"/a/bin:/b\"\nZEN_REPO='/home/x/zenbot'\n\nPLAIN = v \nBAD\n");
        assert_eq!(env["PATH"], "/a/bin:/b");
        assert_eq!(env["ZEN_REPO"], "/home/x/zenbot");
        assert_eq!(env["PLAIN"], "v");
        assert_eq!(env.len(), 3);
    }

    /// A fake kernel session stream: reads the prompt, then sends `events`.
    async fn fake_stream(events: Vec<Value>) -> (Client, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            let prompt = ws.next().await.unwrap().unwrap().into_text().unwrap().to_string();
            for e in events {
                ws.send(Message::text(e.to_string())).await.unwrap();
            }
            prompt
        });
        (Client::new(url, Some("t".into())).unwrap(), server)
    }

    #[tokio::test]
    async fn ask_starts_on_busy_even_if_the_echo_differs() {
        let (c, server) = fake_stream(vec![
            json!({ "type": "busy", "busy": true, "effort": "high" }),
            json!({ "type": "message", "message": { "role": "user", "content": "do it" } }),
            json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "text", "text": "done" }], "model": "faux/smoke" } }),
            json!({ "type": "end", "error": null }),
        ])
        .await;
        let mut ws = c.connect("s1").await.unwrap();
        let turn = tokio::time::timeout(std::time::Duration::from_secs(5), run_turn(&mut ws, "  do it  ", false, false)).await.expect("no hang").unwrap();
        assert_eq!((turn.text.as_str(), turn.effort.as_deref()), ("done", Some("high")));
        assert!(server.await.unwrap().contains("  do it  "));
    }

    #[test]
    fn engines_line_lists_versions_and_flags_problems() {
        let state = json!({ "checked": "2026-10-06T14:32:34Z", "engines": {
            "pi": { "version": "1.0.4", "status": "up to date" },
            "codex": { "version": "0.160.1", "status": "updated from 0.159.0" },
            "claude": { "version": "2.1.284", "status": "rolled back from 2.1.291" },
        }});
        assert_eq!(
            engines_line(&state).unwrap(),
            "claude 2.1.284, codex 0.160.1  (checked 2026-10-06 14:32:34 UTC; claude: rolled back from 2.1.291)"
        );
        assert_eq!(engines_line(&Value::Null), None);
        assert_eq!(engines_line(&json!({ "engines": {} })), None);
    }
}
