//! Codex engine: runs each turn through `codex app-server` on the owner's ChatGPT plan.
//! Codex's own tools, MCP servers and project instruction files are switched off (`config`);
//! zenbot's tools are given as dynamic tools and executed by the kernel. Switching features off
//! is a list of what is known; what holds the boundary at run time is `foreign_item`: a turn in
//! which Codex starts anything but a message, reasoning, a plan, a compaction or one of zenbot's
//! dynamic tools is stopped at once. Threads persist, like
//! Claude Code's sessions, unless ZEN_CODEX_RESUME=0 (Codex ties its prompt cache to the thread). A
//! new thread gets zenbot's tape as native items (`thread/inject_items`, as qm does), so the model
//! sees its own earlier turns as messages and tool calls; ZEN_CODEX_INJECT=0 (or an app-server that
//! refuses the items) falls back to a transcript.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::watch;

use zen_proto::worker::{Completion, Model, TurnEvent};

use crate::engine::{BoxFuture, Engine};
use crate::turn::{enabled, now_ms, prompt_blocks, seed_blocks, StderrTail, TurnCtx, TurnInput, COMPLETE_LIMIT};

/// Codex features zen turns off in every thread: each gives the model a tool of its own (shell,
/// browser, apps, plugins, hooks, subagents, image tools, …) or acts outside zen's tools.
const DISABLED: &[&str] = &[
    "shell_tool", "unified_exec", "unified_exec_tty", "write_stdin_approval", "shell_snapshot", "apps", "plugins", "remote_plugin",
    "plugin_sharing", "browser_use", "browser_use_external", "browser_use_full_cdp_access", "browser_annotation_api", "computer_use",
    "image_generation", "view_image", "in_app_browser", "in_app_local_automation", "in_app_chat", "in_app_dictation", "in_app_voice",
    "in_app_updates", "multi_agent", "request_permissions_tool", "tool_suggest", "tool_call_mcp_elicitation", "hooks", "goals",
    "code_mode_host", "sleep_tool", "skill_search", "skill_mcp_dependency_install", "worktrees", "workspace_dependencies",
    "realtime_conversation",
];

/// Features on by default that were reviewed and give the model nothing to act with (transport,
/// pacing, compaction, model discovery). `zen-engine codex-features` fails on any feature on by
/// default that is in neither list, so the daily engine job notices a Codex release that adds one.
const REVIEWED: &[&str] = &[
    "api_key_model_discovery", "auth_elicitation", "compaction_image_budget", "content_item_kinds", "daemon_auto_start",
    "enable_request_compression", "fast_mode", "ultrafast_mode", "guardian_approval", "guardian_reuse_parent_compaction",
    "instant_interrupt", "mentions_v2", "system_proxy_fallback", "unbounded_connection_retries",
];

/// Codex's settings for every zen thread: the features in `DISABLED` off, no web search, no MCP
/// servers and no project instruction files from the owner's Codex setup. Every action goes
/// through the kernel's dynamic tools.
fn config() -> Value {
    let features: serde_json::Map<String, Value> = DISABLED.iter().map(|f| (f.to_string(), json!(false))).collect();
    json!({ "web_search": "disabled", "mcp_servers": {}, "project_doc_max_bytes": 0, "agents": { "enabled": false }, "features": features })
}

/// The thread items a zen turn may contain: messages, reasoning, plans, compactions and zenbot's
/// own dynamic tool calls. Anything else (`commandExecution`, `fileChange`, `mcpToolCall`,
/// `webSearch`, `imageView`, subagents, …) is Codex acting by itself.
const ITEMS: &[&str] = &["userMessage", "agentMessage", "reasoning", "plan", "contextCompaction", "dynamicToolCall"];

/// The type of an item Codex started or completed that isn't in `ITEMS`, if this message is one.
fn foreign_item(msg: &Value) -> Option<String> {
    if !matches!(msg["method"].as_str(), Some("item/started" | "item/completed")) {
        return None;
    }
    let kind = msg["params"]["item"]["type"].as_str().unwrap_or("");
    (!ITEMS.contains(&kind)).then(|| if kind.is_empty() { "(an item without a type)".into() } else { kind.to_string() })
}

/// The error a turn ends with when Codex used something of its own.
fn foreign_error(kind: &str) -> String {
    format!("codex used `{kind}`, which zenbot doesn't control; the turn was stopped (has a Codex release turned on a tool of its own? run `zen-engine codex-features`)")
}

