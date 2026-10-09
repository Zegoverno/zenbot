//! The kernel, as the bridge sees it: a client of the client protocol (docs/client-protocol.md),
//! like the `zen` terminal app. The token goes in a header, never a URL.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub struct Kernel {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl Kernel {
    pub fn new(url: &str, token: String) -> Result<Self> {
        let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(60)).build()?;
        Ok(Kernel { http, url: url.trim_end_matches('/').to_string(), token })
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self.http.request(method, format!("{}{}", self.url, path)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.with_context(|| format!("cannot reach zenbot at {}", self.url))?;
        let status = res.status();
        let data: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{status}: {}", data["error"].as_str().unwrap_or("request failed"));
        }
        Ok(data)
    }

    /// A new session; its title is the owner's when given (the kernel won't rename it).
    pub async fn new_session(&self, title: Option<&str>) -> Result<String> {
        let s = self.call(reqwest::Method::POST, "/api/sessions", Some(json!({ "title": title }))).await?;
        Ok(s["id"].as_str().context("bad session response")?.to_string())
    }

    /// Recent job runs, newest first (GET /api/jobs/runs).
    pub async fn job_runs(&self, limit: u32) -> Result<Vec<Value>> {
        let v = self.call(reqwest::Method::GET, &format!("/api/jobs/runs?limit={limit}"), None).await?;
        Ok(v.as_array().cloned().or_else(|| v["runs"].as_array().cloned()).unwrap_or_default())
    }

    /// The session's event stream.
    pub async fn connect(&self, id: &str) -> Result<Ws> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue};
        let mut req = format!("{}/api/sessions/{id}/ws", self.url.replacen("http", "ws", 1)).into_client_request()?;
        req.headers_mut().insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", self.token)).context("token is not a valid header value")?);
        let (ws, _) = tokio::time::timeout(Duration::from_secs(30), tokio_tungstenite::connect_async(req)).await.context("opening session stream: timed out")??;
        Ok(ws)
    }
}

/// A session id the kernel can route: a UUID's characters only, so it can't change the path.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_uuid_like_ids_are_routed() {
        assert!(valid_id("0f9e3c1a-7b2d-4c1e-9a8b-1234567890ab"));
        assert!(!valid_id("../memory"));
        assert!(!valid_id(""));
    }
}
