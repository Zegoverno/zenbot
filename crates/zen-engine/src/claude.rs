//! Claude engine: runs each turn with the official `claude` CLI on the owner's Claude plan.
//! Built-in tools are off; the only tools are zenbot's (via the MCP bridge), and zenbot's system
//! prompt replaces Claude Code's. Every run must show its tool list (`system/init`, only notices may come before it), with
//! nothing but zenbot's tools in it, or it is stopped (`check_init`).
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

use crate::turn::{enabled, new_uuid, now_ms, prompt_blocks, seed_blocks, StderrTail, TurnCtx, TurnInput, COMPLETE_LIMIT};

const PREFIX: &str = "mcp__zen__";

/// Full model ids, never the CLI's aliases (`opus`, …): an alias moves to a new model when
/// Claude Code is updated, so the same session could silently run on a different model.
///
/// Effort levels are Claude Code's `--effort` values. The CLI doesn't report the level it uses when
/// none is given, so zenbot always passes one: `default_effort` unless the session picks another.
pub fn models() -> Vec<Value> {
    let model = |id: &str, name: &str| {
        json!({ "id": format!("claude/{id}"), "name": name, "engine": "claude-code",
                "efforts": EFFORTS, "default_effort": DEFAULT_EFFORT })
    };
    vec![
        model("claude-opus-5-5", "Claude Opus 5.5"),
        model("claude-sonnet-5-5", "Claude Sonnet 5.5"),
        model("claude-haiku-5-5", "Claude Haiku 5.5"),
        model("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
    ]
}

const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// The level a session runs at unless it picks another, and the one summaries (`complete`) use.
const DEFAULT_EFFORT: &str = "medium";

/// Claude Code's configuration directory (where it keeps sessions).
fn config_dir() -> std::path::PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude"))
}