/// How long starting the app-server, or listing models, may take before it is given up.
const SETUP_LIMIT: Duration = Duration::from_secs(15);
/// How long a model list is used before Codex is asked again.
const MODELS_TTL: Duration = Duration::from_secs(3600);

/// The `codex` binary on PATH, resolved, and when it last changed: an update changes it.
type Stamp = (std::path::PathBuf, SystemTime);

fn binary_stamp() -> Option<Stamp> {
    let path = std::env::var_os("PATH")?;
    let bin = std::env::split_paths(&path).map(|d| d.join("codex")).find(|p| p.is_file())?;
    let real = std::fs::canonicalize(bin).ok()?;
    let modified = std::fs::metadata(&real).ok()?.modified().ok()?;
    Some((real, modified))
}

/// The Codex engine (`codex/*` models), through `codex app-server`.
pub struct Codex;

impl Engine for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn version(&self) -> Option<String> {
        version()
    }

    fn models(&self) -> BoxFuture<'_, Vec<Model>> {
        Box::pin(models())
    }

    fn run_turn<'a>(&'a self, ctx: TurnCtx, input: &'a TurnInput, abort: watch::Receiver<bool>) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(run_turn(ctx, input, abort))
    }

    fn complete<'a>(&'a self, model: &'a str, system: &'a str, prompt: &'a str) -> BoxFuture<'a, Result<Completion>> {
        Box::pin(complete(model, system, prompt))
    }

    fn failover_target(&self) -> Option<String> {
        Some(std::env::var("ZEN_FAILOVER_CODEX_TO_CLAUDE").unwrap_or_else(|_| "claude/claude-sonnet-5-5".into()))
    }

    /// No API key or access token in the environment (the CLI would prefer it), and `codex login
    /// status` says it is signed in with ChatGPT.
    fn subscription_login(&self) -> bool {
        if std::env::var_os("OPENAI_API_KEY").is_some() || std::env::var_os("CODEX_ACCESS_TOKEN").is_some() {
            return false;
        }
        let Ok(out) = std::process::Command::new("codex").args(["login", "status"]).output() else { return false };
        out.status.success()
            && [out.stdout.as_slice(), out.stderr.as_slice()].iter().any(|bytes| String::from_utf8_lossy(bytes).to_ascii_lowercase().contains("logged in using chatgpt"))
    }
}

/// The installed Codex CLI version ("codex-cli 0.155.1" -> "0.155.1"). Asked again when the
/// binary changes (the daily engine update replaces it without restarting this process).
/// A blocking process run when it does: call it off the runtime's threads.
fn version() -> Option<String> {
    static VERSION: std::sync::Mutex<Option<(Stamp, Option<String>)>> = std::sync::Mutex::new(None);
    let stamp = binary_stamp();
    if let (Some(now), Some((seen, v))) = (&stamp, &*VERSION.lock().unwrap()) {
        if now == seen {
            return v.clone();
        }
    }
    let out = std::process::Command::new("codex").arg("--version").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).split_whitespace().last().map(String::from);
    if let Some(now) = stamp {
        *VERSION.lock().unwrap() = Some((now, v.clone()));
    }
    v
}

/// `zen-engine codex-features`: the Codex features on by default that zen neither turns off nor
/// has reviewed (`DISABLED`, `REVIEWED`), from `codex features list` run with zen's private
/// CODEX_HOME. Prints them and returns 1 if there are any (or the list can't be read), else 0.
/// The daily engine job runs it, so a Codex release that adds a feature fails loudly.
pub fn check_features() -> i32 {
    let home = match crate::turn::engine_dir("codex") {
        Ok(d) => d.join("home"),
        Err(e) => {
            println!("codex features: can't create zen's Codex home: {e}");
            return 1;
        }
    };
    let _ = std::fs::create_dir_all(&home);
    let out = match std::process::Command::new("codex").args(["features", "list"]).env("CODEX_HOME", &home).current_dir(home.parent().unwrap_or(&home)).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(o) => {
            println!("codex features: `codex features list` failed: {}", String::from_utf8_lossy(&o.stderr).trim());
            return 1;
        }
        Err(e) => {
            println!("codex features: can't run `codex features list`: {e}");
            return 1;
        }
    };
    match unknown_features(&out) {
        Some(unknown) if unknown.is_empty() => {
            println!("codex features: every feature on by default is known to zenbot");
            0
        }
        Some(unknown) => {
            println!("codex features on by default that zenbot doesn't know: {} (review each; add it to DISABLED or REVIEWED in crates/zen-engine/src/codex.rs)", unknown.join(", "));
            1
        }
        None => {
            println!("codex features: `codex features list` printed nothing zenbot can read (has its format changed?)");
            1
        }
    }
}

