//! The kernel client as `zen` uses it: the shared one (`zen-client`: token, calls, the session
//! stream, path encoding) plus what only the terminal app needs (upgrades, display helpers).

use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::IsTerminal;

pub use zen_client::commands::{describe_update, short};
pub use zen_client::{enc, zen_home, Client, Ws};

/// Settings for a session created from the command line; unset means the kernel's default.
#[derive(Default, Clone)]
pub struct NewSession {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The agent a new session is (agents.rs); zenbot when None.
    pub agent: Option<String>,
}

impl NewSession {
    /// The body of POST /api/sessions.
    pub fn body(&self) -> Value {
        json!({ "model": self.model, "effort": self.effort, "agent": self.agent })
    }
}

/// Update the kernel to the latest main, reporting progress line by line. Returns the final
/// message. Survives the kernel restarting underneath it, and reports a rollback as an error.
pub async fn upgrade(c: &Client, mut progress: impl FnMut(String)) -> Result<String> {
    let v = c.get("/api/version?refresh=true").await?;
    if let Some(e) = v["error"].as_str() {
        bail!("could not check for updates: {e}");
    }
    if v["available"] != true {
        return Ok(describe_update(&v));
    }
    progress(describe_update(&v));
    for commit in v["commits"].as_array().into_iter().flatten() {
        progress(format!("  · {}", commit.as_str().unwrap_or("")));
    }
    let target = v["latest"].as_str().unwrap_or("").to_string();
    let started = c.post("/api/upgrade", json!({})).await?;
    let started_at = started["started_at"].as_str().unwrap_or("").to_string();

    // 1. Pull, build or download, check, smoke test (scripts/self-update.sh).
    // When no session is busy the restart can come within seconds, even before we've read the
    // job's final status: an unreachable kernel, or a new one with no job, means it's restarting.
    let mut shown = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let Ok(st) = c.get("/api/upgrade").await else { break };
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
        let Ok(health) = c.health().await else { continue };
        let commit = health["commit"].as_str().unwrap_or("");
        if !target.is_empty() && commit.starts_with(&target) && health["ok"] == true {
            return Ok(format!("zenbot upgraded to {target}"));
        }
        if let Ok(st) = c.get("/api/upgrade").await {
            let last = st["last_result"].as_str().unwrap_or("");
            if last.get(..20).is_some_and(|t| t >= started_at.as_str()) && (last.contains("FAILED") || last.contains("rolled back")) {
                bail!("upgrade rolled back: {last} (details in ~/.zenbot/upgrade.log)");
            }
        }
    }
    bail!("zenbot hasn't restarted after 30 minutes (a session may still be working); check ~/.zenbot/upgrade.log")
}

/// `s` dimmed when stderr is a terminal, where it's printed.
pub fn dim(s: &str) -> String {
    if std::io::stderr().is_terminal() {
        format!("\x1b[2m{}\x1b[0m", crate::print::sanitize(s))
    } else {
        s.to_string()
    }
}

/// Tokens of one model call, from an assistant message's `usage` (cache reads and writes included).
pub fn usage_total(u: &Value) -> i64 {
    ["input", "output", "cacheRead", "cacheWrite"].iter().map(|k| u[*k].as_i64().unwrap_or(0)).sum()
}

/// Tokens of a whole turn, from the kernel's record of it (side calls included).
pub fn record_total(r: &Value) -> i64 {
    ["input_tokens", "output_tokens", "cache_read", "cache_write"].iter().map(|k| r[*k].as_i64().unwrap_or(0)).sum()
}

/// An assistant message's text blocks, joined (its tool calls left out).
pub fn assistant_text(m: &Value) -> String {
    m["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect()
}

pub fn tool_summary(name: &str, args: &Value) -> String {
    let detail = args["command"]
        .as_str()
        .or(args["path"].as_str())
        .map(str::to_string)
        .or_else(|| args["from"].as_str().map(|f| format!("{f} → {}", args["to"].as_str().unwrap_or(""))))
        .unwrap_or_else(|| args.to_string());
    let detail: String = detail.lines().next().unwrap_or("").chars().take(120).collect();
    crate::print::sanitize(&format!("{name} {detail}")).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_totals_and_assistant_text() {
        assert_eq!(usage_total(&json!({ "input": 10, "output": 5, "cacheRead": 100, "cacheWrite": 1 })), 116);
        assert_eq!(record_total(&json!({ "input_tokens": 1000, "output_tokens": 200, "cache_read": 3000 })), 4200);
        let m = json!({ "content": [{ "type": "text", "text": "a" }, { "type": "toolCall", "name": "x" }, { "type": "text", "text": "b" }] });
        assert_eq!(assistant_text(&m), "ab");
    }
}

