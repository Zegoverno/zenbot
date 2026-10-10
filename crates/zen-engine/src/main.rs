//! zen-engine — a zenbot worker that runs turns on the official Claude Code and Codex CLIs,
//! so the owner's existing subscriptions are used. zenbot keeps control of the loop's inputs
//! and outputs: its system prompt, its tools (executed by the kernel), and its session history.
//!
//! Speaks the worker protocol (docs/worker-protocol.md) on stdio. Also runs as the MCP bridge
//! that the Claude CLI launches: `zen-engine mcp-bridge <socket>`.

mod bridge;
mod claude;
mod codex;
mod faux;
mod failover;
mod rpc;
mod turn;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{watch, Mutex};

use rpc::Rpc;
use turn::{TurnCtx, TurnInput};

/// Turns running, by the kernel's turn id, and the turns aborted before their `turn.start` came.
type Running = Arc<Mutex<Turns>>;

#[derive(Default)]
struct Turns {
    /// Each running turn's session and the switch that aborts it.
    running: HashMap<String, (String, watch::Sender<bool>)>,
    /// Turn ids aborted while not running here (yet): a `turn.start` for one ends at once. The
    /// kernel may abort a turn before its `turn.start` arrives; the last few hundred are kept.
    aborted: VecDeque<String>,
}

impl Turns {
    const TOMBSTONES: usize = 256;

    /// Register a turn. None if it was aborted already (its `turn.abort` came first).
    fn start(&mut self, turn_id: &str, session_id: &str) -> Option<watch::Receiver<bool>> {
        if self.aborted.iter().any(|t| t == turn_id) {
            return None;
        }
        let (tx, rx) = watch::channel(false);
        self.running.insert(turn_id.to_string(), (session_id.to_string(), tx));
        Some(rx)
    }