/// Features `codex features list` shows on (`<name> <stage…> true`), not removed, and in neither
/// `DISABLED` nor `REVIEWED`. None when no line could be read.
fn unknown_features(list: &str) -> Option<Vec<String>> {
    let mut read = 0;
    let mut unknown = Vec::new();
    for line in list.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let (Some(name), Some(on)) = (words.first(), words.last()) else { continue };
        if words.len() < 3 || !matches!(*on, "true" | "false") {
            continue;
        }
        read += 1;
        let removed = words[1..words.len() - 1].join(" ") == "removed";
        if *on == "true" && !removed && !DISABLED.contains(name) && !REVIEWED.contains(name) {
            unknown.push(name.to_string());
        }
    }
    (read > 0).then_some(unknown)
}

struct AppServer {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next: AtomicU64,
    stderr: StderrTail,
    /// Lines from the app-server that weren't JSON (reported when a turn's stream makes no sense).
    unknown: usize,
}

impl AppServer {
    async fn start(cwd: &std::path::Path) -> Result<Self> {
        // A private CODEX_HOME prevents the owner's config, plugins, skills and MCP servers from
        // entering zen turns. Only the ChatGPT sign-in is shared; Codex keeps its own session data.
        let isolated_home = cwd.join("home");
        std::fs::create_dir_all(&isolated_home)?;
        let source_home = std::env::var("CODEX_HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".codex")
        });
        let auth = isolated_home.join("auth.json");
        if auth.symlink_metadata().is_err() && source_home.join("auth.json").exists() {
            std::os::unix::fs::symlink(source_home.join("auth.json"), &auth)?;
        }
        let mut child = Command::new("codex")
            .arg("app-server")
            .env("CODEX_HOME", &isolated_home)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("starting `codex app-server` (is Codex installed and signed in?)")?;
        let stdin = child.stdin.take().context("codex stdin")?;
        let lines = BufReader::new(child.stdout.take().context("codex stdout")?).lines();
        let stderr = StderrTail::collect(child.stderr.take());
        let mut s = AppServer { child, stdin, lines, next: AtomicU64::new(1), stderr, unknown: 0 };
        // A hung app-server is killed (dropping it does) instead of holding up the turn.
        let init = json!({ "clientInfo": { "name": "zen", "version": env!("CARGO_PKG_VERSION") }, "capabilities": { "experimentalApi": true } });
        match tokio::time::timeout(SETUP_LIMIT, s.call("initialize", init, None)).await {
            Ok(r) => r?,
            Err(_) => bail!("{}", s.stderr.explain(&format!("codex app-server didn't answer `initialize` within {} s", SETUP_LIMIT.as_secs())).await),
        };
        s.send(json!({ "method": "initialized" })).await?;
        Ok(s)
    }

    async fn send(&mut self, mut msg: Value) -> Result<()> {
        msg["jsonrpc"] = json!("2.0");
        self.stdin.write_all(format!("{msg}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// The next message from the app-server; an error (with what it last wrote to stderr) if it exited.
    async fn next(&mut self) -> Result<Value> {
        while let Some(line) = self.lines.next_line().await? {
            match serde_json::from_str::<Value>(&line) {
                Ok(v) => return Ok(v),
                Err(_) => self.unknown += 1,
            }
        }
        bail!("{}", self.stderr.explain("codex app-server exited").await)
    }

    /// Send a request and wait for its response. Other messages that arrive meanwhile go to `other`;
    /// without `other`, a request from the app-server is refused and fails the call.
    async fn call(&mut self, method: &str, params: Value, mut other: Option<&mut Vec<Value>>) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.send(json!({ "id": id, "method": method, "params": params })).await?;
        loop {
            let msg = self.next().await?;
            if msg["id"].as_u64() == Some(id) && msg.get("method").is_none() {
                if let Some(e) = msg.get("error") {
                    bail!("{}", self.stderr.explain(&format!("codex {method}: {}", e["message"].as_str().unwrap_or("error"))).await);
                }
                return Ok(msg["result"].clone());
            }
            match other.as_deref_mut() {
                Some(o) => o.push(msg),
                None if is_request(&msg) => bail!("{}", self.refuse(&msg).await?),
                None => {}
            }
        }
    }

    /// Answer a request from the app-server that zen doesn't handle with an error, so Codex doesn't
    /// wait for it forever. Returns the error for the turn: Codex's protocol may have changed.
    async fn refuse(&mut self, msg: &Value) -> Result<String> {
        let method = msg["method"].as_str().unwrap_or("");
        self.send(json!({ "id": msg["id"], "error": { "code": -32601, "message": format!("zen doesn't handle `{method}`") } })).await?;
        Ok(format!("codex asked for `{method}`, which zen doesn't handle (has Codex's app-server protocol changed?)"))
    }
}

/// Whether a message from the app-server is a request (it waits for an answer).
fn is_request(msg: &Value) -> bool {
    msg.get("id").is_some_and(|id| !id.is_null()) && msg.get("method").is_some()
}

/// The models Codex listed last: when, for which binary, and the list.
struct Listed {
    at: Instant,
    stamp: Option<Stamp>,
    models: Vec<Model>,
}

/// Not held while Codex is asked: a slow lookup must not hold up other callers.
static MODELS: std::sync::Mutex<Option<Listed>> = std::sync::Mutex::new(None);

/// Models available to this Codex sign-in, asked from Codex itself. A list is kept for an hour, or
/// until the binary changes (an update). A failed lookup (within `SETUP_LIMIT`) is logged and the
/// previous list, if any, is returned; it is tried again next time.
async fn models() -> Vec<Model> {
    let stamp = binary_stamp();
    let previous = MODELS.lock().unwrap().as_ref().map(|l| (l.at.elapsed() < MODELS_TTL && l.stamp == stamp, l.models.clone()));
    if let Some((true, models)) = previous {
        return models;
    }
    match tokio::time::timeout(SETUP_LIMIT, list_models()).await {
        Ok(Ok(models)) if !models.is_empty() => {
            *MODELS.lock().unwrap() = Some(Listed { at: Instant::now(), stamp, models: models.clone() });
            return models;
        }
        Ok(Ok(_)) => eprintln!("[engine] codex: model/list returned no models"),
        Ok(Err(e)) => eprintln!("[engine] codex: listing models failed: {e:#}"),
        Err(_) => eprintln!("[engine] codex: listing models took over {} s; stopped", SETUP_LIMIT.as_secs()),
    }
    previous.map(|(_, models)| models).unwrap_or_default()
}

/// Ask a new app-server for its models (in zen's private Codex folder, like turns). Dropping the
/// future (a timeout) kills the app-server.
async fn list_models() -> Result<Vec<Model>> {
    let mut s = AppServer::start(&crate::turn::engine_dir("codex")?).await?;
    let list = s.call("model/list", json!({}), None).await;
    let _ = s.child.start_kill();
    Ok(list?["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str().or(m["model"].as_str())?;
            let efforts = m["supportedReasoningEfforts"].as_array().into_iter().flatten().filter_map(|e| e["reasoningEffort"].as_str()).map(String::from).collect();
            let mut extra = serde_json::Map::new();
            extra.insert("default".into(), m["isDefault"].clone());
            Some(Model {
                id: format!("codex/{id}"),
                name: format!("Codex {}", m["displayName"].as_str().unwrap_or(id)),
                engine: "codex".into(),
                efforts,
                default_effort: m["defaultReasoningEffort"].as_str().map(String::from),
                context: None,
                extra,
            })
        })
        .collect())
}

