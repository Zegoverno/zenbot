//! JSON-RPC 2.0 over stdio with the kernel: one JSON object per line.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::{oneshot, Mutex};

#[derive(Clone)]
pub struct Rpc {
    out: Arc<Mutex<tokio::io::Stdout>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    next: Arc<AtomicU64>,
}

impl Rpc {
    pub fn new() -> Self {
        Rpc { out: Arc::new(Mutex::new(tokio::io::stdout())), pending: Arc::default(), next: Arc::new(AtomicU64::new(1)) }
    }

    async fn write(&self, mut msg: Value) {
        msg["jsonrpc"] = json!("2.0");
        let mut line = msg.to_string();
        line.push('\n');
        let mut out = self.out.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.flush().await;
    }

    pub async fn notify(&self, method: &str, params: Value) {
        self.write(json!({ "method": method, "params": params })).await;
    }

    pub async fn respond(&self, id: Value, result: Result<Value, String>) {
        match result {
            Ok(r) => self.write(json!({ "id": id, "result": r })).await,
            Err(e) => self.write(json!({ "id": id, "error": { "code": -32000, "message": e } })).await,
        }
    }

    /// Ask the kernel something (e.g. `tool.call`) and wait for its answer.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write(json!({ "id": id, "method": method, "params": params })).await;
        rx.await.map_err(|_| anyhow!("kernel dropped request"))?.map_err(|e| anyhow!(e))
    }

    /// Route a response from the kernel to the waiting request.
    pub async fn resolve(&self, msg: &Value) {
        let Some(id) = msg["id"].as_u64() else { return };
        if let Some(tx) = self.pending.lock().await.remove(&id) {
            let res = match msg.get("error") {
                Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(res);
        }
    }
}
