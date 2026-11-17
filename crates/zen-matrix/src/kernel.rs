//! The kernel, as the bridge sees it: a client of the client protocol (MAP.md, "Client protocol"),
//! like `zen` and the web app, through the same client (`zen-client`: the token in a header,
//! never a URL; timeouts; path encoding; the plain-http warning).

use anyhow::Result;
use serde_json::{json, Value};
use zen_client::{commands, enc, Client};

pub use zen_client::Ws;

#[derive(Clone)]
pub struct Kernel {
    c: Client,
}

impl Kernel {
    pub fn new(url: &str, token: String) -> Result<Self> {
        Ok(Kernel { c: Client::new(url, Some(token))? })
    }

    /// A new session, with `agent` (zenbot when None); its title is the owner's when given (the
    /// kernel won't rename it).
    pub async fn new_session(&self, title: Option<&str>, agent: Option<&str>) -> Result<String> {
        self.c.new_session(json!({ "title": title, "agent": agent })).await
    }

    /// A channel session for the main room: kept open by the bridge, ended by archive.
    pub async fn new_channel(&self) -> Result<String> {
        self.c.new_session(json!({ "title": "Matrix", "kind": "channel" })).await
    }

    /// Replace archived or missing channels, but never turn an auth/server/transport error into
    /// a new session. Inspect the HTTP status, not the client's human-readable error text.
    pub async fn needs_replacement(&self, id: &str) -> Result<bool> {
        let res = self.c.http.get(format!("{}/api/sessions/{}?last=0", self.c.url, enc(id)))
            .bearer_auth(&self.c.token).send().await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(true);
        }
        let v: Value = res.error_for_status()?.json().await?;
        Ok(v["archived"] == true)
    }

    /// Recent job runs, newest first (GET /api/jobs/runs).
    pub async fn job_runs(&self, limit: u32) -> Result<Vec<Value>> {
        let v = self.c.get(&format!("/api/jobs/runs?limit={limit}")).await?;
        Ok(v.as_array().cloned().or_else(|| v["runs"].as_array().cloned()).unwrap_or_default())
    }

    /// The session's messages after `seq` (each with its `seq`), oldest first, and whether a turn
    /// is running (GET /api/sessions/{id}?after=).
    pub async fn messages_after(&self, id: &str, seq: i64) -> Result<(Vec<Value>, bool)> {
        let v = self.c.get(&format!("/api/sessions/{}?after={seq}", enc(id))).await?;
        Ok((v["messages"].as_array().cloned().unwrap_or_default(), v["busy"] == true))
    }

    /// The `seq` of the session's latest message (0 when it has none).
    pub async fn last_seq(&self, id: &str) -> Result<i64> {
        let v = self.c.get(&format!("/api/sessions/{}?last=1", enc(id))).await?;
        Ok(v["messages"].as_array().and_then(|m| m.last()).and_then(|m| m["seq"].as_i64()).unwrap_or(0))
    }

    /// A shared command (`!jobs …`, `!model …`; zen_client::commands) run against the kernel for
    /// the room's `session`: its answer as Markdown, or the usage when it doesn't fit.
    pub async fn command(&self, text: &str, session: Option<&str>) -> Result<String> {
        match commands::parse(text) {
            Some(Ok(cmd)) => match cmd {
                commands::Cmd::McpLogin(_) => Ok("Sign in to an MCP server from the terminal: `zen mcp login <server>`.".into()),
                cmd => Ok(commands::run(&self.c, &cmd, session).await?.text),
            },
            Some(Err(usage)) => Ok(format!("`!{}`", usage.trim_start_matches("usage: "))),
            None => Ok("Unknown command: `!help` lists them.".into()),
        }
    }

    /// The session's event stream.
    pub async fn connect(&self, id: &str) -> Result<Ws> {
        self.c.connect(id).await
    }
}

/// A session id the kernel can route: a UUID's characters only, so it can't change the path.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercise the real HTTP client against a local server, including failure status codes.
    async fn replacement_response(status: &str, body: &str) -> Result<bool> {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /api/sessions/abc?last=0 "));
            assert!(request.to_lowercase().contains("authorization: bearer test-token"));
            stream.write_all(response.as_bytes()).unwrap();
        });
        let result = Kernel::new(&url, "test-token".into())?.needs_replacement("abc").await;
        server.join().unwrap();
        result
    }

    #[tokio::test]
    async fn missing_or_archived_channels_are_replaced() {
        assert!(replacement_response("404 Not Found", r#"{"error":"session not found"}"#).await.unwrap());
        assert!(replacement_response("200 OK", r#"{"archived":true}"#).await.unwrap());
        assert!(!replacement_response("200 OK", r#"{"archived":false}"#).await.unwrap());
    }

    #[tokio::test]
    async fn other_failures_do_not_replace_channels() {
        for status in ["401 Unauthorized", "403 Forbidden", "500 Internal Server Error"] {
            assert!(replacement_response(status, r#"{"error":"failed"}"#).await.is_err());
        }
        assert!(replacement_response("200 OK", "not json").await.is_err());
    }

    #[test]
    fn only_uuid_like_ids_are_routed() {
        assert!(valid_id("0f9e3c1a-7b2d-4c1e-9a8b-1234567890ab"));
        assert!(!valid_id("../memory"));
        assert!(!valid_id(""));
    }
}