fn error_text(v: &Value) -> String {
    let raw = v["message"].as_str().unwrap_or("codex error");
    // Codex wraps provider errors as JSON text; surface the inner message when present.
    serde_json::from_str::<Value>(raw).ok().and_then(|j| j["error"]["message"].as_str().map(String::from)).unwrap_or_else(|| raw.to_string())
}

async fn run_turn(ctx: TurnCtx, input: &TurnInput, mut abort: watch::Receiver<bool>) -> Result<Option<String>> {
    let model = input.model.as_str();
    let effort = input.effort.as_deref();
    let system_prompt = input.system.as_str();
    // Threads are kept (engine sessions, like Claude Code's) unless ZEN_CODEX_RESUME=0: Codex ties its
    // prompt cache to the thread, so a new thread per turn never reuses the cache.
    let sessions = enabled("ZEN_CODEX_RESUME");
    // An empty folder only the owner can read (a shared /tmp path would let others plant files).
    let jail = crate::turn::engine_dir("codex")?;
    // Starting the app-server and the thread can hang; an abort must stop that too.
    let mut setup_abort = abort.clone();
    let setup = async {
        let mut s = AppServer::start(&jail).await?;
        let tools: Vec<Value> = ctx
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "name": t["name"], "description": t["description"], "inputSchema": t["parameters"] }))
            .collect();
        let config = config();
        let developer = "Use the supplied dynamic tools for all commands and file operations. Your own working directory is an empty, read-only placeholder, not the user's workspace.";
        let mut early = Vec::new();
        // Continue the thread the kernel says is in sync with the tape; if Codex no longer has it,
        // start a new one.
        // Why the turn couldn't run the way the kernel asked (resume, native history), if it couldn't.
        let mut fallback: Option<String> = None;
        let resumed = match &input.resume {
            Some(id) if sessions => {
                let r = s
                    .call("thread/resume", json!({ "threadId": id, "model": model, "cwd": jail, "approvalPolicy": "never", "sandbox": "read-only",
                        "baseInstructions": system_prompt, "developerInstructions": developer, "config": config }), Some(&mut early))
                    .await;
                match r.map(|r| r["thread"]["id"].as_str().map(String::from)) {
                    Ok(Some(id)) => Some(id),
                    Ok(None) => {
                        fallback = Some(format!("thread/resume of {id} returned no thread id; starting a new thread"));
                        None
                    }
                    Err(e) => {
                        fallback = Some(format!("thread/resume of {id} failed ({e:#}); starting a new thread"));
                        None
                    }
                }
            }
            _ => None,
        };
        let (thread_id, render) = match resumed {
            Some(id) => (id, "resume"),
            None => {
                let thread = s
                    .call(
                        "thread/start",
                        json!({
                            "model": model, "cwd": jail, "approvalPolicy": "never", "sandbox": "read-only", "ephemeral": !sessions,
                            "baseInstructions": system_prompt, "developerInstructions": developer, "dynamicTools": tools, "config": config
                        }),
                        Some(&mut early),
                    )
                    .await?;
                let id = thread["thread"]["id"].as_str().context("codex thread id")?.to_string();
                let mut injected = false;
                if !input.history.is_empty() && enabled("ZEN_CODEX_INJECT") {
                    match s.call("thread/inject_items", json!({ "threadId": id, "items": history_items(&input.history) }), Some(&mut early)).await {
                        Ok(_) => injected = true,
                        Err(e) => {
                            let why = format!("thread/inject_items failed ({e:#}); sending the history as a transcript");
                            fallback = Some(fallback.map_or(why.clone(), |f| format!("{f}; {why}")));
                        }
                    }
                }
                (id, if input.history.is_empty() || injected { "inject" } else { "transcript" })
            }
        };
        let items = if render == "transcript" {
            seed_blocks(&input.history, &input.prompt, input.context.as_deref())
        } else {
            prompt_blocks(&input.prompt, input.context.as_deref())
        };
        s.call("turn/start", json!({ "threadId": thread_id, "input": items, "effort": effort }), Some(&mut early)).await?;
        Ok::<_, anyhow::Error>((s, thread_id, render, fallback, early))
    };
    let (mut s, thread_id, render, fallback, early) = tokio::select! {
        r = setup => r?,
        _ = setup_abort.changed() => return Ok(Some("interrupted".into())),
    };
    async {

        if let Some(reason) = &fallback {
            eprintln!("[engine] codex: {reason}");
        }
        let mut usage = json!({ "input": 0, "output": 0, "cacheRead": 0 });
        let mut error: Option<String> = None;
        let mut interrupted = false;
        // Messages sent to the tape: a turn that completes without any means the stream has changed.
        let mut sent = 0;
        let mut pending = early.into_iter();
        loop {
            let msg = match pending.next() {
                Some(m) => m,
                None => tokio::select! {
                    next = s.next() => match next {
                        Ok(v) => v,
                        Err(e) => { error.get_or_insert_with(|| e.to_string()); break; }
                    },
                    // Stop, but still report what the turn used (its tokens count toward cost).
                    _ = abort.changed() => { interrupted = true; break; }
                },
            };
            // Codex acting by itself (its own shell, files, web, …): stop it before it goes on.
            if let Some(kind) = foreign_item(&msg) {
                let _ = s.child.start_kill();
                error = Some(foreign_error(&kind));
                break;
            }
            let p = &msg["params"];
            match msg["method"].as_str().unwrap_or("") {
                "item/tool/call" => {
                    let call_id = p["callId"].as_str().unwrap_or("").to_string();
                    let result = ctx.call_tool(p["tool"].as_str().unwrap_or(""), p["arguments"].clone(), Some(call_id)).await;
                    let id = msg["id"].clone();
                    s.send(json!({ "id": id, "result": { "contentItems": dynamic_content(&result), "success": !result.is_error } })).await?;
                }
                "item/started" if p["item"]["type"] == "dynamicToolCall" => {
                    let it = &p["item"];
                    let message = json!({ "role": "assistant", "provider": "codex", "model": model, "stopReason": "toolUse", "timestamp": now_ms(),
                        "content": [{ "type": "toolCall", "id": it["id"], "name": it["tool"], "arguments": it["arguments"] }],
                        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "total": 0.0 } } });
                    ctx.notify(TurnEvent::Message { message }).await;
                    sent += 1;
                }
                "item/agentMessage/delta" => {
                    ctx.notify(TurnEvent::Delta { delta: p["delta"].as_str().unwrap_or("").into() }).await;
                }
                "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                    ctx.notify(TurnEvent::Thinking { delta: p["delta"].as_str().unwrap_or("").into() }).await;
                }
                "item/completed" if p["item"]["type"] == "agentMessage" => {
                    let message = json!({ "role": "assistant", "provider": "codex", "model": model, "stopReason": "stop", "timestamp": now_ms(),
                        "content": [{ "type": "text", "text": p["item"]["text"] }],
                        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "total": 0.0 } } });
                    ctx.notify(TurnEvent::Message { message }).await;
                    sent += 1;
                }
                "thread/tokenUsage/updated" => {
                    let l = &p["tokenUsage"]["last"];
                    let add = |k: &str, src: &str, u: &mut Value| u[k] = json!(u[k].as_i64().unwrap_or(0) + l[src].as_i64().unwrap_or(0));
                    add("input", "inputTokens", &mut usage);
                    add("output", "outputTokens", &mut usage);
                    add("cacheRead", "cachedInputTokens", &mut usage);
                }
                "error" => error = Some(error_text(&p["error"])),
                "turn/completed" => {
                    if p["turn"]["status"] == "failed" {
                        error.get_or_insert_with(|| error_text(&p["turn"]["error"]));
                    } else if p["turn"]["status"] != "interrupted" {
                        error = None;
                        if sent == 0 {
                            error = Some(format!("codex stream format changed: the turn completed, but no message was read from it ({} lines not understood)", s.unknown));
                        }
                    }
                    break;
                }
                // A request zen doesn't know would leave Codex waiting until the watchdog: refuse it
                // and end the turn, naming it.
                _ if is_request(&msg) => {
                    error = Some(s.refuse(&msg).await?);
                    break;
                }
                _ => {}
            }
        }
        let _ = s.child.start_kill();
        if s.unknown > 0 {
            eprintln!("[engine] codex: {} app-server lines not understood", s.unknown);
        }
        let engine_version = tokio::task::spawn_blocking(version).await.ok().flatten();
        ctx
            .report_usage(json!({ "engine": "codex", "engine_version": engine_version,
                "provider": "codex", "model": model, "render": render, "fallback_reason": fallback,
                "engine_session": if sessions { json!({ "id": thread_id, "resumable": error.is_none() && !interrupted }) } else { Value::Null },
                "input": usage["input"], "output": usage["output"], "cache_read": usage["cacheRead"], "cost_usd": null }))
            .await;
        if interrupted {
            return Ok(Some("interrupted".into()));
        }
        Ok(error)
    }
    .await
}

