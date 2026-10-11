//! Connection to a model worker: JSON-RPC 2.0, one JSON object per line over stdio. Requests are
//! `zen_proto::worker::Request`s; what the worker sends is read in `dispatch.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use zen_proto::worker::Request;

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
        // Not a login shell: a profile could put back what is removed below.
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(command).current_dir(dir);
        for name in scrubbed(std::env::vars_os().filter_map(|(k, _)| k.into_string().ok())) {
            cmd.env_remove(name);
        }
        let mut child = cmd
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

    /// Send a request (docs/worker-protocol.md) and wait up to 30 seconds for its answer.
    pub async fn call(&self, req: &Request) -> Result<Value> {
        self.call_within(req, std::time::Duration::from_secs(30)).await
    }

    /// A request that may take longer than the default 30 seconds (e.g. `complete`), or must take less.
    pub async fn call_within(&self, req: &Request, limit: std::time::Duration) -> Result<Value> {
        self.request_within(req.method(), req.params(), limit).await
    }

    async fn request_within(&self, method: &str, params: Value, limit: std::time::Duration) -> Result<Value> {
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

    /// Answer a worker's request (`tool.call`).
    pub async fn respond<T: serde::Serialize>(&self, id: Value, result: &T) -> Result<()> {
        self.write(json!({ "jsonrpc": "2.0", "id": id, "result": serde_json::to_value(result)? })).await
    }
}

/// Engine sign-ins a worker may need, though named like secrets: they stay in its environment.
const ENGINE_SIGN_INS: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"];

/// The variables removed from a worker's environment: every secret-looking one of the kernel's
/// (`secrets::is_secret_var`: its token, database, web search and OpenRouter keys, any other
/// credential) but the engines' sign-ins. Workers run model CLIs, not the kernel's tools.
fn scrubbed(names: impl Iterator<Item = String>) -> Vec<String> {
    names.filter(|n| crate::secrets::is_secret_var(n) && !ENGINE_SIGN_INS.contains(&n.as_str())).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_get_no_secrets_but_engine_sign_ins() {
        let names = ["ZEN_TOKEN", "DATABASE_URL", "OPENROUTER_API_KEY", "GITHUB_TOKEN", "MY_SECRET", "ANTHROPIC_API_KEY", "CODEX_ACCESS_TOKEN", "PATH", "HOME", "ZEN_FAUX"];
        assert_eq!(scrubbed(names.iter().map(|n| n.to_string())), ["ZEN_TOKEN", "DATABASE_URL", "OPENROUTER_API_KEY", "GITHUB_TOKEN", "MY_SECRET"]);
    }
}
