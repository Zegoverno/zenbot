//! Codex engine: runs each turn through `codex app-server` on the owner's ChatGPT plan.
//! Codex's own tools, MCP servers and project instruction files are switched off (`config`);
//! zenbot's tools are given as dynamic tools and executed by the kernel. Threads persist, like
//! Claude Code's sessions, unless ZEN_CODEX_RESUME=0 (Codex ties its prompt cache to the thread). A
//! new thread gets zenbot's tape as native items (`thread/inject_items`, as qm does), so the model
//! sees its own earlier turns as messages and tool calls; ZEN_CODEX_INJECT=0 (or an app-server that
//! refuses the items) falls back to a transcript.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{watch, Mutex};

use crate::turn::{enabled, now_ms, prompt_blocks, seed_blocks, StderrTail, TurnCtx, TurnInput};

/// Codex's settings for every zen thread: no tool of its own (shell, browser, apps, plugins, hooks,
/// subagents, image tools, …), no MCP servers and no project instruction files from the owner's
/// Codex setup. Every action goes through the kernel's dynamic tools. A Codex release that adds a
/// tool feature on by default needs it added here (the daily engine check lists them).
fn config() -> Value {
    json!({ "web_search": "disabled", "mcp_servers": {}, "project_doc_max_bytes": 0, "features": {
        "shell_tool": false, "unified_exec": false, "shell_snapshot": false, "apps": false, "plugins": false, "remote_plugin": false,
        "browser_use": false, "browser_use_external": false, "computer_use": false, "image_generation": false, "view_image": false,
        "in_app_browser": false, "in_app_local_automation": false, "multi_agent": false, "request_permissions_tool": false,
        "tool_suggest": false, "hooks": false, "goals": false, "code_mode_host": false, "sleep_tool": false, "skill_search": false,
        "skill_mcp_dependency_install": false, "worktrees": false, "workspace_dependencies": false, "realtime_conversation": false
    }})
}

pub fn available() -> bool {
    version().is_some()
}

/// The installed Codex CLI version ("codex-cli 0.155.1" -> "0.155.1").
fn version() -> Option<String> {
    let out = std::process::Command::new("codex").arg("--version").output().ok()?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().last().map(String::from)
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
        s.call("initialize", json!({ "clientInfo": { "name": "zen", "version": env!("CARGO_PKG_VERSION") }, "capabilities": { "experimentalApi": true } }), None).await?;
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

/// Models available to this Codex sign-in, asked from Codex itself. Cached once Codex lists some;
/// a failed lookup is logged and tried again next time.
static MODELS: Mutex<Vec<Value>> = Mutex::const_new(Vec::new());

pub async fn models() -> Vec<Value> {
    let mut cached = MODELS.lock().await;
    if cached.is_empty() {
        match list_models().await {
            Ok(list) if !list.is_empty() => *cached = list,
            Ok(_) => eprintln!("[engine] codex: model/list returned no models"),
            Err(e) => eprintln!("[engine] codex: listing models failed: {e:#}"),
        }
    }
    cached.clone()
}

async fn list_models() -> Result<Vec<Value>> {
    let mut s = AppServer::start(&std::env::temp_dir()).await?;
    let list = s.call("model/list", json!({}), None).await;
    let _ = s.child.start_kill();
    Ok(list?["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str().or(m["model"].as_str())?;
            let efforts: Vec<&Value> = m["supportedReasoningEfforts"].as_array().into_iter().flatten().map(|e| &e["reasoningEffort"]).collect();
            Some(json!({ "id": format!("codex/{id}"), "name": format!("Codex {}", m["displayName"].as_str().unwrap_or(id)), "engine": "codex", "default": m["isDefault"],
                         "efforts": efforts, "default_effort": m["defaultReasoningEffort"] }))
        })
        .collect())
}

fn error_text(v: &Value) -> String {
    let raw = v["message"].as_str().unwrap_or("codex error");
    // Codex wraps provider errors as JSON text; surface the inner message when present.
    serde_json::from_str::<Value>(raw).ok().and_then(|j| j["error"]["message"].as_str().map(String::from)).unwrap_or_else(|| raw.to_string())
}

