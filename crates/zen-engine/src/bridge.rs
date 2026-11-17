//! MCP bridge. Claude Code starts `zen-engine mcp-bridge <socket>` as a stdio MCP server;
//! the bridge answers tools/list and tools/call by asking the engine over a Unix socket,
//! and the engine forwards each call to the kernel, which executes it.
//!
//! Long tool calls: Claude Code aborts a call whose server sends "no response or progress" for its
//! idle limit (30 min for stdio servers), and the abort ends the turn's work with it (a `delegate`
//! child loses its turn). Claude Code asks for progress (`_meta.progressToken`) on every
//! tools/call, and each `notifications/progress` resets that limit, so while a call runs the
//! bridge sends one every `PROGRESS_EVERY`. The engine also starts the CLI with the limit off
//! (`CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT=0`, claude.rs); the kernel's own limits bound the tools.
//! A call without a token is reported on stderr (Claude Code's MCP log): the CLI changed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

/// How often a running tool call reports progress; well under any idle limit Claude Code uses.
const PROGRESS_EVERY: Duration = Duration::from_secs(30);

/// One request/response exchange with the engine over its per-turn socket.
pub async fn ask(socket: &str, req: &Value) -> Result<Value> {
    let stream = UnixStream::connect(socket).await.context("connecting to zen-engine")?;
    let (r, mut w) = stream.into_split();
    let mut line = req.to_string();
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    let mut resp = String::new();
    BufReader::new(r).read_line(&mut resp).await?;
    Ok(serde_json::from_str(&resp)?)
}

/// Write one JSON-RPC message as a line; the lock keeps lines whole.
async fn send<W: AsyncWrite + Unpin>(out: &Mutex<W>, msg: &Value) {
    let mut line = msg.to_string();
    line.push('\n');
    let mut o = out.lock().await;
    let _ = o.write_all(line.as_bytes()).await;
    let _ = o.flush().await;
}

/// Run `call`, sending `notifications/progress` for `token` every `every` until it finishes.
async fn with_progress<W, F>(call: F, token: Option<Value>, out: &Mutex<W>, every: Duration) -> F::Output
where
    W: AsyncWrite + Unpin,
    F: std::future::Future,
{
    let Some(token) = token else { return call.await };
    let start = Instant::now();
    let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tokio::pin!(call);
    loop {
        tokio::select! {
            result = &mut call => return result,
            _ = ticks.tick() => {
                let secs = start.elapsed().as_secs();
                send(out, &json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": {
                    "progressToken": token,
                    "progress": start.elapsed().as_millis() as u64,
                    "message": format!("still running ({secs}s)"),
                }})).await;
            }
        }
    }
}

pub async fn run(socket: String) -> Result<()> {
    let out = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // notifications need no answer
        let method = msg["method"].as_str().unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let socket = socket.clone();
        let out = out.clone();
        tokio::spawn(async move {
            let mut reply = match method.as_str() {
                "initialize" => json!({ "result": {
                    "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "zen", "version": env!("CARGO_PKG_VERSION") }
                }}),
                "ping" => json!({ "result": {} }),
                "tools/list" => match ask(&socket, &json!({ "op": "list" })).await {
                    Ok(v) => json!({ "result": { "tools": v["tools"] } }),
                    Err(e) => json!({ "error": { "code": -32000, "message": e.to_string() } }),
                },
                "tools/call" => {
                    let req = json!({ "op": "call", "name": params["name"], "args": params.get("arguments").cloned().unwrap_or(json!({})) });
                    let token = params["_meta"].get("progressToken").filter(|t| t.is_string() || t.is_number()).cloned();
                    if token.is_none() {
                        eprintln!("zen-engine mcp-bridge: tools/call without _meta.progressToken; long calls can't report progress (did Claude Code change?)");
                    }
                    match with_progress(ask(&socket, &req), token, &out, PROGRESS_EVERY).await {
                        Ok(v) => {
                            let is_error = v["is_error"].as_bool().unwrap_or(false);
                            let result = serde_json::from_value::<zen_proto::worker::ToolResult>(v).unwrap_or_default();
                            json!({ "result": { "content": result.blocks(), "isError": is_error } })
                        }
                        Err(e) => json!({ "result": { "content": [{ "type": "text", "text": e.to_string() }], "isError": true } }),
                    }
                }
                _ => json!({ "error": { "code": -32601, "message": format!("method not found: {method}") } }),
            };
            reply["jsonrpc"] = json!("2.0");
            reply["id"] = id;
            send(&out, &reply).await;
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(out: &Mutex<Vec<u8>>) -> Vec<Value> {
        let buf = out.try_lock().unwrap().clone();
        String::from_utf8(buf).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[tokio::test]
    async fn a_running_call_reports_progress_with_its_token_until_it_ends() {
        let out = Mutex::new(Vec::new());
        let call = async {
            tokio::time::sleep(Duration::from_millis(230)).await;
            "done"
        };
        let result = with_progress(call, Some(json!(7)), &out, Duration::from_millis(50)).await;
        assert_eq!(result, "done");
        let sent = lines(&out);
        assert!((3..=5).contains(&sent.len()), "{sent:?}");
        let mut last = 0;
        for n in &sent {
            assert_eq!(n["method"], "notifications/progress");
            assert_eq!(n["params"]["progressToken"], 7);
            let p = n["params"]["progress"].as_u64().unwrap();
            assert!(p > last, "progress must increase: {sent:?}");
            last = p;
        }
        // Nothing more once the call has answered.
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(lines(&out).len(), sent.len());
    }

    #[tokio::test]
    async fn quick_calls_and_calls_without_a_token_send_nothing() {
        let out = Mutex::new(Vec::new());
        assert_eq!(with_progress(async { 1 }, Some(json!("t")), &out, Duration::from_millis(50)).await, 1);
        let slow = async {
            tokio::time::sleep(Duration::from_millis(120)).await;
            2
        };
        assert_eq!(with_progress(slow, None, &out, Duration::from_millis(20)).await, 2);
        assert!(lines(&out).is_empty());
    }
}
