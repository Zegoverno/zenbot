//! Claude engine: runs each turn with the official `claude` CLI on the owner's Claude plan.
//! Built-in tools are off; the only tools are zenbot's (via the MCP bridge), zenbot's system
//! prompt replaces Claude Code's, and no session is kept on disk: zenbot's tape is the history.

use std::collections::HashSet;
use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

use crate::turn::{now_ms, prompt_with_history, TurnCtx};

const PREFIX: &str = "mcp__zen__";

/// Full model ids, never the CLI's aliases (`opus`, …): an alias moves to a new model when
/// Claude Code is updated, so the same session could silently run on a different model.
///
/// Effort levels are Claude Code's `--effort` values. The CLI doesn't report the level it uses when
/// none is given, so zenbot always passes one: `default_effort` unless the session picks another.
pub fn models() -> Vec<Value> {
    let model = |id: &str, name: &str| {
        json!({ "id": format!("claude/{id}"), "name": name, "engine": "claude-code",
                "efforts": EFFORTS, "default_effort": "medium" })
    };
    vec![
        model("claude-opus-5-5", "Claude Opus 5.5"),
        model("claude-sonnet-5-5", "Claude Sonnet 5.5"),
        model("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
    ]
}

const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

pub fn available() -> bool {
    std::process::Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
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
    let socket = format!("{}/zen-engine-{}-{}.sock", std::env::temp_dir().display(), std::process::id(), now_ms());
    let server = ctx.serve_socket(&socket).context("opening tool socket")?;
    let jail = std::env::temp_dir().join(format!("zen-claude-{}", now_ms()));
    std::fs::create_dir_all(&jail)?;
    let exe = std::env::current_exe()?.display().to_string();
    let mcp = json!({ "mcpServers": { "zen": { "command": exe, "args": ["mcp-bridge", socket] } } }).to_string();
    let allowed: Vec<String> = ctx.tools.iter().filter_map(|t| t["name"].as_str()).map(|n| format!("{PREFIX}{n}")).collect();

    let mut cmd = Command::new("claude");
    if let Some(e) = effort {
        cmd.args(["--effort", e]);
    }
    let mut child = cmd
        .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
        .args(["--tools", "", "--strict-mcp-config", "--mcp-config", &mcp, "--setting-sources", ""])
        .args(["--allowedTools", &allowed.join(",")])
        .args(["--permission-mode", "bypassPermissions", "--no-session-persistence"])
        .args(["--system-prompt", system_prompt, "--model", model])
        .current_dir(&jail)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting the claude CLI (is Claude Code installed and signed in?)")?;

    let mut stdin = child.stdin.take().context("claude stdin")?;
    let user = json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": prompt_with_history(history, prompt) }] } });
    stdin.write_all(format!("{user}\n").as_bytes()).await?;
    stdin.flush().await?;
    let mut stdin = Some(stdin);

    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut s) = stderr {
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut s, &mut buf).await;
        }
        buf
    });

    let mut lines = BufReader::new(child.stdout.take().context("claude stdout")?).lines();
    let mut seen_messages: HashSet<String> = HashSet::new();
    let mut outcome: Option<Result<Option<String>>> = None;
    let mut cost = 0.0;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
                match ev["type"].as_str() {
                    Some("stream_event") => {
                        let e = &ev["event"];
                        if e["type"] == "content_block_delta" {
                            match e["delta"]["type"].as_str() {
                                Some("text_delta") => ctx.rpc.notify("turn.delta", json!({ "session_id": ctx.session_id, "delta": e["delta"]["text"] })).await,
                                Some("thinking_delta") => ctx.rpc.notify("turn.thinking", json!({ "session_id": ctx.session_id, "delta": e["delta"]["thinking"] })).await,
                                _ => {}
                            }
                        }
                    }
                    Some("assistant") => {
                        let m = &ev["message"];
                        let mut content = Vec::new();
                        for b in m["content"].as_array().into_iter().flatten() {
                            match b["type"].as_str() {
                                Some("text") => content.push(json!({ "type": "text", "text": b["text"] })),
                                Some("thinking") => content.push(json!({ "type": "thinking", "thinking": b["thinking"] })),
                                Some("tool_use") => {
                                    let name = b["name"].as_str().unwrap_or("").trim_start_matches(PREFIX).to_string();
                                    ctx.announce(b["id"].as_str().unwrap_or(""), &name, &b["input"]).await;
                                    content.push(json!({ "type": "toolCall", "id": b["id"], "name": name, "arguments": b["input"] }));
                                }
                                _ => {}
                            }
                        }
                        // Claude Code emits one event per content block; count usage once per model call.
                        let first = seen_messages.insert(m["id"].as_str().unwrap_or("").to_string());
                        let u = &m["usage"];
                        let n = |k: &str| if first { u[k].as_i64().unwrap_or(0) } else { 0 };
                        let has_tool = content.iter().any(|c| c["type"] == "toolCall");
                        let message = json!({
                            "role": "assistant", "content": content, "provider": "claude", "model": m["model"],
                            "usage": { "input": n("input_tokens"), "output": n("output_tokens"),
                                       "cacheRead": n("cache_read_input_tokens"), "cacheWrite": n("cache_creation_input_tokens"),
                                       "totalTokens": 0, "cost": { "total": 0.0 } },
                            "stopReason": if has_tool { "toolUse" } else { "stop" }, "timestamp": now_ms()
                        });
                        ctx.rpc.notify("turn.message", json!({ "session_id": ctx.session_id, "message": message })).await;
                    }
                    Some("result") => {
                        cost = ev["total_cost_usd"].as_f64().unwrap_or(0.0);
                        let err = if ev["is_error"] == true {
                            Some(ev["result"].as_str().or(ev["subtype"].as_str()).unwrap_or("claude reported an error").to_string())
                        } else { None };
                        outcome = Some(Ok(err));
                        stdin = None; // closing stdin lets the CLI exit
                    }
                    _ => {}
                }
            }
            _ = abort.changed() => {
                let _ = child.start_kill();
                outcome = Some(Ok(Some("interrupted".into())));
                break;
            }
        }
    }
    drop(stdin);
    let status = child.wait().await.ok();
    server.abort();
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_dir_all(&jail);
    let stderr = stderr_task.await.unwrap_or_default();
    if cost > 0.0 {
        ctx.rpc.notify("turn.usage", json!({ "session_id": ctx.session_id, "provider": "claude", "model": model, "cost_usd": cost })).await;
    }
    match outcome {
        Some(o) => o,
        None => {
            let detail = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
            Ok(Some(format!("claude exited unexpectedly ({}): {detail}", status.map(|s| s.to_string()).unwrap_or_default())))
        }
    }
}
