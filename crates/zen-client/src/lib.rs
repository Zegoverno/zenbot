//! The kernel, as its clients reach it (docs/client-protocol.md): zenbot's home, the env files
//! there, the token, HTTP calls and a session's event stream. `zen` and `zen-matrix` both use this,
//! so they authenticate, time out, encode paths and warn about plain http the same way. The
//! events themselves are typed in `zen_proto::client`.
//!
//! The token goes in the `Authorization` header, never a URL, where it could end up in logs.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// zenbot's home: `ZEN_HOME`, else `~/.zenbot` (as the kernel decides it). The token, the prompt
/// history, the installed binaries, `env`, `matrix.env` and `engines.json` live there.
pub fn zen_home() -> PathBuf {
    home_from(std::env::var_os("ZEN_HOME"), std::env::var_os("HOME"))
}

fn home_from(zen_home: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> PathBuf {
    match zen_home.filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(home.unwrap_or_default()).join(".zenbot"),
    }
}

/// `KEY=value` lines as a shell would read them, as `~/.zenbot/env` and `~/.zenbot/matrix.env`
/// are written: comments and blank lines skipped, an `export ` prefix and matching quotes around
/// the value removed, lines without `=` ignored.
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("export ").unwrap_or(l).split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let unquoted = [('"', '"'), ('\'', '\'')].iter().find_map(|(a, b)| v.strip_prefix(*a)?.strip_suffix(*b));
            (k.trim().to_string(), unquoted.unwrap_or(v).to_string())
        })
        .collect()
}

/// How long to wait for the kernel to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest a single API call, or opening the session stream, may take. Generous: a version check
/// fetches from GitHub (the kernel gives git 60 seconds). The open stream itself has no limit.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// A session's event stream.
pub type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub struct Client {
    pub http: reqwest::Client,
    pub url: String,
    pub token: String,
}

impl Client {
    /// A client of the kernel at `url`; without a token, the one in `<zen home>/token`. Warns on
    /// stderr when the token would cross the network in clear text.
    pub fn new(url: &str, token: Option<String>) -> Result<Self> {
        let token = match token {
            Some(t) => t,
            None => {
                let path = zen_home().join("token");
                std::fs::read_to_string(&path).with_context(|| format!("no token: set ZEN_TOKEN or create {}", path.display()))?.trim().to_string()
            }
        };
        if let Some(host) = cleartext_remote(url) {
            eprintln!("warning: the zenbot token goes to {host} unencrypted (plain http); use https or an SSH tunnel");
        }
        let http = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).timeout(REQUEST_TIMEOUT).build()?;
        Ok(Client { http, url: url.trim_end_matches('/').to_string(), token })
    }

    /// One API call with the token; a non-2xx answer is an error carrying the kernel's `error`.
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

    pub async fn delete(&self, path: &str) -> Result<Value> {
        self.call(reqwest::Method::DELETE, path, None).await
    }

    /// The kernel's health report (GET /health, no token needed).
    pub async fn health(&self) -> Result<Value> {
        let res = self.http.get(format!("{}/health", self.url)).send().await.with_context(|| format!("cannot reach zenbot at {}", self.url))?;
        Ok(res.json().await?)
    }

    /// A new session (POST /api/sessions with `body`: `title`, `model`, `effort`, each optional);
    /// its id.
    pub async fn new_session(&self, body: Value) -> Result<String> {
        let s = self.post("/api/sessions", body).await?;
        Ok(s["id"].as_str().context("bad session response")?.to_string())
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

    /// Open session `id`'s event stream.
    pub async fn connect(&self, id: &str) -> Result<Ws> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue};
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
    fn zen_home_follows_zen_home_then_home() {
        assert_eq!(home_from(Some("/srv/zen".into()), Some("/home/x".into())), PathBuf::from("/srv/zen"));
        assert_eq!(home_from(None, Some("/home/x".into())), PathBuf::from("/home/x/.zenbot"));
        assert_eq!(home_from(Some("".into()), Some("/home/x".into())), PathBuf::from("/home/x/.zenbot"), "empty means unset");
    }

    #[test]
    fn env_files_are_read_like_a_shell_would() {
        let env = parse_env("# zenbot\nexport PATH=\"/a/bin:/b\"\nZEN_REPO='/home/x/zenbot'\n\nPLAIN = v \nBAD\nexport MATRIX_PASSWORD=\"p=ss word\"\n");
        assert_eq!(env["PATH"], "/a/bin:/b");
        assert_eq!(env["ZEN_REPO"], "/home/x/zenbot");
        assert_eq!(env["PLAIN"], "v");
        assert_eq!(env["MATRIX_PASSWORD"], "p=ss word");
        assert_eq!(env.len(), 4);
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
        let c = Client::new(&url, Some("s3cret+/=".into())).unwrap();
        let _ = c.connect("abc/../x").await; // the fake server hangs up without a handshake
        let req = server.await.unwrap();
        assert!(req.starts_with("GET /api/sessions/abc%2F..%2Fx/ws HTTP/1.1\r\n"), "{req}");
        assert!(req.lines().any(|l| l.eq_ignore_ascii_case("authorization: Bearer s3cret+/=")), "{req}");
    }
}
