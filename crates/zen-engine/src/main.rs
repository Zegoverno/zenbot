//! zen-engine — a zenbot worker that runs turns on the official Claude Code and Codex CLIs,
//! so the owner's existing subscriptions are used. zenbot keeps control of the loop's inputs
//! and outputs: its system prompt, its tools (executed by the kernel), and its session history.
//!
//! Speaks the worker protocol (docs/worker-protocol.md) on stdio. Also runs as the MCP bridge
//! that the Claude CLI launches: `zen-engine mcp-bridge <socket>`.

mod bridge;
mod claude;
mod codex;
mod engine;
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
use zen_proto::worker::{Ack, CompleteError, ModelsList, Pong, Request, TurnEvent};

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
    for e in engine::all() {
        e.prepare();
    }
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

/// Answer one request from the kernel. Every engine is reached through the registry (`engine`).
async fn handle(rpc: &Rpc, running: &Running, method: &str, p: Value) -> Result<Value> {
    let ack = || Ok(serde_json::to_value(Ack { ok: true })?);
    match Request::parse(method, p) {
        Ok(Request::Ping {}) => Ok(serde_json::to_value(Pong { pong: true })?),
        Ok(Request::ModelsList {}) => {
            // `--version` probes are blocking process runs: off the runtime's threads. "authenticated"
            // means the CLI is installed and answers; the sign-in itself is checked when a turn runs.
            let engines = tokio::task::spawn_blocking(|| engine::all().into_iter().map(|e| (e, e.version().is_some())).collect::<Vec<_>>())
                .await
                .unwrap_or_else(|_| engine::all().into_iter().map(|e| (e, false)).collect());
            let mut list = ModelsList::default();
            for (e, up) in engines {
                if e.subscription() {
                    list.authenticated.insert(e.name().into(), json!(up));
                }
                if up {
                    list.models.extend(e.models().await);
                }
            }
            Ok(serde_json::to_value(list)?)
        }
        Ok(Request::TurnStart(p)) => {
            let session_id = p.session_id.clone();
            // A kernel older than turn ids sends none; a local one keeps this turn's entry apart.
            let turn_id = p.turn_id.clone().unwrap_or_else(turn::new_uuid);
            let model = p.model.clone();
            let ctx = TurnCtx::new(rpc.clone(), session_id.clone(), turn_id.clone(), p.tools.clone());
            let input = TurnInput::from(p);
            let Some(abort_rx) = running.lock().await.start(&turn_id, &session_id) else {
                // Aborted before it started: it ends at once, without running.
                ctx.notify(TurnEvent::End { error: Some("interrupted".into()) }).await;
                return ack();
            };
            let running = running.clone();
            tokio::spawn(async move {
                let engine = engine::for_model(&model).map(|(e, _)| e);
                let result = match engine {
                    Some(e) => e.run_turn(ctx.clone(), &input, abort_rx.clone()).await,
                    None => Ok(Some(format!("zen-engine has no `{}` engine", model.split_once('/').map(|(e, _)| e).unwrap_or("")))),
                };
                let mut error = match result {
                    Ok(e) => e,
                    Err(e) => Some(e.to_string()),
                };
                if let (Some(first_error), Some(engine)) = (error.as_deref(), engine) {
                    if failover::is_hard_usage_limit(first_error) {
                        if let Some(continued) = failover::try_continue(engine, &input, &ctx, abort_rx.clone(), first_error).await {
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
                ctx.notify(TurnEvent::End { error }).await;
            });
            ack()
        }
        Ok(Request::Complete(p)) => {
            // One completion without tools (summaries, session names and suggestions). Errors are returned in the result.
            let res = match engine::for_model(&p.model) {
                Some((e, model)) => e.complete(model, &p.system, &p.prompt).await.map(serde_json::to_value),
                None => Err(anyhow::anyhow!("zen-engine has no `{}` engine", p.model.split_once('/').map(|(e, _)| e).unwrap_or(""))),
            };
            Ok(match res {
                Ok(v) => v?,
                Err(e) => serde_json::to_value(CompleteError { error: e.to_string() })?,
            })
        }
        Ok(Request::TurnAbort(p)) => {
            running.lock().await.abort(&p.session_id, p.turn_id.as_deref());
            ack()
        }
        Err(_) if !matches!(method, "ping" | "models.list" | "turn.start" | "turn.abort" | "complete") => anyhow::bail!("unknown method {method}"),
        Err(e) => anyhow::bail!("invalid `{method}` params: {e}"),
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
