//! Connection to a model worker: JSON-RPC 2.0, one JSON object per line over stdio.

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
    /// Requests waiting for their answer. A plain mutex: it is never held across an await, and a
    /// dropped request removes its own entry (`Waiting`), which needs a lock usable in `Drop`.
    pending: std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
}

impl Mind {
    /// Start a worker. The returned receiver fires when the process has exited; by then every
    /// request still waiting on it has failed.
    pub async fn spawn(command: &str, dir: &str) -> Result<(Arc<Mind>, mpsc::UnboundedReceiver<Incoming>, oneshot::Receiver<()>)> {
        let mut child = Command::new("bash")
            .arg("-lc")
            .arg(command)
            .current_dir(dir)
            // Workers run model CLIs, not the kernel's tools: they get none of the kernel's own
            // credentials (engine sign-ins such as ANTHROPIC_API_KEY pass through).
            .env_remove("ZEN_TOKEN")
            .env_remove("DATABASE_URL")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("BRAVE_API_KEY")
            .env_remove("TAVILY_API_KEY")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawning model worker")?;
        let command_owned = command.to_string();
        let stdin = child.stdin.take().context("mind stdin")?;
        let stdout = child.stdout.take().context("mind stdout")?;
        let mind = Arc::new(Mind { stdin: Mutex::new(stdin), pending: std::sync::Mutex::new(HashMap::new()), next_id: AtomicU64::new(1) });
        let (tx, rx) = mpsc::unbounded_channel();

        let reader_mind = mind.clone();
        tokio::spawn(async move {
            // Read bytes, not `String` lines: one invalid UTF-8 line must not stop the reader while
            // the process lives on (every request would then fail and nothing restarts it).
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end();
                if line.is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Value>(line) else {
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
                    if let Some(waiter) = reader_mind.pending.lock().unwrap().remove(&id) {
                        let res = match msg.get("error") {
                            Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = waiter.send(res);
                    }
                }
            }
            reader_mind.fail_pending().await;
        });
        let (exit_tx, exit_rx) = oneshot::channel();
        let waiter_mind = mind.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            tracing::error!("worker process `{command_owned}` ended: {status:?}");
            waiter_mind.fail_pending().await;
            let _ = exit_tx.send(());
        });
        Ok((mind, rx, exit_rx))
    }

    async fn fail_pending(&self) {
        let waiters: Vec<_> = self.pending.lock().unwrap().drain().collect();
        for (_, waiter) in waiters {
            let _ = waiter.send(Err("worker exited".into()));
        }
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
        self.request_within(method, params, std::time::Duration::from_secs(30)).await
    }

    /// A request that may take longer than the default 30 seconds (e.g. `complete`).
    pub async fn request_within(&self, method: &str, params: Value, limit: std::time::Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        // Removes the entry however this future ends: answered, timed out, failed or dropped
        // (a caller that stops waiting must not leak its entry).
        let _waiting = Waiting { mind: self, id };
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await.context("worker is not running")?;
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(res)) => res.map_err(|e| anyhow!(e)),
            Ok(Err(_)) => Err(anyhow!("mind dropped request")),
            Err(_) => Err(anyhow!("mind request `{method}` timed out")),
        }
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.write(json!({ "jsonrpc": "2.0", "id": id, "result": result })).await
    }
}

struct Waiting<'a> {
    mind: &'a Mind,
    id: u64,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.mind.pending.lock() {
            pending.remove(&self.id);
        }
    }
}
