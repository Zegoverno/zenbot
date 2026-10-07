//! HTTP + WebSocket client for the zenbot kernel.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::IsTerminal;
use std::time::Duration;

/// How long to wait for the kernel to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest a single API call, or opening the session stream, may take. Generous: a version check
/// fetches from GitHub (the kernel gives git 60 seconds). The open stream itself has no limit.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
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
        let http = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).timeout(REQUEST_TIMEOUT).build()?;
        Ok(Client { http, url: url.trim_end_matches('/').to_string(), token })
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

    /// The kernel's health report (GET /health, no token needed).
    pub async fn health(&self) -> Result<Value> {
        let res = self.http.get(format!("{}/health", self.url)).send().await.with_context(|| format!("cannot reach zenbot at {}", self.url))?;
        Ok(res.json().await?)
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

    pub async fn new_session(&self, model: Option<String>, effort: Option<String>) -> Result<String> {
        let s = self.post("/api/sessions", json!({ "model": model, "effort": effort })).await?;
        Ok(s["id"].as_str().context("bad session response")?.to_string())
    }

    pub async fn connect(&self, id: &str) -> Result<Ws> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue};
        // The token goes in a header, not the URL, where it could end up in logs.
        let mut req = format!("{}/api/sessions/{id}/ws", self.url.replacen("http", "ws", 1)).into_client_request()?;
        req.headers_mut().insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", self.token)).context("token is not a valid header value")?);
        let (ws, _) = tokio::time::timeout(REQUEST_TIMEOUT, tokio_tungstenite::connect_async(req))
            .await
            .context("opening session stream: timed out")?
            .context("opening session stream")?;
        Ok(ws)
    }
}

/// Settings for a session created from the command line; unset means the kernel's default.
#[derive(Default, Clone)]
pub struct NewSession {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// One line summarizing a version check (GET /api/version).
pub fn describe_update(v: &Value) -> String {
    if let Some(e) = v["error"].as_str() {
        return format!("zenbot {} · could not check for updates: {e}", v["running"].as_str().unwrap_or("?"));
    }
    if v["available"] == true {
        let how = if v["prebuilt_ready"] == true { "prebuilt, quick" } else { "will compile on this machine, takes a few minutes" };
        format!(
            "zenbot update available: {} → {} ({} new commit{}; {how})",
            v["running"].as_str().unwrap_or("?"),
            v["latest"].as_str().unwrap_or("?"),
            v["behind"],
            if v["behind"] == 1 { "" } else { "s" }
        )
    } else {
        format!("zenbot {} is up to date", v["running"].as_str().unwrap_or("?"))
    }
}

impl Client {
    /// Update the kernel to the latest main, reporting progress line by line. Returns the final
    /// message. Survives the kernel restarting underneath it, and reports a rollback as an error.
    pub async fn upgrade(&self, mut progress: impl FnMut(String)) -> Result<String> {
        let v = self.get("/api/version?refresh=true").await?;
        if let Some(e) = v["error"].as_str() {
            bail!("could not check for updates: {e}");
        }
        if v["available"] != true {
            return Ok(describe_update(&v));
        }
        progress(describe_update(&v));
        for c in v["commits"].as_array().into_iter().flatten() {
            progress(format!("  · {}", c.as_str().unwrap_or("")));
        }
        let target = v["latest"].as_str().unwrap_or("").to_string();
        let started = self.post("/api/upgrade", json!({})).await?;
        let started_at = started["started_at"].as_str().unwrap_or("").to_string();

        // 1. Pull, build or download, check, smoke test (scripts/self-update.sh).
        // When no session is busy the restart can come within seconds, even before we've read the
        // job's final status: an unreachable kernel, or a new one with no job, means it's restarting.
        let mut shown = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let Ok(st) = self.get("/api/upgrade").await else { break };
            let log = st["job"]["log"].as_str().unwrap_or("");
            for l in log.lines().skip(shown) {
                progress(l.to_string());
            }
            shown = log.lines().count();
            match st["job"]["status"].as_str() {
                Some("running") => continue,
                Some("scheduled") | None => break,
                _ => bail!("upgrade failed; nothing was changed"),
            }
        }

        // 2. upgrade.sh restarts zenbot once no session is working, then health-checks it.
        progress("restarting zenbot as soon as no session is working…".into());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30 * 60);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let Ok(health) = self.health().await else { continue };
            let commit = health["commit"].as_str().unwrap_or("");
            if !target.is_empty() && commit.starts_with(&target) && health["ok"] == true {
                return Ok(format!("zenbot upgraded to {target}"));
            }
            if let Ok(st) = self.get("/api/upgrade").await {
                let last = st["last_result"].as_str().unwrap_or("");
                if last.get(..20).is_some_and(|t| t >= started_at.as_str()) && (last.contains("FAILED") || last.contains("rolled back")) {
                    bail!("upgrade rolled back: {last} (details in ~/.zenbot/upgrade.log)");
                }
            }
        }
        bail!("zenbot hasn't restarted after 30 minutes (a session may still be working); check ~/.zenbot/upgrade.log")
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
    crate::md::sanitize(&format!("{name} {detail}")).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn the_session_stream_sends_the_token_in_a_header_not_the_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        let c = Client::new(url, Some("s3cret+/=".into())).unwrap();
        let _ = c.connect("abc").await; // the fake server hangs up without a handshake
        let req = server.await.unwrap();
        assert!(req.starts_with("GET /api/sessions/abc/ws HTTP/1.1\r\n"), "{req}");
        assert!(req.lines().any(|l| l.eq_ignore_ascii_case("authorization: Bearer s3cret+/=")), "{req}");
    }
}