/// One completion without tools, in an ephemeral thread (summaries).
/// Given up after `COMPLETE_LIMIT` (the app-server is killed).
async fn complete(model: &str, system: &str, prompt: &str) -> Result<Completion> {
    match tokio::time::timeout(COMPLETE_LIMIT, complete_now(model, system, prompt)).await {
        Ok(r) => r,
        Err(_) => bail!("codex didn't complete within {} s", COMPLETE_LIMIT.as_secs()),
    }
}

async fn complete_now(model: &str, system: &str, prompt: &str) -> Result<Completion> {
    let dir = crate::turn::engine_dir("codex")?;
    let mut s = AppServer::start(&dir).await?;
    let thread = s
        .call("thread/start", json!({ "model": model, "cwd": dir, "approvalPolicy": "never", "sandbox": "read-only", "ephemeral": true,
            "baseInstructions": system, "config": config() }), None)
        .await?;
    let thread_id = thread["thread"]["id"].as_str().context("codex thread id")?.to_string();
    let mut pending = Vec::new();
    s.call("turn/start", json!({ "threadId": thread_id, "input": [{ "type": "text", "text": prompt }] }), Some(&mut pending)).await?;
    let (mut text, mut input, mut output) = (String::new(), 0i64, 0i64);
    let mut pending = pending.into_iter();
    loop {
        let msg = match pending.next() {
            Some(m) => m,
            None => s.next().await?,
        };
        if let Some(kind) = foreign_item(&msg) {
            let _ = s.child.start_kill();
            bail!("{}", foreign_error(&kind));
        }
        let p = &msg["params"];
        match msg["method"].as_str().unwrap_or("") {
            "item/completed" if p["item"]["type"] == "agentMessage" => text.push_str(p["item"]["text"].as_str().unwrap_or("")),
            "thread/tokenUsage/updated" => {
                input += p["tokenUsage"]["last"]["inputTokens"].as_i64().unwrap_or(0);
                output += p["tokenUsage"]["last"]["outputTokens"].as_i64().unwrap_or(0);
            }
            "error" => anyhow::bail!("{}", error_text(&p["error"])),
            "turn/completed" => {
                if p["turn"]["status"] == "failed" {
                    anyhow::bail!("{}", error_text(&p["turn"]["error"]));
                }
                break;
            }
            _ if is_request(&msg) => anyhow::bail!("{}", s.refuse(&msg).await?),
            _ => {}
        }
    }
    let _ = s.child.start_kill();
    Ok(Completion { text, usage: json!({ "input": input, "output": output }), model: model.to_string() })
}

