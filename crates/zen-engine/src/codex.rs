//! Codex engine: runs each turn through `codex app-server` on the owner's ChatGPT plan.
//! Codex's own shell, browser, apps and plugins are switched off; zenbot's tools are given as
//! dynamic tools and executed by the kernel. Threads are ephemeral: zenbot's tape is the history.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{watch, OnceCell};

use crate::turn::{now_ms, prompt_with_history, TurnCtx};

pub fn available() -> bool {
    std::process::Command::new("codex").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
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

pub async fn run_turn(
    ctx: TurnCtx,
    model: &str,
    effort: Option<&str>,
    system_prompt: &str,
    history: &[Value],
    prompt: &str,
    mut abort: watch::Receiver<bool>,
) -> Result<Option<String>> {
    let jail = std::env::temp_dir().join(format!("zen-codex-{}", now_ms()));
    std::fs::create_dir_all(&jail)?;
    let result = async {
        let mut s = AppServer::start(&jail).await?;
        let tools: Vec<Value> = ctx
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "name": t["name"], "description": t["description"], "inputSchema": t["parameters"] }))
            .collect();
        let thread = s
            .call(
                "thread/start",
                json!({
                    "model": model, "cwd": jail, "approvalPolicy": "never", "sandbox": "read-only", "ephemeral": true,
                    "baseInstructions": system_prompt,
                    "developerInstructions": "Use the supplied dynamic tools for all commands and file operations. Your own working directory is an empty, read-only placeholder, not the user's workspace.",
                    "dynamicTools": tools,
                    "config": { "web_search": "disabled", "features": {
                        "shell_tool": false, "unified_exec": false, "shell_snapshot": false, "apps": false, "plugins": false,
                        "browser_use": false, "browser_use_external": false, "computer_use": false, "image_generation": false,
                        "in_app_browser": false, "multi_agent": false, "request_permissions_tool": false, "tool_suggest": false
                    }}
                }),
                None,
            )
            .await?;
        let thread_id = thread["thread"]["id"].as_str().context("codex thread id")?.to_string();
        let mut early = Vec::new();
        let input = json!([{ "type": "text", "text": prompt_with_history(history, prompt) }]);
        s.call("turn/start", json!({ "threadId": thread_id, "input": input, "effort": effort }), Some(&mut early)).await?;

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
                    ctx.rpc.notify("turn.message", json!({ "session_id": ctx.session_id, "message": message })).await;
                }
                "item/agentMessage/delta" => {
                    ctx.rpc.notify("turn.delta", json!({ "session_id": ctx.session_id, "delta": p["delta"] })).await;
                }
                "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                    ctx.rpc.notify("turn.thinking", json!({ "session_id": ctx.session_id, "delta": p["delta"] })).await;
                }
                "item/completed" if p["item"]["type"] == "agentMessage" => {
                    let message = json!({ "role": "assistant", "provider": "codex", "model": model, "stopReason": "stop", "timestamp": now_ms(),
                        "content": [{ "type": "text", "text": p["item"]["text"] }],
                        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "total": 0.0 } } });
                    ctx.rpc.notify("turn.message", json!({ "session_id": ctx.session_id, "message": message })).await;
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
        ctx.rpc
            .notify("turn.usage", json!({ "session_id": ctx.session_id, "engine": "codex", "engine_version": version(),
                "provider": "codex", "model": model,
                "input": usage["input"], "output": usage["output"], "cache_read": usage["cacheRead"], "cost_usd": null }))
            .await;
        Ok(error)
    }
    .await;
    let _ = std::fs::remove_dir_all(&jail);
    result
}