/// The fixed directory engine sessions run in. Claude Code files sessions by directory and puts the
/// directory in every request, so it must not change between turns.
fn session_dir() -> std::path::PathBuf {
    crate::turn::engine_dir("claude").unwrap_or_else(|_| std::env::temp_dir())
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

/// Tools in Claude Code's `system/init` event that aren't zenbot's (`mcp__zen__*`), if any.
fn unmediated_tools(init: &Value) -> Option<String> {
    let bad: Vec<&str> = init["tools"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|t| !t.starts_with(PREFIX)).collect();
    (!bad.is_empty()).then(|| bad.join(", "))
}

/// Check an event of the CLI's stream before it is handled, given whether `system/init` was seen.
/// Every action must go through the kernel: the run must start with `system/init` listing its
/// tools (as an array), none but zenbot's; if a CLI release stops honoring `--tools ""` or stops
/// showing its tools, the run is refused before the model can use a built-in tool.
/// Notices the CLI may send before `system/init` (rate-limit status, other `system` events) carry
/// no model output or tool use, so they pass; anything else before init is refused.
/// Returns Ok(true) for the init event, Ok(false) for any other allowed event, Err(why) to refuse.
fn check_init(ev: &Value, init_seen: bool) -> Result<bool, String> {
    let is_init = ev["type"] == "system" && ev["subtype"] == "init";
    if !is_init {
        let notice = matches!(ev["type"].as_str(), Some("rate_limit_event" | "system"));
        return if init_seen || notice { Ok(false) } else { Err(format!("claude sent `{}` before its system/init event, so its tools can't be checked; refusing the run (has the stream format changed?)", ev["type"].as_str().unwrap_or("?"))) };
    }
    if !ev["tools"].is_array() {
        return Err("claude's system/init event has no tool list, so its tools can't be checked; refusing the run (has the stream format changed?)".into());
    }
    match unmediated_tools(ev) {
        Some(bad) => Err(format!("claude offered tools zenbot doesn't control ({bad}); refusing the run (does the CLI still honor --tools \"\"?)")),
        None => Ok(true),
    }
}

/// Stops the turn's tool socket server when dropped, however `run_turn` ends.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The text of a `tool_result` block (a string, or text blocks).
fn result_text(block: &Value) -> String {
    zen_proto::text_of(&block["content"])
}

pub fn available() -> bool {
    std::process::Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

pub async fn run_turn(ctx: TurnCtx, input: &TurnInput, abort: watch::Receiver<bool>) -> Result<Option<String>> {
    let model = input.model.as_str();
    // The turn's report, sent on every path (an early error included): what ran, and what it used.
    let mut usage = json!({ "engine": "claude-code", "provider": "claude", "model": model });
    let outcome = drive(&ctx, input, abort, &mut usage).await;
    ctx.notify("turn.usage", usage).await;
    outcome
}

/// Run the turn's CLI, filling in `usage` as it learns it.
async fn drive(ctx: &TurnCtx, input: &TurnInput, mut abort: watch::Receiver<bool>, usage: &mut Value) -> Result<Option<String>> {
    let model = input.model.as_str();
    let (socket_dir, socket) = crate::turn::socket_dir().context("creating the tool socket's folder")?;
    // The socket and the prompt file go when the turn ends, on every path.
    let _turn_dir = crate::turn::RemoveOnDrop(socket_dir.clone());
    let system_file = socket_dir.join("system.md");
    crate::turn::private_file(&system_file, &input.system).context("writing the system prompt file")?;
    let _server = AbortOnDrop(ctx.serve_socket(&socket).context("opening tool socket")?);
    let exe = std::env::current_exe()?.display().to_string();
    let mcp = json!({ "mcpServers": { "zen": { "command": exe, "args": ["mcp-bridge", socket] } } }).to_string();
    let allowed: Vec<String> = ctx.tools.iter().filter_map(|t| t["name"].as_str()).map(|n| format!("{PREFIX}{n}")).collect();

    // How this turn reaches Claude Code: continue its session, start one seeded from the tape, or
    // (sessions off) a throwaway session with the replayed history.
    let sessions = enabled("ZEN_CLAUDE_RESUME");
    let fallback = match &input.resume {
        Some(id) if sessions && !session_exists(id) => Some(format!("engine session {id} is gone; seeding a new one from the history")),
        _ => None,
    };
    let (render, engine_session, content) = match &input.resume {
        Some(id) if sessions && session_exists(id) => ("resume", Some(id.clone()), prompt_blocks(&input.prompt, input.context.as_deref())),
        _ if sessions => ("seed", Some(new_uuid()), seed_blocks(&input.history, &input.prompt, input.context.as_deref())),
        _ => ("transcript", None, seed_blocks(&input.history, &input.prompt, input.context.as_deref())),
    };
    usage["render"] = json!(render);
    if let Some(reason) = &fallback {
        eprintln!("[engine] claude: {reason}");
        usage["fallback_reason"] = json!(reason);
    }
    // Until the turn has finished cleanly, the engine session isn't in sync with the tape.
    if let Some(id) = &engine_session {
        usage["engine_session"] = json!({ "id": id, "resumable": false });
    }
    let dir = if sessions { session_dir() } else { std::env::temp_dir().join(format!("zen-claude-{}", now_ms())) };
    std::fs::create_dir_all(&dir)?;
    let _throwaway = (!sessions).then(|| crate::turn::RemoveOnDrop(dir.clone()));

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
        .arg("--system-prompt-file")
        .arg(&system_file)
        .args(["--model", model])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting the claude CLI (is Claude Code installed and signed in?)")?;

    let stderr = StderrTail::collect(child.stderr.take());
    let mut stdin = child.stdin.take().context("claude stdin")?;
    let user = json!({ "type": "user", "message": { "role": "user", "content": content } });
    let mut outcome: Option<Result<Option<String>>> = None;
    if let Err(e) = async {
        stdin.write_all(format!("{user}\n").as_bytes()).await?;
        stdin.flush().await
    }
    .await
    {
        let _ = child.start_kill();
        outcome = Some(Err(anyhow::anyhow!("{}", stderr.explain(&format!("sending the prompt to claude failed ({e})")).await)));
    }
    let mut stdin = Some(stdin);

    let mut lines = BufReader::new(child.stdout.take().context("claude stdout")?).lines();
    let mut calls = Calls::default();
    // Messages sent to the tape, and stream lines this code doesn't understand: if the CLI's
    // stream format changes, the turn fails loudly instead of answering with nothing.
    let mut sent = 0;
    let mut unknown = 0;
    let mut init_seen = false;

    while outcome.is_none() {
        tokio::select! {
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(l)) => l,
                    Ok(None) => break,
                    Err(e) => { outcome = Some(Err(anyhow::anyhow!("reading claude's output: {e}"))); break; }
                };
                let Ok(ev) = serde_json::from_str::<Value>(&line) else { unknown += 1; continue };
                match check_init(&ev, init_seen) {
                    Ok(true) => {
                        init_seen = true;
                        usage["engine_version"] = ev["claude_code_version"].clone();
                        continue;
                    }
                    Ok(false) => {}
                    Err(why) => {
                        let _ = child.start_kill();
                        outcome = Some(Ok(Some(why)));
                        break;
                    }
                }
                if !matches!(ev["type"].as_str(), Some("stream_event" | "assistant" | "user" | "system" | "result")) {
                    unknown += 1;
                }
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
                    sent += 1;
                }
                // Results the CLI gave the model without the kernel (an unknown tool, invalid
                // arguments): the model read them, so they go on the tape too. The call's message
                // must be on the tape first.
                let own: Vec<(String, String, bool)> = engine_results(&ev).into_iter().filter(|(id, _, _)| !ctx.is_answered(id)).collect();
                if !own.is_empty() {
                    if let Some(message) = calls.flush("toolUse") {
                        ctx.notify("turn.message", json!({ "message": message })).await;
                        sent += 1;
                    }
                    for (id, text, is_error) in own {
                        ctx.engine_result(&id, &text, is_error).await;
                    }
                }
                if ev["type"] == "result" {
                    turn_usage(&ev, usage);
                    let answered = ev["result"].as_str().is_some_and(|t| !t.trim().is_empty());
                    let err = if ev["is_error"] == true {
                        Some(ev["result"].as_str().or(ev["subtype"].as_str()).unwrap_or("claude reported an error").to_string())
                    } else if answered && sent == 0 {
                        Some(format!("claude stream format changed: the CLI answered, but no assistant message was read from its stream ({unknown} lines not understood)"))
                    } else { None };
                    outcome = Some(Ok(err));
                    stdin = None; // closing stdin lets the CLI exit
                }
            }
            _ = abort.changed() => {
                let _ = child.start_kill();
                outcome = Some(Ok(Some("interrupted".into())));
            }
        }
    }
    // A model call cut off (abort, crash) still goes on the tape with what it had produced.
    if let Some(message) = calls.flush("aborted") {
        ctx.notify("turn.message", json!({ "message": message })).await;
    }
    drop(stdin);
    // The result is in: the CLI exits once stdin closes. One that doesn't is killed.
    let status = match tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await {
        Ok(s) => s.ok(),
        Err(_) => {
            let _ = child.start_kill();
            child.wait().await.ok()
        }
    };
    if unknown > 0 {
        eprintln!("[engine] claude: {unknown} stream lines not understood");
    }
    // Only a turn that finished cleanly leaves the engine session in sync with the tape.
    if let (Some(Ok(None)), Some(es)) = (&outcome, usage["engine_session"].as_object_mut()) {
        es.insert("resumable".into(), json!(true));
    }
    match outcome {
        Some(o) => o,
        None => Ok(Some(stderr.explain(&format!("claude exited unexpectedly ({})", status.map(|s| s.to_string()).unwrap_or_default())).await)),
    }
}

