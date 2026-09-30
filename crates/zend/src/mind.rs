//! Connection to the zen-mind worker: JSON-RPC 2.0, one JSON object per line over stdio.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

/// A message initiated by the worker: a request (has id) or a notification.
pub struct Incoming {
    pub id: Option<Value>,
    pub method: String,
    pub params: Value,
}

pub struct Mind {
    stdin: Mutex<ChildStdin>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
}

impl Mind {
    pub async fn spawn(command: &str, dir: &str) -> Result<(Arc<Mind>, mpsc::UnboundedReceiver<Incoming>)> {
        let mut child = Command::new("bash")
            .arg("-lc")
            .arg(command)
            .current_dir(dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawning zen-mind")?;
        let stdin = child.stdin.take().context("mind stdin")?;
        let stdout = child.stdout.take().context("mind stdout")?;
        let mind = Arc::new(Mind { stdin: Mutex::new(stdin), pending: Mutex::new(HashMap::new()), next_id: AtomicU64::new(1) });
        let (tx, rx) = mpsc::unbounded_channel();

        let reader_mind = mind.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    tracing::warn!("mind sent invalid json: {line}");
                    continue;
                };
                if let Some(method) = msg.get("method").and_then(Value::as_str) {
                    let _ = tx.send(Incoming {
                        id: msg.get("id").cloned(),
                        method: method.to_string(),
                        params: msg.get("params").cloned().unwrap_or(Value::Null),
                    });
                } else if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                    if let Some(waiter) = reader_mind.pending.lock().await.remove(&id) {
                        let res = match msg.get("error") {
                            Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = waiter.send(res);
                    }
                }
            }
            tracing::error!("zen-mind exited");
        });
        tokio::spawn(async move {
            let status = child.wait().await;
            tracing::error!("zen-mind process ended: {status:?}");
        });
        Ok((mind, rx))
    }

    async fn write(&self, msg: Value) -> Result<()> {
        let mut line = serde_json::to_vec(&msg)?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(res)) => res.map_err(|e| anyhow!(e)),
            Ok(Err(_)) => Err(anyhow!("mind dropped request")),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(anyhow!("mind request `{method}` timed out"))
            }
        }
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.write(json!({ "jsonrpc": "2.0", "id": id, "result": result })).await
    }
}
