//! Faux engine: a scripted model for tests and the upgrade smoke check. It drives a real turn
//! through the kernel (tool calls included) without any subscription.
//!
//! Listed as `faux/smoke` when ZEN_FAUX=1. The script is read from the JSON file in
//! ZEN_FAUX_SCRIPT if set: a list of steps (or an object of lists by kind of session, see `script`),
//! each one of
//!   {"tool": "<name>", "args": {...}}   call a kernel tool
//!   {"text": "..."}                    answer (streamed as deltas, then a message)
//!   {"sleep": <seconds>}               do nothing for a while (to test abort and the watchdog)
//!   {"exit": <code>}                   crash the worker (to test supervision)
//! A step with `"when": "<text>"` runs only in turns whose prompt contains the text, and one with
//! `"ignore_abort": true` keeps running through an abort (a worker that is slow to stop).

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::turn::{now_ms, TurnCtx, TurnInput};

pub fn enabled() -> bool {
    std::env::var("ZEN_FAUX").is_ok_and(|v| v == "1")
}

pub fn models() -> Vec<Value> {
    vec![json!({ "id": "faux/smoke", "name": "Test model (scripted)", "engine": "faux" })]
}

/// The steps for this turn. The script is a list of steps, or an object of lists keyed by the
/// kind of session: `verify` for a verifier (a child session that checks work), else `default`.
fn script(input: &TurnInput) -> Result<Vec<Value>> {
    match std::env::var("ZEN_FAUX_SCRIPT").ok().filter(|p| !p.trim().is_empty()) {
        Some(path) => {
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading ZEN_FAUX_SCRIPT {path}"))?;
            let v: Value = serde_json::from_str(&text).context("ZEN_FAUX_SCRIPT must be JSON")?;
            let key = if input.kind.as_deref() == Some("verifier") { "verify" } else { "default" };
            let steps = if v.is_object() { v.get(key).cloned().unwrap_or(json!([])) } else { v.clone() };
            serde_json::from_value(steps).context("ZEN_FAUX_SCRIPT must be a list of steps, or an object of them")
        }
        None => Ok(vec![
            json!({ "tool": "bash", "args": { "command": "echo zen-ok" } }),
            json!({ "text": "Smoke test passed: I ran a command through the kernel." }),
        ]),
    }
}

/// An assistant message. `input` is the size of what the model was sent (bytes / 4), so the
/// kernel's context measurements and summary triggers behave as with a real model.
fn assistant(content: Vec<Value>, stop: &str, input: i64) -> Value {
    json!({ "role": "assistant", "provider": "faux", "model": "smoke", "content": content, "stopReason": stop,
            "usage": { "input": input, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": input + 5, "cost": { "total": 0.0 } },
            "timestamp": now_ms() })
}

pub async fn run_turn(ctx: TurnCtx, input: &TurnInput, mut abort: watch::Receiver<bool>) -> Result<Option<String>> {
    let mut sent = (input.system.len() + Value::Array(input.history.clone()).to_string().len() + input.prompt.len()) as i64 / 4;
    for (i, step) in script(input)?.into_iter().enumerate() {
        if step["when"].as_str().is_some_and(|w| !input.prompt.contains(w)) {
            continue;
        }
        let work = async {
            if let Some(name) = step["tool"].as_str() {
                let call_id = format!("faux-{}-{i}", now_ms());
                let args = step.get("args").cloned().unwrap_or(json!({}));
                let call = json!({ "type": "toolCall", "id": call_id, "name": name, "arguments": args });
                ctx.notify("turn.message", json!({ "message": assistant(vec![call], "toolUse", sent) })).await;
                let (out, _) = ctx.call_tool(name, args, Some(call_id)).await;
                sent += out.len() as i64 / 4;
            } else if let Some(text) = step["text"].as_str() {
                for word in text.split_inclusive(' ') {
                    ctx.notify("turn.delta", json!({ "delta": word })).await;
                }
                let msg = assistant(vec![json!({ "type": "text", "text": text })], "stop", sent);
                ctx.notify("turn.message", json!({ "message": msg })).await;
            } else if let Some(secs) = step["sleep"].as_f64() {
                tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
            } else if let Some(code) = step["exit"].as_i64() {
                std::process::exit(code as i32);
            }
        };
        if step["ignore_abort"] == true {
            work.await;
            continue;
        }
        tokio::select! {
            _ = work => {}
            _ = abort.changed() => return Ok(Some("interrupted".into())),
        }
    }
    ctx.notify("turn.usage", json!({ "engine": "faux", "engine_version": env!("CARGO_PKG_VERSION") })).await;
    Ok(None)
}

/// A scripted completion for tests: a valid summary that keeps every line marked `FACT:` from the
/// prompt, so tests can check what survives summarizing.
pub fn complete(prompt: &str) -> Value {
    let facts: Vec<Value> = prompt
        .lines()
        .filter_map(|l| l.find("FACT:").map(|i| json!({ "text": l[i..].trim_end_matches(['"', '\\']).to_string(), "refs": [] })))
        .collect();
    let summary = json!({ "goal": "(scripted summary)", "state": "", "decisions": [], "files": [], "facts": facts, "open": [], "next": "" });
    json!({ "text": summary.to_string(), "usage": { "input": prompt.len() / 4, "output": 50 }, "model": "faux/smoke" })
}
