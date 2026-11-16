//! zen's commands, one table for every client: `/model` in the `zen` terminal app, `!model` in
//! Matrix and `zen model` on the command line are the same command with the same arguments.
//!
//! [`COMMANDS`] lists them all with where each works ([`Scope`]). The kernel and session commands
//! are parsed ([`parse`]) and run ([`run`]) here, so every client does the same thing; a client
//! adds only what is its own (the terminal app's panels and pickers, `zen ask`). The client reads
//! a command before anything reaches the agent and calls the kernel itself with the owner's token.

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::{enc, Client};

/// Where a command works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everywhere: the kernel's state (jobs, skills, memory, …).
    Kernel,
    /// Everywhere, on a session: the open one in a chat, `-s <id>` on the command line.
    Session,
    /// Only in a chat page: its screens and panels (the web app; Matrix leaves them out).
    Screen,
    /// Only on the command line: scripting and setup.
    Line,
}

pub struct Command {
    pub name: &'static str,
    /// Its arguments, as typed after the name.
    pub args: &'static str,
    pub help: &'static str,
    pub scope: Scope,
}

const fn cmd(name: &'static str, args: &'static str, help: &'static str, scope: Scope) -> Command {
    Command { name, args, help, scope }
}

use Scope::*;

/// Every command, in the order help lists them.
pub const COMMANDS: &[Command] = &[
    cmd("new", "[@agent] [title]", "start a new session (with another agent: `new @garry`)", Session),
    cmd("resume", "", "switch to another session", Screen),
    cmd("board", "", "every session, running or idle, with its subagents", Screen),
    cmd("model", "[model]", "this session's model (no argument: list them)", Session),
    cmd("effort", "[level]", "this session's thinking level (no argument: the model's levels)", Session),
    cmd("done", "accept|more|reshape|drop [note]", "your verdict on the work so far", Session),
    cmd("rename", "<title>", "rename this session", Session),
    cmd("archive", "", "archive this session", Session),
    cmd("steer", "<message>", "change direction at the next tool boundary", Session),
    cmd("files", "[dir]", "the side panel's Files view: the folder tree (ctrl+b)", Screen),
    cmd("open", "[path]", "show a file next to the chat", Screen),
    cmd("close", "", "close the side panel", Screen),
    cmd("mouse", "", "mouse wheel scrolling on/off", Screen),
    cmd("agents", "[new <name> [description] | rm <name>]", "your agents and what each may use; `agents new` makes one, `agents rm` removes one (kept in backups)", Kernel),
    cmd("tasks", "", "open tasks from the vault's projects, most pressing first", Kernel),
    cmd("jobs", "[run|resume|pause|rm <name> | runs [name]]", "scheduled jobs and their runs", Kernel),
    cmd("skills", "", "skills and how often they're used", Kernel),
    cmd("memory", "[short|long|archived|all | sleep]", "zenbot's memory; `memory sleep` tidies it now", Kernel),
    cmd("mcp", "[login <server>]", "MCP servers; `mcp login <server>` signs in to one", Kernel),
    cmd("models", "", "the models and their thinking levels", Kernel),
    cmd("suggestions", "", "what became of suggested next prompts", Kernel),
    cmd("status", "", "kernel, workers, sign-ins and version", Kernel),
    cmd("upgrade", "[check]", "update zenbot to the latest main and restart it", Kernel),
    cmd("restart", "", "restart zen on the installed version, back in this session", Screen),
    cmd("help", "", "keys and commands", Screen),
    cmd("exit", "", "quit zen", Screen),
    cmd("ask", "<prompt>", "run one task and print the answer", Line),
    cmd("sessions", "ls|show|restore", "list, show or restore sessions", Line),
    cmd("brain", "<action>", "records: get, list, put, schema, lint, index", Line),
    cmd("s1", "[check|models|replay|compare]", "System One's decisions and files", Line),
    cmd("login", "[claude|codex]", "sign in to the model engines", Line),
    cmd("mcp-pending", "", "MCP forms waiting for an answer", Line),
    cmd("mcp-respond", "<id>", "answer an MCP form", Line),
];

