//! The kernel, as the bridge sees it: a client of the client protocol (docs/client-protocol.md),
//! like the `zen` terminal app, through the same client (`zen-client`: the token in a header,
//! never a URL; timeouts; path encoding; the plain-http warning).

use anyhow::Result;
use serde_json::{json, Value};
use zen_client::{enc, Client};

pub use zen_client::Ws;

#[derive(Clone)]
pub struct Kernel {
    c: Client,
}

impl Kernel {
    pub fn new(url: &str, token: String) -> Result<Self> {
        Ok(Kernel { c: Client::new(url, Some(token))? })
    }

    /// A new session; its title is the owner's when given (the kernel won't rename it).
    pub async fn new_session(&self, title: Option<&str>) -> Result<String> {
        self.c.new_session(json!({ "title": title })).await
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

    /// An owner command (`!jobs …`, `!skills …`, `!tools …`) run against the kernel: its answer as
    /// Markdown, or the usage when it doesn't fit.
    pub async fn owner(&self, text: &str) -> Result<String> {
        match zen_client::owner::parse(text) {
            Some(Ok(cmd)) => zen_client::owner::run(&self.c, &cmd).await,
            Some(Err(usage)) => Ok(format!("`{usage}`")),
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

    #[test]
    fn only_uuid_like_ids_are_routed() {
        assert!(valid_id("0f9e3c1a-7b2d-4c1e-9a8b-1234567890ab"));
        assert!(!valid_id("../memory"));
        assert!(!valid_id(""));
    }
}
