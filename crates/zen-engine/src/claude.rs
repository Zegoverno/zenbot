//! Claude engine: runs each turn with the official `claude` CLI on the owner's Claude plan.
//! Built-in tools are off; the only tools are zenbot's (via the MCP bridge), and zenbot's system
//! prompt replaces Claude Code's.
//!
//! Engine sessions (docs/context.md): Claude Code caches earlier turns only within its own session,
//! so each zenbot session keeps a matching Claude Code session, run from one fixed directory. When
//! the kernel says that session is in sync with the tape (`resume`), the turn continues it with only
//! the new prompt; otherwise a new one is seeded with the replayed history. zenbot's tape stays the
//! source of truth: Claude Code's session is a cache, and the turn report says whether it can be
//! resumed. ZEN_CLAUDE_RESUME=0 runs every turn in a fresh, unsaved session instead.

use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

use crate::turn::{enabled, new_uuid, now_ms, prompt_blocks, seed_blocks, TurnCtx, TurnInput};

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

/// Claude Code's configuration directory (where it keeps sessions).
fn config_dir() -> std::path::PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude"))
}

/// The fixed directory engine sessions run in. Claude Code files sessions by directory and puts the
/// directory in every request, so it must not change between turns.
fn session_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".zenbot/engine/claude")
}

/// Whether Claude Code still has a session on disk (it may have been cleaned up).
fn session_exists(id: &str) -> bool {
    let Ok(dirs) = std::fs::read_dir(config_dir().join("projects")) else { return false };
    dirs.flatten().any(|d| d.path().join(format!("{id}.jsonl")).is_file())
}