pub async fn run_turn(ctx: TurnCtx, input: &TurnInput, mut abort: watch::Receiver<bool>) -> Result<Option<String>> {
    let model = input.model.as_str();
    let effort = input.effort.as_deref();
    let system_prompt = input.system.as_str();
    // Threads are kept (engine sessions, like Claude Code's) unless ZEN_CODEX_RESUME=0: Codex ties its
    // prompt cache to the thread, so a new thread per turn never reuses the cache.
    let sessions = enabled("ZEN_CODEX_RESUME");
    // An empty folder only the owner can read (a shared /tmp path would let others plant files).
    let jail = crate::turn::engine_dir("codex")?;
    let result = async {
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

        if let Some(reason) = &fallback {
            eprintln!("[engine] codex: {reason}");
        }
        let mut usage = json!({ "input": 0, "output": 0, "cacheRead": 0 });
        let mut error: Option<String> = None;
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
                    _ = abort.changed() => { let _ = s.child.start_kill(); return Ok(Some("interrupted".into())); }
                },
            };
            let p = &msg["params"];
            match msg["method"].as_str().unwrap_or("") {
                "item/tool/call" => {
                    let call_id = p["callId"].as_str().unwrap_or("").to_string();
                    let (content, is_error) = ctx.call_tool(p["tool"].as_str().unwrap_or(""), p["arguments"].clone(), Some(call_id)).await;
                    let id = msg["id"].clone();
                    s.send(json!({ "id": id, "result": { "contentItems": [{ "type": "inputText", "text": content }], "success": !is_error } })).await?;
                }
                "item/started" if p["item"]["type"] == "dynamicToolCall" => {
                    let it = &p["item"];
                    let message = json!({ "role": "assistant", "provider": "codex", "model": model, "stopReason": "toolUse", "timestamp": now_ms(),
                        "content": [{ "type": "toolCall", "id": it["id"], "name": it["tool"], "arguments": it["arguments"] }],
                        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "total": 0.0 } } });
                    ctx.notify("turn.message", json!({ "message": message })).await;
                    sent += 1;
                }
                "item/agentMessage/delta" => {
                    ctx.notify("turn.delta", json!({ "delta": p["delta"] })).await;
                }
                "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                    ctx.notify("turn.thinking", json!({ "delta": p["delta"] })).await;
                }
                "item/completed" if p["item"]["type"] == "agentMessage" => {
                    let message = json!({ "role": "assistant", "provider": "codex", "model": model, "stopReason": "stop", "timestamp": now_ms(),
                        "content": [{ "type": "text", "text": p["item"]["text"] }],
                        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "total": 0.0 } } });
                    ctx.notify("turn.message", json!({ "message": message })).await;
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
        ctx
            .notify("turn.usage", json!({ "engine": "codex", "engine_version": version(),
                "provider": "codex", "model": model, "render": render, "fallback_reason": fallback,
                "engine_session": if sessions { json!({ "id": thread_id, "resumable": error.is_none() }) } else { Value::Null },
                "input": usage["input"], "output": usage["output"], "cache_read": usage["cacheRead"], "cost_usd": null }))
            .await;
        Ok(error)
    }
    .await;
    result
}

/// One completion without tools, in an ephemeral thread (summaries). Returns `{ text, usage, model }`.
pub async fn complete(model: &str, system: &str, prompt: &str) -> Result<Value> {
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
    Ok(json!({ "text": text, "usage": { "input": input, "output": output }, "model": model }))
}

/// The kernel's history as Responses API items for `thread/inject_items`: messages as messages,
/// tool calls and results as function calls and outputs (call ids longer than the API allows are
/// shortened the same way everywhere). Thinking is left out: it can't be replayed to another model.
fn history_items(history: &[Value]) -> Vec<Value> {
    let call_id = |id: &str| if id.len() > 64 { id[id.len() - 64..].to_string() } else { id.to_string() };
    let text = |t: &str, kind: &str| json!({ "type": kind, "text": t });
    let mut items = Vec::new();
    for m in history {
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
                items.push(json!({ "type": "function_call_output", "call_id": call_id(m["toolCallId"].as_str().unwrap_or("")), "output": out }));
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
