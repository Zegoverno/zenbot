//! Faux engine: a scripted model for tests and the upgrade smoke check. It drives a real turn
//! through the kernel (tool calls included) without any subscription.
//!
//! Listed as `faux/smoke` when ZEN_FAUX=1. The script is read from the JSON file in
//! ZEN_FAUX_SCRIPT if set: a list of steps, each one of
//!   {"tool": "<name>", "args": {...}}   call a kernel tool
//!   {"text": "..."}                    answer (streamed as deltas, then a message)
//!   {"sleep": <seconds>}               do nothing for a while (to test abort and the watchdog)
//!   {"exit": <code>}                   crash the worker (to test supervision)

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::turn::{now_ms, TurnCtx};

pub fn enabled() -> bool {
    std::env::var("ZEN_FAUX").is_ok_and(|v| v == "1")
}

pub fn models() -> Vec<Value> {
    vec![json!({ "id": "faux/smoke", "name": "Test model (scripted)", "engine": "faux" })]
}

fn script() -> Result<Vec<Value>> {
    match std::env::var("ZEN_FAUX_SCRIPT") {
        Ok(path) => {
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading ZEN_FAUX_SCRIPT {path}"))?;
            serde_json::from_str(&text).context("ZEN_FAUX_SCRIPT must be a JSON list of steps")
        }
        Err(_) => Ok(vec![
            json!({ "tool": "bash", "args": { "command": "echo zen-ok" } }),
            json!({ "text": "Smoke test passed: I ran a command through the kernel." }),
        ]),
    }
}

fn assistant(content: Vec<Value>, stop: &str) -> Value {
    json!({ "role": "assistant", "provider": "faux", "model": "smoke", "content": content, "stopReason": stop,
            "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15, "cost": { "total": 0.0 } },
            "timestamp": now_ms() })
}

pub async fn run_turn(ctx: TurnCtx, mut abort: watch::Receiver<bool>) -> Result<Option<String>> {
    for (i, step) in script()?.into_iter().enumerate() {
        let work = async {
            if let Some(name) = step["tool"].as_str() {
                let call_id = format!("faux-{}-{i}", now_ms());
                let args = step.get("args").cloned().unwrap_or(json!({}));
                let call = json!({ "type": "toolCall", "id": call_id, "name": name, "arguments": args });
                ctx.rpc.notify("turn.message", json!({ "session_id": ctx.session_id, "message": assistant(vec![call], "toolUse") })).await;
                ctx.call_tool(name, args, Some(call_id)).await;
            } else if let Some(text) = step["text"].as_str() {
                for word in text.split_inclusive(' ') {
                    ctx.rpc.notify("turn.delta", json!({ "session_id": ctx.session_id, "delta": word })).await;
                }
                let msg = assistant(vec![json!({ "type": "text", "text": text })], "stop");
                ctx.rpc.notify("turn.message", json!({ "session_id": ctx.session_id, "message": msg })).await;
            } else if let Some(secs) = step["sleep"].as_f64() {
                tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
            } else if let Some(code) = step["exit"].as_i64() {
                std::process::exit(code as i32);
            }
        };
        tokio::select! {
            _ = work => {}
            _ = abort.changed() => return Ok(Some("interrupted".into())),
        }
    }
    Ok(None)
}