/// The `tool_result` blocks in a `user` event of the stream: (call id, text, is_error).
fn engine_results(ev: &Value) -> Vec<(String, String, bool)> {
    if ev["type"] != "user" {
        return Vec::new();
    }
    let blocks = ev["message"]["content"].as_array().into_iter().flatten();
    blocks
        .filter(|b| b["type"] == "tool_result")
        .filter_map(|b| Some((b["tool_use_id"].as_str()?.to_string(), result_text(b), b["is_error"] == true)))
        .collect()
}

/// One completion without tools, in a throwaway session (summaries). Returns `{ text, usage, model }`.
/// Its stream must show the run's tools off (`check_init`), like a turn's; given up (the CLI
/// killed) after `COMPLETE_LIMIT`.
pub async fn complete(model: &str, system: &str, prompt: &str) -> Result<Value> {
    // A fixed, empty directory: Claude Code tells the model its working directory.
    let dir = crate::turn::engine_dir("complete")?;
    let (prompt_dir, _) = crate::turn::socket_dir()?;
    let system_file = prompt_dir.join("system.md");
    let _cleanup = crate::turn::RemoveOnDrop(prompt_dir);
    crate::turn::private_file(&system_file, system)?;
    let mut child = Command::new("claude")
        .args(["-p", "--output-format", "stream-json", "--verbose", "--tools", "", "--setting-sources", "", "--no-session-persistence"])
        .args(["--strict-mcp-config", "--effort", DEFAULT_EFFORT, "--system-prompt-file"])
        .arg(&system_file)
        .args(["--model", model])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting the claude CLI")?;
    let stderr = StderrTail::collect(child.stderr.take());
    let mut stdin = child.stdin.take().context("claude stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("claude stdout")?).lines();
    let read = async {
        stdin.write_all(prompt.as_bytes()).await?;
        drop(stdin);
        let mut init_seen = false;
        while let Some(line) = lines.next_line().await? {
            let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
            match check_init(&ev, init_seen) {
                Ok(seen) => init_seen |= seen,
                Err(why) => anyhow::bail!("{why}"),
            }
            if ev["type"] == "result" {
                return Ok(Some(ev));
            }
        }
        Ok::<_, anyhow::Error>(None)
    };
    let res = match tokio::time::timeout(COMPLETE_LIMIT, read).await {
        Ok(Ok(Some(res))) => res,
        Ok(Ok(None)) => anyhow::bail!("{}", stderr.explain("claude gave no result").await),
        Ok(Err(e)) => {
            let _ = child.start_kill();
            return Err(e);
        }
        Err(_) => {
            let _ = child.start_kill();
            anyhow::bail!("claude didn't complete within {} s", COMPLETE_LIMIT.as_secs());
        }
    };
    let _ = child.wait().await;
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
    fn only_zen_tools_may_be_offered() {
        assert_eq!(unmediated_tools(&json!({ "tools": [format!("{PREFIX}bash"), format!("{PREFIX}read")] })), None);
        assert_eq!(unmediated_tools(&json!({ "tools": [] })), None);
        assert_eq!(unmediated_tools(&json!({ "tools": [format!("{PREFIX}bash"), "Bash", "Read"] })).as_deref(), Some("Bash, Read"));
    }

    #[test]
    fn the_tool_list_must_be_seen_first() {
        let init = |tools: Value| json!({ "type": "system", "subtype": "init", "tools": tools });
        assert_eq!(check_init(&init(json!([format!("{PREFIX}bash")])), false), Ok(true));
        assert_eq!(check_init(&json!({ "type": "assistant" }), true), Ok(false));
        assert!(check_init(&json!({ "type": "assistant" }), false).is_err(), "an event before init");
        assert_eq!(check_init(&json!({ "type": "rate_limit_event" }), false), Ok(false), "a notice before init");
        assert_eq!(check_init(&json!({ "type": "system", "subtype": "status" }), false), Ok(false));
        assert!(check_init(&json!({ "type": "result" }), false).is_err());
        assert!(check_init(&json!({ "type": "stream_event" }), false).is_err());
        assert!(check_init(&json!({ "type": "system", "subtype": "init" }), false).is_err(), "init without a tool list");
        assert!(check_init(&init(json!(["Bash"])), false).unwrap_err().contains("Bash"));
    }

    #[test]
    fn results_the_cli_gave_itself_are_read() {
        let ev = json!({ "type": "user", "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t1", "content": "<tool_use_error>Error: No such tool available: Bash</tool_use_error>", "is_error": true },
            { "type": "tool_result", "tool_use_id": "t2", "content": [{ "type": "text", "text": "fine" }] } ] } });
        assert_eq!(engine_results(&ev), vec![
            ("t1".to_string(), "<tool_use_error>Error: No such tool available: Bash</tool_use_error>".to_string(), true),
            ("t2".to_string(), "fine".to_string(), false),
        ]);
        assert!(engine_results(&json!({ "type": "assistant" })).is_empty());
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
