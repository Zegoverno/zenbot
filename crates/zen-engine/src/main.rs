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

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{watch, Mutex};

use rpc::Rpc;
use turn::{TurnCtx, TurnInput};

/// Turns running, by the kernel's turn id: their session and the switch that aborts them.
type Running = Arc<Mutex<HashMap<String, (String, watch::Sender<bool>)>>>;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("mcp-bridge") {
        return bridge::run(args.get(2).cloned().unwrap_or_default()).await;
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
            let (abort_tx, abort_rx) = watch::channel(false);
            running.lock().await.insert(turn_id.clone(), (session_id.clone(), abort_tx));
            let tools = p["tools"].as_array().cloned().unwrap_or_default();
            let ctx = TurnCtx::new(rpc.clone(), session_id.clone(), turn_id.clone(), tools);
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
                failover::publish_usage(&ctx).await;
                running.lock().await.remove(&turn_id);
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
            // The turn named, or every turn of the session (a stale one the kernel ended included).
            let session_id = p["session_id"].as_str().unwrap_or("");
            for (turn_id, (session, tx)) in running.lock().await.iter() {
                if session == session_id && p["turn_id"].as_str().is_none_or(|t| t == turn_id) {
                    let _ = tx.send(true);
                }
            }
            Ok(json!({ "ok": true }))
        }
        _ => anyhow::bail!("unknown method {method}"),
    }
}