/// The app-server's dynamic tool response supports inline images with subscription sign-in.
fn dynamic_content(result: &zen_proto::worker::ToolResult) -> Vec<Value> {
    crate::turn::result_blocks(result).iter().filter_map(|block| {
        match block["type"].as_str() {
            Some("text") => Some(json!({ "type": "inputText", "text": block["text"] })),
            Some("image") => Some(json!({ "type": "inputImage", "imageUrl": format!("data:{};base64,{}", block["mimeType"].as_str()?, block["data"].as_str()?) })),
            _ => None,
        }
    }).collect()
}

/// The kernel's history as Responses API items for `thread/inject_items`: messages as messages,
/// tool calls and results as function calls and outputs (call ids longer than the API allows are
/// shortened the same way everywhere). Thinking is left out: it can't be replayed to another model.
fn history_items(history: &[Value]) -> Vec<Value> {
    // The last 64 bytes, moved forward to a character boundary (ids are ASCII today; never panic).
    let call_id = |id: &str| if id.len() > 64 { id[id.ceil_char_boundary(id.len() - 64)..].to_string() } else { id.to_string() };
    let text = |t: &str, kind: &str| json!({ "type": kind, "text": t });
    let mut items = Vec::new();
    let kept = crate::turn::replayed_images(history);
    for (i, m) in history.iter().enumerate() {
        match m["role"].as_str() {
            Some("user") => {
                let mut content: Vec<Value> = match &m["content"] {
                    Value::String(s) => vec![text(s, "input_text")],
                    Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).map(|t| text(t, "input_text")).collect(),
                    _ => vec![],
                };
                if let Some(c) = m["context"].as_str() {
                    content.push(text(c, "input_text"));
                }
                items.push(json!({ "type": "message", "role": "user", "content": content }));
            }
            Some("assistant") => {
                for p in m["content"].as_array().into_iter().flatten() {
                    match p["type"].as_str() {
                        Some("text") => items.push(json!({ "type": "message", "role": "assistant",
                            "content": [text(p["text"].as_str().unwrap_or(""), "output_text")] })),
                        Some("toolCall") => items.push(json!({ "type": "function_call", "call_id": call_id(p["id"].as_str().unwrap_or("")),
                            "name": p["name"], "arguments": p["arguments"].to_string() })),
                        _ => {}
                    }
                }
            }
            Some("toolResult") => {
                let out: String = m["content"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n");
                // Only the latest images are sent again (turn::replayed_images).
                let mut images: Vec<_> = m["content"].as_array().into_iter().flatten().enumerate().filter(|(j, b)| b["type"] == "image" && kept.contains(&(i, *j))).filter_map(|(_, b)| Some(json!({ "type": "input_image", "image_url": format!("data:{};base64,{}", b["mimeType"].as_str()?, b["data"].as_str()?) }))).collect();
                let output = if images.is_empty() { Value::String(out) } else {
                    images.insert(0, text(&out, "input_text"));
                    Value::Array(images)
                };
                items.push(json!({ "type": "function_call_output", "call_id": call_id(m["toolCallId"].as_str().unwrap_or("")), "output": output }));
            }
            _ => {}
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_images_and_replay_use_supported_content_items() {
        let image = json!({"type":"image","mimeType":"image/png","data":"AAAA"});
        let result = zen_proto::worker::ToolResult {
            content: "[source 4] image".into(),
            content_blocks: vec![image.clone()],
            is_error: false,
        };
        let live = dynamic_content(&result);
        assert_eq!(live[0]["text"], "[source 4] image");
        assert_eq!(
            live[1],
            json!({"type":"inputImage","imageUrl":"data:image/png;base64,AAAA"})
        );
        let replay = history_items(&[
            json!({"role":"toolResult","toolCallId":"c1","content":[{"type":"text","text":"[source 4] image"},image]}),
        ]);
        assert_eq!(replay[0]["type"], "function_call_output");
        assert_eq!(
            replay[0]["output"][1],
            json!({"type":"input_image","image_url":"data:image/png;base64,AAAA"})
        );
    }

    #[test]
    fn only_messages_and_zen_tools_may_run() {
        let item = |method: &str, kind: &str| json!({ "method": method, "params": { "item": { "type": kind, "id": "i1" } } });
        for kind in ["agentMessage", "reasoning", "userMessage", "dynamicToolCall", "plan", "contextCompaction"] {
            assert_eq!(foreign_item(&item("item/started", kind)), None, "{kind}");
            assert_eq!(foreign_item(&item("item/completed", kind)), None, "{kind}");
        }
        for kind in ["commandExecution", "fileChange", "mcpToolCall", "webSearch", "imageView", "collabAgentToolCall", "somethingNew"] {
            assert_eq!(foreign_item(&item("item/started", kind)).as_deref(), Some(kind));
        }
        assert!(foreign_item(&json!({ "method": "item/started", "params": { "item": {} } })).is_some(), "an item without a type");
        assert_eq!(foreign_item(&json!({ "method": "item/agentMessage/delta", "params": {} })), None);
    }

    #[test]
    fn features_on_by_default_must_be_known() {
        let list = "apps                 stable             true\n\
                    fast_mode            stable             true\n\
                    steer                removed            true\n\
                    new_magic_tool       under development  true\n\
                    chronicle            under development  false\n";
        assert_eq!(unknown_features(list), Some(vec!["new_magic_tool".to_string()]));
        assert_eq!(unknown_features("apps stable false\n"), Some(vec![]));
        assert_eq!(unknown_features("Usage: codex features …"), None);
        assert_eq!(config()["agents"]["enabled"], false, "native agents must stay disabled");
        assert!(foreign_item(&json!({"method":"item/started","params":{"item":{"type":"subAgentActivity"}}})).is_some());
        let disabled = config()["features"].as_object().unwrap().len();
        assert_eq!(disabled, DISABLED.len());
        assert!(DISABLED.iter().all(|f| !REVIEWED.contains(f)));
    }

    #[test]
    fn requests_are_told_from_notifications_and_responses() {
        assert!(is_request(&json!({ "id": 7, "method": "item/tool/call", "params": {} })));
        assert!(!is_request(&json!({ "method": "turn/completed", "params": {} })));
        assert!(!is_request(&json!({ "id": 7, "result": {} })));
    }

    /// Each turn's items start with the previous turn's (the prefix the provider can cache).
    #[test]
    fn history_items_are_native_and_append_only() {
        let mut history = vec![
            json!({ "role": "user", "content": "<summary/>", "summary": true, "seq": 9 }),
            json!({ "role": "user", "content": "fix it", "context": "<turn_context/>", "seq": 10 }),
            json!({ "role": "assistant", "content": [{ "type": "thinking", "thinking": "hm" }, { "type": "toolCall", "id": "toolu_1", "name": "bash", "arguments": { "command": "ls" } }], "seq": 11 }),
            json!({ "role": "toolResult", "toolCallId": "toolu_1", "toolName": "bash", "content": [{ "type": "text", "text": "a.rs" }], "seq": 12 }),
        ];
        let first = history_items(&history);
        assert_eq!(first.len(), 4, "thinking is dropped");
        assert_eq!(first[1]["content"][1]["text"], "<turn_context/>");
        assert_eq!(first[2]["type"], "function_call");
        assert_eq!(first[2]["arguments"], r#"{"command":"ls"}"#);
        assert_eq!(first[3]["call_id"], "toolu_1");
        history.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": "done" }], "seq": 13 }));
        let second = history_items(&history);
        assert_eq!(&second[..first.len()], &first[..]);
    }
}