/// Delete engine sessions not used for 30 days. They are only a cache of zenbot's tape.
pub fn clean_sessions() {
    let name = session_dir().display().to_string().replace(['/', '.'], "-");
    let Ok(files) = std::fs::read_dir(config_dir().join("projects").join(name)) else { return };
    let month = std::time::Duration::from_secs(30 * 24 * 3600);
    for f in files.flatten() {
        let old = f.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > month);
        if old && f.path().extension().is_some_and(|e| e == "jsonl") {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

pub fn available() -> bool {
    std::process::Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

pub async fn run_turn(ctx: TurnCtx, input: &TurnInput, mut abort: watch::Receiver<bool>) -> Result<Option<String>> {
    let model = input.model.as_str();
    let socket = format!("{}/zen-engine-{}-{}.sock", std::env::temp_dir().display(), std::process::id(), now_ms());
    let server = ctx.serve_socket(&socket).context("opening tool socket")?;
    let exe = std::env::current_exe()?.display().to_string();
    let mcp = json!({ "mcpServers": { "zen": { "command": exe, "args": ["mcp-bridge", socket] } } }).to_string();
    let allowed: Vec<String> = ctx.tools.iter().filter_map(|t| t["name"].as_str()).map(|n| format!("{PREFIX}{n}")).collect();

    // How this turn reaches Claude Code: continue its session, start one seeded from the tape, or
    // (sessions off) a throwaway session with the replayed history.
    let sessions = enabled("ZEN_CLAUDE_RESUME");
    let (render, engine_session, content) = match &input.resume {
        Some(id) if sessions && session_exists(id) => ("resume", Some(id.clone()), prompt_blocks(&input.prompt, input.context.as_deref())),
        _ if sessions => ("seed", Some(new_uuid()), seed_blocks(&input.history, &input.prompt, input.context.as_deref())),
        _ => ("transcript", None, seed_blocks(&input.history, &input.prompt, input.context.as_deref())),
    };
    let dir = if sessions { session_dir() } else { std::env::temp_dir().join(format!("zen-claude-{}", now_ms())) };
    std::fs::create_dir_all(&dir)?;

    let mut cmd = Command::new("claude");
    if let Some(e) = &input.effort {
        cmd.args(["--effort", e]);
    }
    match (render, &engine_session) {
        ("resume", Some(id)) => cmd.args(["--resume", id]),
        (_, Some(id)) => cmd.args(["--session-id", id]),
        _ => cmd.arg("--no-session-persistence"),
    };
    let mut child = cmd
        .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
        .args(["--tools", "", "--strict-mcp-config", "--mcp-config", &mcp, "--setting-sources", ""])
        .args(["--allowedTools", &allowed.join(",")])
        .args(["--permission-mode", "bypassPermissions"])
        .args(["--system-prompt", &input.system, "--model", model])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting the claude CLI (is Claude Code installed and signed in?)")?;

    let mut stdin = child.stdin.take().context("claude stdin")?;
    let user = json!({ "type": "user", "message": { "role": "user", "content": content } });
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
    let mut calls = Calls::default();
    let mut outcome: Option<Result<Option<String>>> = None;
    let mut usage = json!({ "engine": "claude-code", "provider": "claude", "model": model, "render": render });

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
                if ev["type"] == "stream_event" && ev["event"]["type"] == "content_block_delta" {
                    let d = &ev["event"]["delta"];
                    match d["type"].as_str() {
                        Some("text_delta") => ctx.notify("turn.delta", json!({ "delta": d["text"] })).await,
                        Some("thinking_delta") => ctx.notify("turn.thinking", json!({ "delta": d["thinking"] })).await,
                        _ => {}
                    }
                    continue;
                }
                for (id, name, args) in calls.tool_uses(&ev) {
                    ctx.announce(&id, &name, &args).await;
                }
                if let Some(message) = calls.feed(&ev) {
                    ctx.notify("turn.message", json!({ "message": message })).await;
                }
                match ev["type"].as_str() {
                    Some("system") if ev["subtype"] == "init" => usage["engine_version"] = ev["claude_code_version"].clone(),
                    Some("result") => {
                        turn_usage(&ev, &mut usage);
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
    // A model call cut off (abort, crash) still goes on the tape with what it had produced.
    if let Some(message) = calls.flush("aborted") {
        ctx.notify("turn.message", json!({ "message": message })).await;
    }
    drop(stdin);
    let status = child.wait().await.ok();
    server.abort();
    let _ = std::fs::remove_file(&socket);
    if !sessions {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let stderr = stderr_task.await.unwrap_or_default();
    // Only a turn that finished cleanly leaves the engine session in sync with the tape.
    let clean = matches!(outcome, Some(Ok(None)));
    if let Some(id) = engine_session {
        usage["engine_session"] = json!({ "id": id, "resumable": clean });
    }
    ctx.notify("turn.usage", usage).await;
    match outcome {
        Some(o) => o,
        None => {
            let detail = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
            Ok(Some(format!("claude exited unexpectedly ({}): {detail}", status.map(|s| s.to_string()).unwrap_or_default())))
        }
    }
}

/// One completion without tools, in a throwaway session (summaries). Returns `{ text, usage, model }`.
pub async fn complete(model: &str, system: &str, prompt: &str) -> Result<Value> {
    // A fixed, empty directory: Claude Code tells the model its working directory.
    let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".zenbot/engine/complete");
    std::fs::create_dir_all(&dir)?;
    let mut child = Command::new("claude")
        .args(["-p", "--output-format", "json", "--tools", "", "--setting-sources", "", "--no-session-persistence"])
        .args(["--strict-mcp-config", "--system-prompt", system, "--model", model])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting the claude CLI")?;
    let mut stdin = child.stdin.take().context("claude stdin")?;
    stdin.write_all(prompt.as_bytes()).await?;
    drop(stdin);
    let out = child.wait_with_output().await?;
    let res: Value = serde_json::from_slice(&out.stdout)
        .with_context(|| format!("claude gave no result: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("")))?;
    if res["is_error"] == true {
        anyhow::bail!("{}", res["result"].as_str().unwrap_or("claude reported an error"));
    }
    let mut usage = json!({});
    turn_usage(&res, &mut usage);
    Ok(json!({ "text": res["result"], "usage": usage, "model": model }))
}

/// Turn totals from the CLI's `result` event. `modelUsage` covers every model call the CLI made,
/// including the small side calls that never appear in the stream.
fn turn_usage(result: &Value, usage: &mut Value) {
    let sum = |k: &str| -> i64 { result["modelUsage"].as_object().into_iter().flatten().map(|(_, m)| m[k].as_i64().unwrap_or(0)).sum() };
    usage["input"] = json!(sum("inputTokens"));
    usage["output"] = json!(sum("outputTokens"));
    usage["cache_read"] = json!(sum("cacheReadInputTokens"));
    usage["cache_write"] = json!(sum("cacheCreationInputTokens"));
    usage["cost_usd"] = result["total_cost_usd"].clone();
    usage["models"] = result["modelUsage"].clone();
}

/// Turns Claude Code's stream into one message per model call. The CLI sends each content block
/// of a call as its own `assistant` event, with the usage known when the call started; the final
/// usage and stop reason come in `message_delta`, and `message_stop` ends the call.
#[derive(Default)]
struct Calls {
    current: Option<Call>,
}

struct Call {
    id: String,
    model: Value,
    content: Vec<Value>,
    usage: Value,
    stop_reason: Option<String>,
    started: std::time::Instant,
}

impl Call {
    fn start(m: &Value) -> Call {
        Call {
            id: m["id"].as_str().unwrap_or("").to_string(),
            model: m["model"].clone(),
            content: Vec::new(),
            usage: m["usage"].clone(),
            stop_reason: None,
            started: std::time::Instant::now(),
        }
    }
}

impl Calls {
    /// Tool calls in an `assistant` event, to announce before the CLI asks for them.
    fn tool_uses(&self, ev: &Value) -> Vec<(String, String, Value)> {
        if ev["type"] != "assistant" {
            return Vec::new();
        }
        let blocks = ev["message"]["content"].as_array().into_iter().flatten();
        blocks
            .filter(|b| b["type"] == "tool_use")
            .map(|b| (b["id"].as_str().unwrap_or("").to_string(), tool_name(b), b["input"].clone()))
            .collect()
    }

    /// Take in one event; returns a finished message when a model call ends.
    fn feed(&mut self, ev: &Value) -> Option<Value> {
        match ev["type"].as_str() {
            Some("stream_event") => {
                let e = &ev["event"];
                match e["type"].as_str() {
                    Some("message_start") => {
                        let done = self.flush("aborted");
                        self.current = Some(Call::start(&e["message"]));
                        done
                    }
                    Some("message_delta") => {
                        if let Some(c) = &mut self.current {
                            if let Some(u) = e["usage"].as_object() {
                                for (k, v) in u {
                                    c.usage[k] = v.clone();
                                }
                            }
                            c.stop_reason = e["delta"]["stop_reason"].as_str().map(String::from);
                        }
                        None
                    }
                    Some("message_stop") => self.flush("stop"),
                    _ => None,
                }
            }
            Some("assistant") => {
                let m = &ev["message"];
                // Without partial messages (or if message_start was missed), the call starts here.
                let mut done = None;
                if self.current.as_ref().is_none_or(|c| m["id"] != c.id.as_str()) {
                    done = self.flush("aborted");
                    self.current = Some(Call::start(m));
                }
                self.add_blocks(m);
                done
            }
            _ => None,
        }
    }

    fn add_blocks(&mut self, m: &Value) {
        let Some(c) = &mut self.current else { return };
        for b in m["content"].as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => c.content.push(json!({ "type": "text", "text": b["text"] })),
                Some("thinking") => c.content.push(json!({ "type": "thinking", "thinking": b["thinking"] })),
                Some("tool_use") => c.content.push(json!({ "type": "toolCall", "id": b["id"], "name": tool_name(b), "arguments": b["input"] })),
                _ => {}
            }
        }
    }

    /// End the current call as a message. `fallback` is the stop reason when the CLI gave none.
    fn flush(&mut self, fallback: &str) -> Option<Value> {
        let c = self.current.take()?;
        let has_tool = c.content.iter().any(|b| b["type"] == "toolCall");
        let stop = match c.stop_reason.as_deref() {
            Some("tool_use") => "toolUse",
            Some("max_tokens") => "length",
            Some(_) => "stop",
            None if has_tool && fallback == "stop" => "toolUse",
            None => fallback,
        };
        let n = |k: &str| c.usage[k].as_i64().unwrap_or(0);
        Some(json!({
            "role": "assistant", "content": c.content, "provider": "claude", "model": c.model,
            "usage": { "input": n("input_tokens"), "output": n("output_tokens"),
                       "cacheRead": n("cache_read_input_tokens"), "cacheWrite": n("cache_creation_input_tokens"),
                       "totalTokens": n("input_tokens") + n("output_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens"),
                       "cost": { "total": 0.0 } },
            "stopReason": stop, "durationMs": c.started.elapsed().as_millis() as i64, "timestamp": now_ms()
        }))
    }
}

fn tool_name(block: &Value) -> String {
    block["name"].as_str().unwrap_or("").trim_start_matches(PREFIX).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(event: Value) -> Value {
        json!({ "type": "stream_event", "event": event })
    }

    fn block(id: &str, b: Value) -> Value {
        json!({ "type": "assistant", "message": { "id": id, "model": "claude-x", "content": [b], "usage": { "output_tokens": 6 } } })
    }

    #[test]
    fn one_message_per_model_call_with_final_usage() {
        let mut calls = Calls::default();
        let events = [
            stream(json!({ "type": "message_start", "message": { "id": "m1", "model": "claude-x", "usage": { "input_tokens": 10, "output_tokens": 6 } } })),
            block("m1", json!({ "type": "thinking", "thinking": "hmm" })),
            block("m1", json!({ "type": "text", "text": "Running it." })),
            block("m1", json!({ "type": "tool_use", "id": "t1", "name": "mcp__zen__bash", "input": { "command": "echo hi" } })),
            stream(json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "input_tokens": 10, "output_tokens": 136, "cache_creation_input_tokens": 500 } })),
        ];
        for ev in &events {
            assert!(calls.feed(ev).is_none(), "nothing is sent before the call ends");
        }
        assert_eq!(calls.tool_uses(&events[3]), vec![("t1".to_string(), "bash".to_string(), json!({ "command": "echo hi" }))]);
        let m = calls.feed(&stream(json!({ "type": "message_stop" }))).expect("message at message_stop");
        let kinds: Vec<&str> = m["content"].as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["thinking", "text", "toolCall"]);
        assert_eq!(m["content"][2]["name"], "bash");
        assert_eq!(m["stopReason"], "toolUse");
        assert_eq!(m["usage"]["output"], 136, "final usage, not the count at the start of the call");
        assert_eq!(m["usage"]["cacheWrite"], 500);
        assert!(calls.flush("aborted").is_none());
    }

    #[test]
    fn a_call_cut_off_is_flushed_as_aborted() {
        let mut calls = Calls::default();
        calls.feed(&stream(json!({ "type": "message_start", "message": { "id": "m1", "usage": {} } })));
        calls.feed(&block("m1", json!({ "type": "text", "text": "partial" })));
        let m = calls.flush("aborted").unwrap();
        assert_eq!(m["stopReason"], "aborted");
        assert_eq!(m["content"][0]["text"], "partial");
    }

    #[test]
    fn turn_totals_include_side_calls() {
        let result = json!({ "total_cost_usd": 0.5, "modelUsage": {
            "claude-opus-5-5": { "inputTokens": 100, "outputTokens": 50, "cacheReadInputTokens": 1000, "cacheCreationInputTokens": 10 },
            "claude-haiku-4-5": { "inputTokens": 900, "outputTokens": 10, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0 } } });
        let mut u = json!({});
        turn_usage(&result, &mut u);
        assert_eq!((u["input"].as_i64(), u["output"].as_i64(), u["cache_read"].as_i64()), (Some(1000), Some(60), Some(1000)));
        assert_eq!(u["cost_usd"], 0.5);
    }
}