    /// Abort the turn named, or every turn of the session (a stale one the kernel ended included).
    /// A named turn that isn't running is remembered, so it can't start later.
    fn abort(&mut self, session_id: &str, turn_id: Option<&str>) {
        let mut found = false;
        for (id, (session, tx)) in &self.running {
            if session == session_id && turn_id.is_none_or(|t| t == id) {
                let _ = tx.send(true);
                found = true;
            }
        }
        if let (Some(t), false) = (turn_id, found) {
            if self.aborted.len() >= Self::TOMBSTONES {
                self.aborted.pop_front();
            }
            self.aborted.push_back(t.to_string());
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("mcp-bridge") => return bridge::run(args.get(2).cloned().unwrap_or_default()).await,
        Some("codex-features") => std::process::exit(codex::check_features()),
        _ => {}
    }
    serve().await
}

async fn serve() -> Result<()> {
    let rpc = Rpc::new();
    let running: Running = Arc::default();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    // Requests being answered. When stdin closes we still finish these, or a reply (e.g. to a
    // piped-in `ping`) could be lost as the process exits.
    let mut answering = tokio::task::JoinSet::new();
    claude::clean_sessions();
    eprintln!("[engine] ready");
    while let Some(line) = lines.next_line().await? {
        while answering.try_join_next().is_some() {}
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(method) = msg["method"].as_str().map(String::from) else {
            rpc.resolve(&msg).await;
            continue;
        };
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        // Starting and aborting turns are handled here, in the order they came: an abort sent
        // just after a `turn.start` must find the turn registered, and one sent before it must
        // stop it from starting. Both are quick (the turn itself runs in its own task).
        if matches!(method.as_str(), "turn.start" | "turn.abort") {
            let result = handle(&rpc, &running, &method, params).await;
            if let Some(id) = id {
                rpc.respond(id, result.map_err(|e| e.to_string())).await;
            }
            continue;
        }
        let rpc = rpc.clone();
        let running = running.clone();
        answering.spawn(async move {
            let result = handle(&rpc, &running, &method, params).await;
            if let Some(id) = id {
                rpc.respond(id, result.map_err(|e| e.to_string())).await;
            }
        });
    }
    while answering.join_next().await.is_some() {}
    Ok(())
}

async fn handle(rpc: &Rpc, running: &Running, method: &str, p: Value) -> Result<Value> {
    match method {
        "ping" => Ok(json!({ "pong": true })),
        "models.list" => {
            // `--version` probes are blocking process runs: off the runtime's threads. "authenticated"
            // means the CLI is installed and answers; the sign-in itself is checked when a turn runs.
            let (claude, codex) = tokio::task::spawn_blocking(|| (claude::available(), codex::available())).await.unwrap_or((false, false));
            let mut models: Vec<Value> = if claude { claude::models() } else { vec![] };
            if codex {
                models.extend(codex::models().await);
            }
            if faux::enabled() {
                models.extend(faux::models());
            }
            Ok(json!({ "authenticated": { "claude": claude, "codex": codex }, "models": models }))
        }
        "turn.start" => {
            let session_id = p["session_id"].as_str().unwrap_or("").to_string();
            // A kernel older than turn ids sends none; a local one keeps this turn's entry apart.
            let turn_id = p["turn_id"].as_str().map(String::from).unwrap_or_else(turn::new_uuid);
            let engine = p["model"].as_str().unwrap_or("").split_once('/').map(|(e, _)| e.to_string()).unwrap_or_default();
            let input = TurnInput::from_params(&p);
            let tools = p["tools"].as_array().cloned().unwrap_or_default();
            let ctx = TurnCtx::new(rpc.clone(), session_id.clone(), turn_id.clone(), tools);
            let Some(abort_rx) = running.lock().await.start(&turn_id, &session_id) else {
                // Aborted before it started: it ends at once, without running.
                ctx.notify("turn.end", json!({ "error": "interrupted" })).await;
                return Ok(json!({ "ok": true }));
            };
            let running = running.clone();
            tokio::spawn(async move {
                let result = if engine == "faux" && faux::enabled() {
                    faux::run_turn(ctx.clone(), &input, abort_rx.clone()).await
                } else {
                    failover::run(&engine, ctx.clone(), &input, abort_rx.clone()).await
                };
                let mut error = match result {
                    Ok(e) => e,
                    Err(e) => Some(e.to_string()),
                };
                if let Some(first_error) = error.as_deref() {
                    if failover::is_hard_usage_limit(first_error) {
                        if let Some(continued) = failover::try_continue(
                            &engine,
                            &input,
                            &ctx,
                            abort_rx.clone(),
                            first_error,
                        )
                        .await
                        {
                            error = continued;
                        }
                    }
                }
                // No tool call is left on the tape without a result.
                ctx.settle().await;
                let unmatched = ctx.unmatched_calls();
                if error.is_none() && unmatched > 0 {
                    error = Some(format!("{unmatched} tool call(s) ran without the model's announcement in the engine's stream, so the tape can't pair them with their results (has the stream format changed?)"));
                }
                failover::publish_usage(&ctx).await;
                running.lock().await.running.remove(&turn_id);
                ctx.notify("turn.end", json!({ "error": error })).await;
            });
            Ok(json!({ "ok": true }))
        }
        "complete" => {
            // One completion without tools (summaries, session names and suggestions). Errors are returned in the result.
            let model_ref = p["model"].as_str().unwrap_or("");
            let (engine, model) = model_ref.split_once('/').unwrap_or(("", model_ref));
            let (system, prompt) = (p["system"].as_str().unwrap_or(""), p["prompt"].as_str().unwrap_or(""));
            let res = match engine {
                "claude" => claude::complete(model, system, prompt).await,
                "codex" => codex::complete(model, system, prompt).await,
                "faux" if faux::enabled() => Ok(faux::complete(system, prompt)),
                other => Err(anyhow::anyhow!("zen-engine has no `{other}` engine")),
            };
            Ok(res.unwrap_or_else(|e| json!({ "error": e.to_string() })))
        }
        "turn.abort" => {
            running.lock().await.abort(p["session_id"].as_str().unwrap_or(""), p["turn_id"].as_str());
            Ok(json!({ "ok": true }))
        }
        _ => anyhow::bail!("unknown method {method}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_abort_before_its_start_keeps_the_turn_from_running() {
        let mut turns = Turns::default();
        turns.abort("s1", Some("t1"));
        assert!(turns.start("t1", "s1").is_none(), "aborted before it started");
        let rx = turns.start("t2", "s1").expect("another turn starts");
        turns.abort("s1", Some("t2"));
        assert!(*rx.borrow(), "a running turn is aborted");
        assert!(!turns.aborted.contains(&"t2".to_string()), "no tombstone for a turn that was running");
        let rx = turns.start("t3", "s2").unwrap();
        turns.abort("s2", None);
        assert!(*rx.borrow(), "without a turn id, every turn of the session");
        for i in 0..300 {
            turns.abort("s", Some(&format!("x{i}")));
        }
        assert_eq!(turns.aborted.len(), Turns::TOMBSTONES);
    }
}
