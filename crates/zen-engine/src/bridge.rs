//! MCP bridge. Claude Code starts `zen-engine mcp-bridge <socket>` as a stdio MCP server;
//! the bridge answers tools/list and tools/call by asking the engine over a Unix socket,
//! and the engine forwards each call to the kernel, which executes it.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

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
            let reply = match method.as_str() {
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
                    match ask(&socket, &req).await {
                        Ok(v) => json!({ "result": {
                            "content": [{ "type": "text", "text": v["content"].as_str().unwrap_or("") }],
                            "isError": v["is_error"].as_bool().unwrap_or(false)
                        }}),
                        Err(e) => json!({ "result": { "content": [{ "type": "text", "text": e.to_string() }], "isError": true } }),
                    }
                }
                _ => json!({ "error": { "code": -32601, "message": format!("method not found: {method}") } }),
            };
            let mut reply = reply;
            reply["jsonrpc"] = json!("2.0");
            reply["id"] = id;
            let mut line = reply.to_string();
            line.push('\n');
            let mut o = out.lock().await;
            let _ = o.write_all(line.as_bytes()).await;
            let _ = o.flush().await;
        });
    }
    Ok(())
}
