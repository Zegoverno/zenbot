//! Codex engine: runs each turn through `codex app-server` on the owner's ChatGPT plan.
//! Codex's own shell, browser, apps and plugins are switched off; zenbot's tools are given as
//! dynamic tools and executed by the kernel. Threads are ephemeral: zenbot's tape is the history,
//! given to each thread as native items (`thread/inject_items`, as qm does), so the model sees its
//! own earlier turns as messages and tool calls, not as a quoted transcript. ZEN_CODEX_INJECT=0
//! (or an app-server that refuses the items) falls back to the transcript.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{watch, OnceCell};

use crate::turn::{enabled, now_ms, prompt_blocks, seed_blocks, TurnCtx, TurnInput};

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
}

impl AppServer {
    async fn start(cwd: &std::path::Path) -> Result<Self> {
        let mut child = Command::new("codex")
            .arg("app-server")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting `codex app-server` (is Codex installed and signed in?)")?;
        let stdin = child.stdin.take().context("codex stdin")?;
        let lines = BufReader::new(child.stdout.take().context("codex stdout")?).lines();
        let mut s = AppServer { child, stdin, lines, next: AtomicU64::new(1) };
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

    /// Send a request and wait for its response. Other messages that arrive meanwhile go to `other`.
    async fn call(&mut self, method: &str, params: Value, mut other: Option<&mut Vec<Value>>) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.send(json!({ "id": id, "method": method, "params": params })).await?;
        while let Some(line) = self.lines.next_line().await? {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg["id"].as_u64() == Some(id) && msg.get("method").is_none() {
                if let Some(e) = msg.get("error") {
                    bail!("codex {method}: {}", e["message"].as_str().unwrap_or("error"));
                }
                return Ok(msg["result"].clone());
            }
            if let Some(o) = other.as_deref_mut() {
                o.push(msg);
            }
        }
        bail!("codex app-server exited")
    }
}

static MODELS: OnceCell<Vec<Value>> = OnceCell::const_new();

/// Models available to this Codex sign-in, asked from Codex itself (cached).
pub async fn models() -> Vec<Value> {
    MODELS
        .get_or_init(|| async {
            let Ok(mut s) = AppServer::start(&std::env::temp_dir()).await else { return vec![] };
            let list = s.call("model/list", json!({}), None).await.unwrap_or(Value::Null);
            let _ = s.child.start_kill();
            list["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| {
                    let id = m["id"].as_str().or(m["model"].as_str())?;
                    let efforts: Vec<&Value> = m["supportedReasoningEfforts"].as_array().into_iter().flatten().map(|e| &e["reasoningEffort"]).collect();
                    Some(json!({ "id": format!("codex/{id}"), "name": format!("Codex {}", m["displayName"].as_str().unwrap_or(id)), "engine": "codex", "default": m["isDefault"],
                                 "efforts": efforts, "default_effort": m["defaultReasoningEffort"] }))
                })
                .collect()
        })
        .await
        .clone()
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
    let jail = std::env::temp_dir().join("zen-codex");
    std::fs::create_dir_all(&jail)?;
    let result = async {
        let mut s = AppServer::start(&jail).await?;
        let tools: Vec<Value> = ctx
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "name": t["name"], "description": t["description"], "inputSchema": t["parameters"] }))
            .collect();
        let config = json!({ "web_search": "disabled", "features": {
            "shell_tool": false, "unified_exec": false, "shell_snapshot": false, "apps": false, "plugins": false,
            "browser_use": false, "browser_use_external": false, "computer_use": false, "image_generation": false,
            "in_app_browser": false, "multi_agent": false, "request_permissions_tool": false, "tool_suggest": false
        }});
        let developer = "Use the supplied dynamic tools for all commands and file operations. Your own working directory is an empty, read-only placeholder, not the user's workspace.";
        let mut early = Vec::new();
        // Continue the thread the kernel says is in sync with the tape; if Codex no longer has it,
        // start a new one.
        let resumed = match &input.resume {
            Some(id) if sessions => s
                .call("thread/resume", json!({ "threadId": id, "model": model, "cwd": jail, "approvalPolicy": "never", "sandbox": "read-only",
                    "baseInstructions": system_prompt, "developerInstructions": developer, "config": config }), Some(&mut early))
                .await
                .ok()
                .and_then(|r| r["thread"]["id"].as_str().map(String::from)),
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
                let injected = !input.history.is_empty()
                    && enabled("ZEN_CODEX_INJECT")
                    && s.call("thread/inject_items", json!({ "threadId": id, "items": history_items(&input.history) }), Some(&mut early)).await.is_ok();
                (id, if input.history.is_empty() || injected { "inject" } else { "transcript" })
            }
        };
        let items = if render == "transcript" {
            seed_blocks(&input.history, &input.prompt, input.context.as_deref())
        } else {
            prompt_blocks(&input.prompt, input.context.as_deref())
        };
        s.call("turn/start", json!({ "threadId": thread_id, "input": items, "effort": effort }), Some(&mut early)).await?;

        let mut usage = json!({ "input": 0, "output": 0, "cacheRead": 0 });
        let mut error: Option<String> = None;
        let mut pending = early.into_iter();
        loop {
            let msg = match pending.next() {
                Some(m) => m,
                None => tokio::select! {
                    line = s.lines.next_line() => match line? {
                        Some(l) => match serde_json::from_str::<Value>(&l) { Ok(v) => v, Err(_) => continue },
                        None => { error.get_or_insert_with(|| "codex app-server exited".into()); break; }
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
                    }
                    break;
                }
                _ => {}
            }
        }
        let _ = s.child.start_kill();
        ctx
            .notify("turn.usage", json!({ "engine": "codex", "engine_version": version(),
                "provider": "codex", "model": model, "render": render,
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
    let dir = std::env::temp_dir().join("zen-codex");
    std::fs::create_dir_all(&dir)?;
    let mut s = AppServer::start(&dir).await?;
    let thread = s
        .call("thread/start", json!({ "model": model, "cwd": dir, "approvalPolicy": "never", "sandbox": "read-only", "ephemeral": true,
            "baseInstructions": system, "config": { "web_search": "disabled", "features": { "shell_tool": false, "unified_exec": false } } }), None)
        .await?;
    let thread_id = thread["thread"]["id"].as_str().context("codex thread id")?.to_string();
    let mut pending = Vec::new();
    s.call("turn/start", json!({ "threadId": thread_id, "input": [{ "type": "text", "text": prompt }] }), Some(&mut pending)).await?;
    let (mut text, mut input, mut output) = (String::new(), 0i64, 0i64);
    let mut pending = pending.into_iter();
    loop {
        let msg = match pending.next() {
            Some(m) => m,
            None => match s.lines.next_line().await? {
                Some(l) => match serde_json::from_str::<Value>(&l) { Ok(v) => v, Err(_) => continue },
                None => anyhow::bail!("codex app-server exited"),
            },
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