/// The command named `name`, if there is one.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// The commands a chat offers (`prefix` is `/` or `!`), one Markdown line each.
pub fn help(prefix: &str, screen: bool) -> String {
    COMMANDS
        .iter()
        .filter(|c| matches!(c.scope, Kernel | Session) || (screen && c.scope == Screen))
        .map(|c| format!("- `{prefix}{}{}{}`: {}", c.name, if c.args.is_empty() { "" } else { " " }, c.args, c.help))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A parsed kernel or session command.
#[derive(Debug, PartialEq)]
pub enum Cmd {
    New { agent: Option<String>, title: Option<String> },
    Agents,
    AgentNew { name: String, description: Option<String> },
    AgentRm(String),
    Model(Option<String>),
    Effort(Option<String>),
    Done { decision: String, note: Option<String> },
    Rename(String),
    Archive,
    Steer(String),
    Tasks,
    Jobs,
    JobRun(String),
    JobResume(String),
    JobPause(String),
    JobRm(String),
    JobRuns(Option<String>),
    Skills,
    Memory(String),
    Sleep,
    Mcp,
    McpLogin(String),
    Models,
    Suggestions,
    Status,
    Upgrade { check: bool },
}

impl Cmd {
    /// Whether it acts on a session.
    pub fn needs_session(&self) -> bool {
        matches!(self, Cmd::Model(Some(_)) | Cmd::Effort(Some(_)) | Cmd::Done { .. } | Cmd::Rename(_) | Cmd::Archive | Cmd::Steer(_))
    }
}

/// The kernel or session command in `text` (without the client's prefix), or None when its first
/// word isn't one. `Some(Err(usage))` when the word is ours but the rest doesn't fit.
pub fn parse(text: &str) -> Option<Result<Cmd, String>> {
    let text = text.trim();
    let (word, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let word = word.to_ascii_lowercase();
    let rest = rest.trim();
    let c = find(&word).filter(|c| matches!(c.scope, Kernel | Session))?;
    let words: Vec<&str> = rest.split_whitespace().collect();
    let usage = || Err(format!("usage: {} {}", c.name, c.args).trim_end().to_string());
    let one = |w: &[&str]| -> Option<String> { (w.len() == 1).then(|| w[0].to_string()) };
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    Some(match word.as_str() {
        "new" => match rest.strip_prefix('@') {
            Some(r) => {
                let (agent, title) = r.split_once(char::is_whitespace).unwrap_or((r, ""));
                if agent.is_empty() {
                    return Some(usage());
                }
                Ok(Cmd::New { agent: Some(agent.to_ascii_lowercase()), title: opt(title.trim()) })
            }
            None => Ok(Cmd::New { agent: None, title: opt(rest) }),
        },
        "agents" => match words.first().map(|w| w.to_ascii_lowercase()).as_deref() {
            None | Some("list") if words.len() <= 1 => Ok(Cmd::Agents),
            Some("rm") if words.len() == 2 => Ok(Cmd::AgentRm(words[1].to_ascii_lowercase())),
            Some("new") if words.len() >= 2 => {
                let description = words[2..].join(" ");
                Ok(Cmd::AgentNew { name: words[1].to_ascii_lowercase(), description: opt(&description) })
            }
            _ => usage(),
        },
        "model" => match words.len() {
            0 | 1 => Ok(Cmd::Model(opt(rest))),
            _ => usage(),
        },
        "effort" => match words.len() {
            0 | 1 => Ok(Cmd::Effort(opt(rest))),
            _ => usage(),
        },
        "done" => match words.first().map(|w| w.to_ascii_lowercase()) {
            Some(d) if ["accept", "more", "reshape", "drop"].contains(&d.as_str()) => {
                let note = rest.split_once(char::is_whitespace).map(|(_, n)| n.trim().to_string()).filter(|n| !n.is_empty());
                Ok(Cmd::Done { decision: d, note })
            }
            _ => usage(),
        },
        "rename" if !rest.is_empty() => Ok(Cmd::Rename(rest.to_string())),
        "archive" if rest.is_empty() => Ok(Cmd::Archive),
        "steer" if !rest.is_empty() => Ok(Cmd::Steer(rest.to_string())),
        "jobs" => match (words.first().map(|w| w.to_ascii_lowercase()).as_deref(), &words[words.len().min(1)..]) {
            (None | Some("list"), []) => Ok(Cmd::Jobs),
            (Some("run"), w) => one(w).map(Cmd::JobRun).map_or_else(usage, Ok),
            (Some("resume"), w) => one(w).map(Cmd::JobResume).map_or_else(usage, Ok),
            (Some("pause"), w) => one(w).map(Cmd::JobPause).map_or_else(usage, Ok),
            (Some("rm"), w) => one(w).map(Cmd::JobRm).map_or_else(usage, Ok),
            (Some("runs"), []) => Ok(Cmd::JobRuns(None)),
            (Some("runs"), w) => one(w).map(|n| Cmd::JobRuns(Some(n))).map_or_else(usage, Ok),
            _ => usage(),
        },
        "tasks" if rest.is_empty() => Ok(Cmd::Tasks),
        "skills" if rest.is_empty() => Ok(Cmd::Skills),
        "memory" => match words.as_slice() {
            [] => Ok(Cmd::Memory("short".into())),
            ["sleep"] => Ok(Cmd::Sleep),
            [t] if ["short", "long", "archived", "all"].contains(t) => Ok(Cmd::Memory(t.to_string())),
            _ => usage(),
        },
        "mcp" => match words.as_slice() {
            [] => Ok(Cmd::Mcp),
            ["login", server] => Ok(Cmd::McpLogin(server.to_string())),
            _ => usage(),
        },
        "models" if rest.is_empty() => Ok(Cmd::Models),
        "suggestions" if rest.is_empty() => Ok(Cmd::Suggestions),
        "status" if rest.is_empty() => Ok(Cmd::Status),
        "upgrade" => match words.as_slice() {
            [] => Ok(Cmd::Upgrade { check: false }),
            ["check"] => Ok(Cmd::Upgrade { check: true }),
            _ => usage(),
        },
        _ => usage(),
    })
}

/// What a command answered: Markdown for a chat, and the kernel's JSON for scripts.
pub struct Reply {
    pub text: String,
    pub json: Value,
}

fn reply(text: impl Into<String>, json: Value) -> Reply {
    Reply { text: text.into(), json }
}

/// Run `cmd` against the kernel on `session` (a chat's open session, or the command line's `-s`).
/// `McpLogin` only starts the sign-in: the client asks for the address and finishes it
/// ([`login_instructions`], [`login_done`]).
pub async fn run(c: &Client, cmd: &Cmd, session: Option<&str>) -> Result<Reply> {
    let sid = || -> Result<String> {
        match session {
            Some(s) => Ok(s.to_string()),
            None => bail!("this command works on a session: open one, or give -s <id>"),
        }
    };
    Ok(match cmd {
        Cmd::New { agent, title } => {
            let s = c.post("/api/sessions", json!({ "title": title, "agent": agent })).await?;
            let who = s["agent"].as_str().filter(|a| *a != "zenbot").map(|a| format!(" with {a}")).unwrap_or_default();
            reply(format!("New session {}{who}.", short(s["id"].as_str().unwrap_or(""))), s)
        }
        Cmd::Agents => {
            let a = c.get("/api/agents").await?;
            reply(agents_list(&a), a)
        }
        Cmd::AgentRm(name) => {
            let r = c.delete(&format!("/api/agents/{}", enc(name))).await?;
            reply(format!("Removed agent `{name}`; its folder is kept in `{}`.", r["moved_to"].as_str().unwrap_or("")), r)
        }
        Cmd::AgentNew { name, description } => {
            let a = c.post("/api/agents", json!({ "name": name, "description": description })).await?;
            let text = format!(
                "Made agent `{name}` in `{}`: edit its `SOUL.md` (who it is) and `agent.toml` (what it may use), then start a session with `new @{name}`.",
                a["dir"].as_str().unwrap_or("")
            );
            reply(text, a)
        }
        Cmd::Model(None) | Cmd::Models => models(c).await?,
        Cmd::Model(Some(model)) => {
            let s = c.patch(&format!("/api/sessions/{}", enc(&sid()?)), json!({ "model": model })).await?;
            reply(format!("Model: `{}`.", s["model"].as_str().unwrap_or(model)), s)
        }
        Cmd::Effort(None) => {
            let id = sid()?;
            let s = c.get(&format!("/api/sessions/{}?last=0", enc(&id))).await?;
            let m = c.get("/api/models").await?;
            let model = s["model"].as_str().unwrap_or("");
            let info = m["models"].as_array().into_iter().flatten().find(|x| x["id"] == model).cloned().unwrap_or(Value::Null);
            reply(format!("`{model}`: effort {}{}", s["effort"].as_str().unwrap_or("default"), efforts(&info)), s)
        }
        Cmd::Effort(Some(level)) => {
            let s = c.patch(&format!("/api/sessions/{}", enc(&sid()?)), json!({ "effort": level })).await?;
            reply(format!("Thinking level: {}.", s["effort"].as_str().unwrap_or(level)), s)
        }
        Cmd::Done { decision, note } => {
            let id = sid()?;
            let d = c.post(&format!("/api/sessions/{}/decision", enc(&id)), json!({ "decision": decision, "note": note })).await?;
            reply(format!("Recorded `{decision}` for {}.", short(&id)), d)
        }
        Cmd::Rename(title) => {
            let s = c.patch(&format!("/api/sessions/{}", enc(&sid()?)), json!({ "title": title })).await?;
            reply(format!("Renamed: {title}."), s)
        }
        Cmd::Archive => {
            let id = sid()?;
            let s = c.patch(&format!("/api/sessions/{}", enc(&id)), json!({ "archived": true })).await?;
            reply(format!("Archived {}.", short(&id)), s)
        }
        Cmd::Steer(text) => {
            use futures_util::SinkExt;
            let id = sid()?;
            let mut ws = c.connect(&id).await?;
            let prompt = json!({ "type": "prompt", "text": text, "mode": "steer" });
            ws.send(tokio_tungstenite::tungstenite::Message::text(prompt.to_string())).await?;
            let _ = ws.close(None).await;
            reply("Steering at the next tool boundary.", json!({ "ok": true }))
        }
        Cmd::Tasks => {
            let t = c.get("/api/tasks").await?;
            reply(tasks_list(&t), t)
        }
        Cmd::Jobs => {
            let r = c.get("/api/jobs").await?;
            let mut out: Vec<String> = r["jobs"].as_array().into_iter().flatten().map(job_line).collect();
            if out.is_empty() {
                out.push("No jobs.".into());
            }
            if r["scheduler"] == false {
                out.insert(0, "The scheduler is off in this kernel (ZEN_JOBS=0).".into());
            }
            reply(out.join("\n"), r)
        }
        Cmd::JobRun(name) => {
            let r = c.post(&format!("/api/jobs/{}/run", enc(name)), json!({})).await?;
            reply(format!("Started run {} of `{name}`: its report arrives when it ends (`jobs runs {name}`).", r["run"]), r)
        }
        Cmd::JobResume(name) => {
            let j = c.patch(&format!("/api/jobs/{}", enc(name)), json!({ "enabled": true })).await?;
            reply(job_line(&j), j)
        }
        Cmd::JobPause(name) => {
            let j = c.patch(&format!("/api/jobs/{}", enc(name)), json!({ "enabled": false })).await?;
            reply(job_line(&j), j)
        }
        Cmd::JobRm(name) => {
            let r = c.delete(&format!("/api/jobs/{}", enc(name))).await?;
            reply(format!("Removed `{name}`."), r)
        }
        Cmd::JobRuns(name) => {
            let q = name.as_deref().map(|n| format!("&job={}", enc(n))).unwrap_or_default();
            let r = c.get(&format!("/api/jobs/runs?limit=10{q}")).await?;
            let runs = r.as_array().cloned().unwrap_or_default();
            let text = if runs.is_empty() { "No runs yet.".into() } else { runs.iter().map(run_block).collect::<Vec<_>>().join("\n\n") };
            reply(text, r)
        }
        Cmd::Skills => {
            let s = c.get("/api/skills").await?;
            reply(skills_list(&s), s)
        }
        Cmd::Memory(tier) => {
            let m = c.get(&format!("/api/memory?tier={}", enc(tier))).await?;
            let mut out: Vec<String> = m["memories"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| {
                    let tier = if x["tier"] == "short" { String::new() } else { format!(", {}", x["tier"].as_str().unwrap_or("")) };
                    format!("- `{}` {} ({}{tier})", x["id"].as_str().unwrap_or(""), x["text"].as_str().unwrap_or(""), x["source"].as_str().unwrap_or(""))
                })
                .collect();
            if out.is_empty() {
                out.push("No memories.".into());
            }
            out.push(memory_line(&m));
            reply(out.join("\n"), m)
        }
        Cmd::Sleep => {
            let r = c.post("/api/memory/sleep", json!({})).await?;
            let mut text = sleep_counts(&r);
            if let Some(note) = r["note"].as_str() {
                text.push_str(&format!("\n{note}"));
            }
            reply(text, r)
        }
        Cmd::Mcp => {
            let r = c.get("/api/mcp").await?;
            reply(mcp_status(&r), r)
        }
        Cmd::McpLogin(server) => {
            let r = c.post("/api/mcp/login", json!({ "server": server })).await?;
            reply(login_instructions(&r), r)
        }
        Cmd::Suggestions => {
            let s = c.get("/api/suggestions").await?;
            let text = s["by_version"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| format!("- {}: {} accepted, {} edited, {} declined", v["prompt_version"].as_str().unwrap_or("?"), v["accepted"], v["edited"], v["declined"]))
                .collect::<Vec<_>>()
                .join("\n");
            reply(if text.is_empty() { "No suggestions yet.".to_string() } else { text }, s)
        }
        Cmd::Status => {
            let h = c.health().await?;
            let m = c.get("/api/models").await.unwrap_or(Value::Null);
            let mut lines = vec![format!("kernel ok, database {}", if h["db"] == true { "ok" } else { "FAIL" })];
            for (name, ok) in h["workers"].as_object().into_iter().flatten() {
                lines.push(format!("worker {name}: {}", if *ok == true { "ok" } else { "FAIL" }));
            }
            for (name, ok) in m["authenticated"].as_object().into_iter().flatten().filter(|(n, _)| *n != "openrouter") {
                lines.push(format!("{name}: {}", if *ok == true { "signed in" } else { "NOT signed in" }));
            }
            lines.push(format!("version {}", h["commit"].as_str().filter(|c| !c.is_empty()).unwrap_or("unknown")));
            reply(lines.join("\n"), h)
        }
        Cmd::Upgrade { check: true } => {
            let v = c.get("/api/version?refresh=true").await?;
            reply(describe_update(&v), v)
        }
        Cmd::Upgrade { check: false } => {
            let r = c.post("/api/upgrade", json!({})).await?;
            reply("Upgrading: zenbot builds the new version and restarts once no session is working.", r)
        }
    })
}

/// The agents as `GET /api/agents` lists them: what each is for and may use.
pub fn agents_list(a: &Value) -> String {
    a.as_array()
        .into_iter()
        .flatten()
        .map(|x| {
            let name = x["name"].as_str().unwrap_or("?");
            if let Some(e) = x["error"].as_str() {
                return format!("- `{name}`: BROKEN: {e}");
            }
            let tools: Vec<&str> = x["tools"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let mut bits = vec![if x["all_tools"] == true { "all tools".to_string() } else { format!("tools: {}", tools.join(", ")) }];
            match x["mcp"].as_array() {
                Some(m) if m.is_empty() => bits.push("no MCP".into()),
                Some(m) => bits.push(format!("MCP: {}", m.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))),
                None => {}
            }
            if let Some(m) = x["model"].as_str() {
                bits.push(format!("model `{m}`"));
            }
            let what = x["description"].as_str().filter(|d| !d.is_empty()).map(|d| format!(": {d}")).unwrap_or_default();
            format!("- `{name}`{what} ({})", bits.join("; "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The models, each with its thinking levels.
async fn models(c: &Client) -> Result<Reply> {
    let m = c.get("/api/models").await?;
    let lines: Vec<String> = m["models"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|x| {
            let id = x["id"].as_str().unwrap_or("");
            let default = if Some(id) == m["default"].as_str() { " (default)" } else { "" };
            format!("- `{id}`{default}{}", efforts(x))
        })
        .collect();
    Ok(reply(lines.join("\n"), m))
}

/// The first 8 bytes of a session id (all of it if shorter, or if 8 bytes would split a
/// character).
pub fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// A model's thinking levels, for a list.
fn efforts(m: &Value) -> String {
    let levels: Vec<&str> = m["efforts"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if levels.is_empty() {
        String::new()
    } else {
        format!(" ({})", levels.join(", "))
    }
}

/// One line summarizing a version check (`GET /api/version`).
pub fn describe_update(v: &Value) -> String {
    if let Some(e) = v["error"].as_str() {
        return format!("zenbot {} · could not check for updates: {e}", v["running"].as_str().unwrap_or("?"));
    }
    if v["available"] == true {
        format!(
            "zenbot update available: {} → {} ({} new commit{})",
            v["running"].as_str().unwrap_or("?"),
            v["latest"].as_str().unwrap_or("?"),
            v["behind"],
            if v["behind"] == 1 { "" } else { "s" }
        )
    } else {
        format!("zenbot {} is up to date", v["running"].as_str().unwrap_or("?"))
    }
}

/// MCP servers as `GET /api/mcp` lists them: tool counts, sign-in state and problems.
pub fn mcp_status(r: &Value) -> String {
    let mut names: Vec<&String> = r["servers"].as_object().map(|m| m.keys().collect()).unwrap_or_default();
    names.sort();
    let mut out: Vec<String> = names
        .iter()
        .map(|n| {
            let login = r["login"][n.as_str()].as_str().map(|l| format!(" · {l}")).unwrap_or_default();
            format!("{n}: {} tools{login}", r["servers"][n.as_str()])
        })
        .collect();
    if out.is_empty() {
        out.push("No MCP servers (add them to ~/.zenbot/mcp.json).".into());
    }
    out.extend(r["problems"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|p| format!("problem: {p}")));
    out.join("\n")
}

/// What to do with an MCP sign-in the kernel started (`POST /api/mcp/login`): the sign-in
/// server's host first (check it's the service you expect), then the link, then what to paste.
pub fn login_instructions(start: &Value) -> String {
    format!(
        "Sign-in at {host}\n\n{url}\n\nOpen the link in your browser and approve. Your browser then fails to load {redirect}: that's expected.\nCopy that page's whole address and paste it here.",
        host = start["auth_server"].as_str().unwrap_or("?"),
        url = start["authorize_url"].as_str().unwrap_or(""),
        redirect = start["redirect"].as_str().unwrap_or("127.0.0.1"),
    )
}

/// The kernel's answer to a finished sign-in.
pub fn login_done(done: &Value) -> String {
    let server = done["server"].as_str().unwrap_or("");
    if done["renews"] == true {
        format!("Signed in to `{server}`; zenbot renews the sign-in itself.")
    } else {
        format!("Signed in to `{server}`. The server gave no way to renew it: sign in again when it expires (mcp login {server}).")
    }
}

/// A UTC timestamp's date and minutes: "2026-10-13 09:30".
fn minute(v: &Value) -> String {
    v.as_str().map(|t| t.get(..16).unwrap_or(t).replace('T', " ")).unwrap_or_default()
}

/// One job as a list item: name, schedule, and when it runs next or why it's paused.
pub fn job_line(j: &Value) -> String {
    let state = if j["enabled"] == true {
        match j["next_run_at"].as_str() {
            Some(_) => format!("next {} UTC", minute(&j["next_run_at"])),
            None => "nothing ahead".into(),
        }
    } else {
        format!("paused: {}", j["paused_reason"].as_str().unwrap_or("paused"))
    };
    let last = j["last_status"].as_str().map(|s| format!("; last {s} {} UTC", minute(&j["last_run_at"]))).unwrap_or_default();
    // A script job's exact command.
    let command = j["command"].as_str().map(|c| format!("\n  `$ {c}`")).unwrap_or_default();
    format!(
        "- `{}` {} ({}): {state}{last}{command}",
        j["name"].as_str().unwrap_or(""),
        j["schedule"].as_str().unwrap_or(""),
        j["tz"].as_str().unwrap_or("")
    )
}

/// The first `max` characters of `s`.
fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// One run: its header, then the start of its report or error.
fn run_block(x: &Value) -> String {
    let mut s = format!(
        "**{}** {} UTC: {} ({})",
        x["job"].as_str().unwrap_or(""),
        minute(&x["started_at"]),
        x["status"].as_str().unwrap_or(""),
        x["trigger"].as_str().unwrap_or("")
    );
    // A script job's run: the exact command it ran and how it exited.
    if let Some(c) = x["command"].as_str() {
        let exit = x["exit_code"].as_i64().map(|e| format!(" (exit {e})")).unwrap_or_default();
        s.push_str(&format!("\n`$ {}`{exit}", cut(c, 200)));
    }
    if let Some(body) = x["output"].as_str().or(x["error"].as_str()) {
        for line in body.lines().take(12) {
            s.push_str(&format!("\n> {line}"));
        }
    }
    s
}

/// What a sleep did, in one line.
pub fn sleep_counts(r: &Value) -> String {
    if let Some(e) = r["error"].as_str() {
        return format!("failed: {e}");
    }
    let n = |k: &str| r[k].as_i64().unwrap_or(0);
    let scorer = r["scorer"].as_str().map(|s| format!(" (judged by {s})")).unwrap_or_else(|| " (by recency: no System One model)".into());
    format!("{} entries: {} kept, {} archived, {} promoted{scorer}", n("entries"), n("kept"), n("dropped"), n("promoted"))
}

/// Short-term memory's size and the last sleep, from `/api/memory`.
pub fn memory_summary(m: &Value) -> Value {
    let list = m["memories"].as_array().cloned().unwrap_or_default();
    let chars: usize = list.iter().map(|x| x["text"].as_str().map_or(0, str::len) + 10).sum();
    json!({ "entries": list.len(), "chars": chars, "size": m["size"], "last_sleep": m["last_sleep"] })
}

pub fn memory_line(m: &Value) -> String {
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

/// The open tasks from `/api/tasks`, a short block per project: compact enough for a phone.
pub fn tasks_list(t: &Value) -> String {
    let mut out = Vec::new();
    for p in t["projects"].as_array().into_iter().flatten() {
        out.push(format!("**{}**", p["project"].as_str().unwrap_or("")));
        out.extend(p["tasks"].as_array().into_iter().flatten().map(task_line));
    }
    for e in t["problems"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        out.push(format!("could not read {e}"));
    }
    if out.is_empty() {
        return "No open tasks.".into();
    }
    let n = t["count"].as_u64().unwrap_or(0);
    out.insert(0, format!("{n} open task{} · {}", if n == 1 { "" } else { "s" }, t["today"].as_str().unwrap_or("")));
    out.join("\n")
}

/// One task: priority, text, dates (overdue in bold), goals and id.
pub fn task_line(x: &Value) -> String {
    let mark = match x["priority"].as_str() {
        Some("urgent") => "🔺 ",
        Some("important") => "⏫ ",
        Some("medium") => "🔼 ",
        Some("not_urgent") => "🔽 ",
        Some("parking_lot") => "⏬ ",
        _ => "",
    };
    let mut s = format!("- {mark}{}", x["text"].as_str().filter(|t| !t.is_empty()).unwrap_or("(no text)"));
    if let Some(due) = x["due"].as_str() {
        s.push_str(&if x["overdue"] == true { format!(" · **overdue {due}**") } else { format!(" · due {due}") });
    }
    if let Some(on) = x["scheduled"].as_str() {
        s.push_str(&format!(" · ⏳ {on}"));
    }
    let goals: Vec<&str> = x["goals"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !goals.is_empty() {
        s.push_str(&format!(" · → {}", goals.join(", ")));
    }
    if let Some(id) = x["id"].as_str() {
        s.push_str(&format!(" `{id}`"));
    }
    if let Some(state) = x["agent"]["state"].as_str() {
        s.push_str(&format!(" · 🤖 {}", agent_state(state)));
    }
    s
}

/// What the agent a task was handed to is doing, in words.
pub fn agent_state(state: &str) -> &'static str {
    match state {
        "running" => "working",
        "waiting" => "waiting on you",
        _ => "reported",
    }
}

/// Skills with how often each is used.
pub fn skills_list(s: &Value) -> String {
    let out: Vec<String> = s["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|x| format!("- `{}`: {} loads", x["skill"].as_str().unwrap_or(""), x["loads"]))
        .collect();
    if out.is_empty() { "No skills.".into() } else { out.join("\n") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids_never_split_a_character() {
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short("ééééé"), "éééé");
        assert_eq!(short("1234567é"), "1234567é", "byte 8 is inside é: the whole id");
    }

    #[test]
    fn parses_the_commands_and_leaves_anything_else_alone() {
        assert_eq!(parse("jobs"), Some(Ok(Cmd::Jobs)));
        assert_eq!(parse("JOBS list"), Some(Ok(Cmd::Jobs)));
        assert_eq!(parse("jobs run daily-readings"), Some(Ok(Cmd::JobRun("daily-readings".into()))));
        assert_eq!(parse("jobs  resume  daily-readings "), Some(Ok(Cmd::JobResume("daily-readings".into()))));
        assert_eq!(parse("jobs rm x"), Some(Ok(Cmd::JobRm("x".into()))));
        assert_eq!(parse("jobs runs"), Some(Ok(Cmd::JobRuns(None))));
        assert_eq!(parse("model"), Some(Ok(Cmd::Model(None))));
        assert_eq!(parse("model codex/gpt-6-sol"), Some(Ok(Cmd::Model(Some("codex/gpt-6-sol".into())))));
        assert_eq!(parse("done accept nice work"), Some(Ok(Cmd::Done { decision: "accept".into(), note: Some("nice work".into()) })));
        assert_eq!(parse("rename A new title"), Some(Ok(Cmd::Rename("A new title".into()))));
        assert_eq!(parse("steer stop, use the other file"), Some(Ok(Cmd::Steer("stop, use the other file".into()))));
        assert_eq!(parse("memory sleep"), Some(Ok(Cmd::Sleep)));
        assert_eq!(parse("mcp login linear"), Some(Ok(Cmd::McpLogin("linear".into()))));
        assert_eq!(parse("upgrade check"), Some(Ok(Cmd::Upgrade { check: true })));
        assert_eq!(parse("new"), Some(Ok(Cmd::New { agent: None, title: None })));
        assert_eq!(parse("new A title"), Some(Ok(Cmd::New { agent: None, title: Some("A title".into()) })));
        assert_eq!(parse("new @Garry Office hours"), Some(Ok(Cmd::New { agent: Some("garry".into()), title: Some("Office hours".into()) })));
        assert_eq!(parse("new @garry"), Some(Ok(Cmd::New { agent: Some("garry".into()), title: None })));
        assert!(matches!(parse("new @"), Some(Err(_))));
        assert_eq!(parse("agents"), Some(Ok(Cmd::Agents)));
        assert_eq!(parse("agents new garry Runs ideas through YC"), Some(Ok(Cmd::AgentNew { name: "garry".into(), description: Some("Runs ideas through YC".into()) })));
        assert_eq!(parse("agents new garry"), Some(Ok(Cmd::AgentNew { name: "garry".into(), description: None })));
        assert!(matches!(parse("agents new"), Some(Err(_))));
        assert_eq!(parse("agents rm Garry"), Some(Ok(Cmd::AgentRm("garry".into()))));
        assert!(matches!(parse("agents rm"), Some(Err(_))));
        assert_eq!(parse("tasks"), Some(Ok(Cmd::Tasks)));
        for gone in ["queue", "approve 1", "deny", "trust add docs.rs", "policy", "tools"] {
            assert_eq!(parse(gone), None, "{gone} is gone");
        }
        assert_eq!(parse("board"), None, "a screen command is the terminal app's own");
        assert_eq!(parse("hello"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn a_wrong_shape_is_usage_never_a_guess() {
        assert!(matches!(parse("jobs run"), Some(Err(u)) if u.starts_with("usage: jobs")));
        assert!(matches!(parse("jobs run a b"), Some(Err(_))));
        assert!(matches!(parse("done maybe"), Some(Err(_))));
        assert!(matches!(parse("rename"), Some(Err(_))));
        assert!(matches!(parse("archive now"), Some(Err(_))));
        assert!(matches!(parse("memory forever"), Some(Err(_))));
        assert!(matches!(parse("tasks all"), Some(Err(u)) if u == "usage: tasks"));
    }

    #[test]
    fn every_command_is_listed_once() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(help("!", false).contains("- `!jobs [run|resume|pause|rm <name> | runs [name]]`"));
        assert!(!help("!", false).contains("`!board`") && help("/", true).contains("`/board`"));
    }

    #[test]
    fn the_agents_list() {
        let a = json!([
            { "name": "zenbot", "description": "", "all_tools": true, "tools": [], "mcp": null, "model": null },
            { "name": "garry", "description": "YC office hours", "all_tools": false, "tools": ["read", "search"], "mcp": [], "model": "claude/opus" },
            { "name": "broken", "error": "unknown tool(s) rm" },
        ]);
        assert_eq!(
            agents_list(&a),
            "- `zenbot` (all tools)\n- `garry`: YC office hours (tools: read, search; no MCP; model `claude/opus`)\n- `broken`: BROKEN: unknown tool(s) rm"
        );
    }

    #[test]
    fn lines_for_the_chat() {
        let j = json!({ "name": "daily-digest", "schedule": "0 8 * * *", "tz": "UTC", "enabled": false, "paused_reason": "paused by the owner" });
        assert_eq!(job_line(&j), "- `daily-digest` 0 8 * * * (UTC): paused: paused by the owner");
        let on = json!({ "name": "n", "schedule": "every 2h", "tz": "UTC", "enabled": true, "next_run_at": "2026-10-14T09:30:00Z", "last_status": "ok", "last_run_at": "2026-10-13T09:30:05Z" });
        assert_eq!(job_line(&on), "- `n` every 2h (UTC): next 2026-10-14 09:30 UTC; last ok 2026-10-13 09:30 UTC");
        assert_eq!(skills_list(&json!({ "skills": [{ "skill": "work/brief", "loads": 3 }] })), "- `work/brief`: 3 loads");
        let t = json!({ "today": "2026-10-28", "count": 2, "problems": [], "projects": [{ "project": "self", "tasks": [
            { "text": "Pay the bill", "priority": "urgent", "due": "2026-10-27", "overdue": true, "goals": [], "id": "s1" },
            { "text": "Plan", "priority": null, "due": null, "scheduled": "2026-10-30", "overdue": false, "goals": ["A calmer week"], "id": null }] }] });
        assert_eq!(tasks_list(&t), "2 open tasks · 2026-10-28\n**self**\n- 🔺 Pay the bill · **overdue 2026-10-27** `s1`\n- Plan · ⏳ 2026-10-30 · → A calmer week");
        assert_eq!(tasks_list(&json!({ "projects": [], "problems": [], "count": 0 })), "No open tasks.");
        let handed = json!({ "text": "Pay", "id": "s1", "agent": { "session": "x", "state": "waiting" } });
        assert_eq!(task_line(&handed), "- Pay `s1` · 🤖 waiting on you");
        let r = json!({ "job": "n", "started_at": "2026-10-13T09:30:00Z", "status": "ok", "trigger": "manual", "output": "line 1\nline 2" });
        assert_eq!(run_block(&r), "**n** 2026-10-13 09:30 UTC: ok (manual)\n> line 1\n> line 2");
    }
}
