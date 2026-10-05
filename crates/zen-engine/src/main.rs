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
    let running: Arc<Mutex<HashMap<String, watch::Sender<bool>>>> = Arc::default();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    // Requests being answered. When stdin closes we still finish these, or a reply (e.g. to a
    // piped-in `ping`) could be lost as the process exits.
    let mut answering = tokio::task::JoinSet::new();
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

async fn handle(rpc: &Rpc, running: &Arc<Mutex<HashMap<String, watch::Sender<bool>>>>, method: &str, p: Value) -> Result<Value> {
    match method {
        "ping" => Ok(json!({ "pong": true })),
        "models.list" => {
            let claude = claude::available();
            let codex = codex::available();
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
            let engine = p["model"].as_str().unwrap_or("").split_once('/').map(|(e, _)| e.to_string()).unwrap_or_default();
            let input = TurnInput::from_params(&p);
            let (abort_tx, abort_rx) = watch::channel(false);
            running.lock().await.insert(session_id.clone(), abort_tx);
            let tools = p["tools"].as_array().cloned().unwrap_or_default();
            let ctx = TurnCtx::new(rpc.clone(), session_id.clone(), tools);
            let rpc = rpc.clone();
            let running = running.clone();
            tokio::spawn(async move {
                let result = match engine.as_str() {
                    "claude" => claude::run_turn(ctx, &input, abort_rx).await,
                    "codex" => codex::run_turn(ctx, &input, abort_rx).await,
                    "faux" if faux::enabled() => faux::run_turn(ctx, abort_rx).await,
                    other => Ok(Some(format!("zen-engine has no `{other}` engine"))),
                };
                let error = match result {
                    Ok(e) => e,
                    Err(e) => Some(e.to_string()),
                };
                running.lock().await.remove(&session_id);
                rpc.notify("turn.end", json!({ "session_id": session_id, "error": error })).await;
            });
            Ok(json!({ "ok": true }))
        }
        "turn.abort" => {
            if let Some(tx) = running.lock().await.get(p["session_id"].as_str().unwrap_or("")) {
                let _ = tx.send(true);
            }
            Ok(json!({ "ok": true }))
        }
        _ => anyhow::bail!("unknown method {method}"),
    }
}
