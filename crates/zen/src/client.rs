//! HTTP + WebSocket client for the zenbot kernel.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

/// zenbot's home: `ZEN_HOME`, else `~/.zenbot` (as the kernel decides it). The token, the prompt
/// history, the installed binaries, `env` and `engines.json` live there.
pub fn zen_home() -> PathBuf {
    home_from(std::env::var_os("ZEN_HOME"), std::env::var_os("HOME"))
}

fn home_from(zen_home: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> PathBuf {
    match zen_home.filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(home.unwrap_or_default()).join(".zenbot"),
    }
}

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
                let path = zen_home().join("token");
                std::fs::read_to_string(&path).with_context(|| format!("no token: set ZEN_TOKEN or create {}", path.display()))?.trim().to_string()
            }
        };
        if let Some(host) = cleartext_remote(&url) {
            eprintln!("warning: the zenbot token goes to {host} unencrypted (plain http); use https or an SSH tunnel");
        }
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
        let mut req = format!("{}/api/sessions/{}/ws", self.url.replacen("http", "ws", 1), enc(id)).into_client_request()?;
        req.headers_mut().insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", self.token)).context("token is not a valid header value")?);
        let (ws, _) = tokio::time::timeout(REQUEST_TIMEOUT, tokio_tungstenite::connect_async(req))
            .await
            .context("opening session stream: timed out")?
            .context("opening session stream")?;
        Ok(ws)
    }
}

/// Text as one URL path segment or query value: everything but unreserved characters
/// (RFC 3986: letters, digits, `-._~`) percent-encoded, so a `/`, `?` or `#` in a name typed on
/// the command line can't change which route it reaches.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The host a plain-http URL points to, when it isn't this machine: the token would cross the
/// network in clear text.
fn cleartext_remote(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    if u.scheme() != "http" {
        return None;
    }
    let host = u.host_str()?;
    let ip = host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>();
    let local = host == "localhost" || host.ends_with(".localhost") || ip.is_ok_and(|ip| ip.is_loopback());
    (!local).then(|| host.to_string())
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

/// The first 8 bytes of an id (all of it if shorter, or if 8 bytes would split a character).
pub fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

pub fn dim(s: &str) -> String {
    if std::io::stderr().is_terminal() {
        format!("\x1b[2m{}\x1b[0m", crate::md::sanitize(s))
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
    crate::md::sanitize(&format!("{name} {detail}")).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(enc("web_fetch-2.x~"), "web_fetch-2.x~");
        assert_eq!(enc("../skills?x=1#y z/é"), "..%2Fskills%3Fx%3D1%23y%20z%2F%C3%A9");
    }

    #[test]
    fn plain_http_is_flagged_only_off_this_machine() {
        for local in ["http://127.0.0.1:8100", "http://localhost:8100", "http://[::1]:8100", "https://zen.example.com", "http://127.0.0.5"] {
            assert_eq!(cleartext_remote(local), None, "{local}");
        }
        assert_eq!(cleartext_remote("http://10.0.0.7:8100").as_deref(), Some("10.0.0.7"));
        assert_eq!(cleartext_remote("http://zen.example.com").as_deref(), Some("zen.example.com"));
    }

    #[test]
    fn short_ids_never_split_a_character() {
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short("ééééé"), "éééé");
        assert_eq!(short("1234567é"), "1234567é", "byte 8 is inside é: the whole id");
    }

    #[test]
    fn zen_home_follows_zen_home_then_home() {
        assert_eq!(home_from(Some("/srv/zen".into()), Some("/home/x".into())), PathBuf::from("/srv/zen"));
        assert_eq!(home_from(None, Some("/home/x".into())), PathBuf::from("/home/x/.zenbot"));
        assert_eq!(home_from(Some("".into()), Some("/home/x".into())), PathBuf::from("/home/x/.zenbot"), "empty means unset");
    }

    #[test]
    fn token_totals_and_assistant_text() {
        assert_eq!(usage_total(&json!({ "input": 10, "output": 5, "cacheRead": 100, "cacheWrite": 1 })), 116);
        assert_eq!(record_total(&json!({ "input_tokens": 1000, "output_tokens": 200, "cache_read": 3000 })), 4200);
        let m = json!({ "content": [{ "type": "text", "text": "a" }, { "type": "toolCall", "name": "x" }, { "type": "text", "text": "b" }] });
        assert_eq!(assistant_text(&m), "ab");
    }

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

