//! HTTP + WebSocket client for the zenbot kernel.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::IsTerminal;

pub struct Client {
    pub http: reqwest::Client,
    pub url: String,
    pub token: String,
}

impl Client {
    pub fn new(url: String, token: Option<String>) -> Result<Self> {
        let token = match token {
            Some(t) => t,
            None => {
                let home = std::env::var("HOME").context("HOME not set")?;
                std::fs::read_to_string(format!("{home}/.zenbot/token"))
                    .context("no token: set ZEN_TOKEN or create ~/.zenbot/token")?
                    .trim()
                    .to_string()
            }
        };
        Ok(Client { http: reqwest::Client::new(), url: url.trim_end_matches('/').to_string(), token })
    }

    pub async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self.http.request(method, format!("{}{}", self.url, path)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.with_context(|| format!("cannot reach zenbot at {}", self.url))?;
        let status = res.status();
        let data: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{}: {}", status, data["error"].as_str().unwrap_or("request failed"));
        }
        Ok(data)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.call(reqwest::Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.call(reqwest::Method::PATCH, path, Some(body)).await
    }

    /// Resolve a full id or a unique prefix, searching active and archived sessions.
    pub async fn resolve(&self, id: &str) -> Result<String> {
        let mut all = self.get("/api/sessions?archived=false").await?.as_array().cloned().unwrap_or_default();
        all.extend(self.get("/api/sessions?archived=true").await?.as_array().cloned().unwrap_or_default());
        let matches: Vec<&Value> = all.iter().filter(|s| s["id"].as_str().is_some_and(|x| x.starts_with(id))).collect();
        match matches.len() {
            1 => Ok(matches[0]["id"].as_str().unwrap().to_string()),
            0 => bail!("no session matches `{id}`"),
            n => bail!("`{id}` matches {n} sessions; use more characters"),
        }
    }

    pub async fn new_session(&self, model: Option<String>) -> Result<String> {
        let s = self.post("/api/sessions", json!({ "model": model })).await?;
        Ok(s["id"].as_str().context("bad session response")?.to_string())
    }

    pub async fn connect(&self, id: &str) -> Result<Ws> {
        let ws_url = format!("{}/api/sessions/{id}/ws?token={}", self.url.replacen("http", "ws", 1), self.token);
        let (ws, _) = tokio_tungstenite::connect_async(ws_url).await.context("opening session stream")?;
        Ok(ws)
    }
}

pub type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

pub fn dim(s: &str) -> String {
    if std::io::stderr().is_terminal() {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn tool_summary(name: &str, args: &Value) -> String {
    let detail = args["command"]
        .as_str()
        .or(args["path"].as_str())
        .map(str::to_string)
        .or_else(|| args["from"].as_str().map(|f| format!("{f} → {}", args["to"].as_str().unwrap_or(""))))
        .unwrap_or_else(|| args.to_string());
    let detail: String = detail.lines().next().unwrap_or("").chars().take(120).collect();
    format!("{name} {detail}")
}

