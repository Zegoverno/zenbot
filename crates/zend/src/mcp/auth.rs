//! OAuth bearer tokens for remote MCP servers, from one of two sources:
//! - **login** (`zen mcp-login <server>`, login.rs): the owner signed in through a browser; the
//!   credentials (issuer, token endpoint, client, tokens) are bound to the server's URL as the
//!   protected resource, and a changed URL needs a new login;
//! - **provisioned**: `oauth` in mcp.json names `token_url`, `client_id` and a `refresh_token` the
//!   owner obtained elsewhere; the stored state is bound to those three.
//!
//! Either is refreshed here when it expires, rotated refresh tokens are persisted before the access
//! token is used, and nothing follows redirects or reports an endpoint's body (it can reflect the
//! submitted credentials). A server with neither gets no Authorization header.
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub struct OAuthConfig {
    /// Provisioned mode only.
    token_url: Option<String>,
    /// A client the owner registered by hand: provisioned mode, or used by a login instead of
    /// registering one.
    pub(super) client_id: Option<String>,
    refresh_token: Option<String>,
    pub(super) client_secret: Option<String>,
    /// Scopes a login asks for when the server doesn't say.
    pub(super) scope: Option<String>,
}

/// An absolute https URL (http only on loopback), without credentials or a fragment.
pub(super) fn checked_url(text: &str, what: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(text).with_context(|| format!("{what} must be an absolute URL"))?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    anyhow::ensure!(url.scheme() == "https" || (url.scheme() == "http" && local), "{what} requires HTTPS (HTTP is allowed on loopback only)");
    anyhow::ensure!(url.username().is_empty() && url.password().is_none() && url.fragment().is_none(), "{what} must not contain credentials or fragments");
    Ok(url)
}

impl OAuthConfig {
    pub fn parse(value: &Value, allow: &[String], missing: &mut Vec<String>) -> Result<Self> {
        let mut field = |key: &str| value[key].as_str().map(|s| super::expand(s, allow, missing)).filter(|s| !s.is_empty());
        let cfg = Self {
            token_url: field("token_url"),
            client_id: field("client_id"),
            refresh_token: field("refresh_token"),
            client_secret: field("client_secret"),
            scope: field("scope"),
        };
        if cfg.refresh_token.is_some() {
            let url = cfg.token_url.as_deref().context("oauth with a refresh_token needs token_url and client_id")?;
            checked_url(url, "oauth.token_url")?;
            anyhow::ensure!(cfg.client_id.is_some(), "oauth with a refresh_token needs token_url and client_id");
        }
        Ok(cfg)
    }

    fn provisioned(&self) -> bool { self.refresh_token.is_some() }
}

#[derive(Clone)]
pub(super) struct Token {
    value: Value,
}

pub(crate) fn directory() -> PathBuf { crate::zen_home().join("mcp-auth") }

fn path(server: &str) -> PathBuf { directory().join(format!("{server}.json")) }

pub(super) fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }

/// Whether stored credentials still belong to this server's configuration.
fn binding_matches(value: &Value, cfg: Option<&OAuthConfig>, resource: &str) -> bool {
    match cfg.filter(|c| c.provisioned()) {
        Some(cfg) => value["login"].is_null() && value["token_url"].as_str() == cfg.token_url.as_deref()
            && value["client_id"].as_str() == cfg.client_id.as_deref()
            && value["provisioned_refresh_token"].as_str() == cfg.refresh_token.as_deref(),
        None => value["login"] == true && value["resource"] == resource,
    }
}

pub(super) async fn expire(state: &Mutex<Option<Token>>) {
    if let Some(token) = &mut *state.lock().await { token.value["expires_at"] = json!(0); }
}

/// Forget what's cached, so the next request reads the file a login just wrote.
pub(super) async fn reset(state: &Mutex<Option<Token>>) {
    *state.lock().await = None;
}

/// Read the stored credentials (no symlink, at most 64 KB).
fn load(server: &str) -> Result<Option<Value>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path(server)) {
        Ok(mut f) => {
            anyhow::ensure!(f.metadata()?.len() <= 64 * 1024, "OAuth credential file too large");
            let mut text = String::new();
            f.read_to_string(&mut text)?;
            Ok(Some(serde_json::from_str(&text).context("invalid OAuth credential file")?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => bail!("cannot read private OAuth credentials"),
    }
}

/// The sign-in state `status` shows, from the file alone: `signed in`, `expired` or none.
pub(super) fn login_state(server: &str, resource: &str) -> Option<&'static str> {
    let value = load(server).ok()??;
    if !binding_matches(&value, None, resource) { return None; }
    let live = value["expires_at"].as_u64().unwrap_or(0) > now() || value["refresh_token"].is_string();
    Some(if live { "signed in" } else { "sign-in expired" })
}

/// The client an earlier login registered with this issuer, so a new login reuses it.
pub(super) fn registration(server: &str, issuer: &str) -> Option<(String, Option<String>)> {
    let value = load(server).ok()??;
    if value["login"] != true || value["issuer"] != issuer || value["redirect"] != super::login::REDIRECT { return None; }
    Some((value["client_id"].as_str()?.to_string(), value["client_secret"].as_str().map(String::from)))
}

/// The hint every "needs sign-in" error carries.
pub(super) fn login_hint(server: &str) -> String {
    format!("sign in with `zen mcp-login {server}` (or /mcp login {server} in zen)")
}

/// The bearer token for a request to `resource` (the server's URL), refreshed when it's within 30
/// seconds of expiring; None when the server has no OAuth. All reads and writes are under the
/// server's mutex.
pub(super) async fn bearer(cfg: Option<&OAuthConfig>, server: &str, resource: &str, state: &Mutex<Option<Token>>) -> Result<Option<String>> {
    let mut state = state.lock().await;
    if state.is_none() {
        if let Some(value) = load(server)?.filter(|v| binding_matches(v, cfg, resource)) {
            *state = Some(Token { value });
        }
    }
    let provisioned = cfg.filter(|c| c.provisioned());
    if state.is_none() && provisioned.is_none() {
        return Ok(None);
    }
    if let Some(token) = &*state {
        if token.value["expires_at"].as_u64().unwrap_or(0) > now() + 30 {
            return token.value["access_token"].as_str().map(|s| Some(s.to_string())).context("OAuth credentials lack access_token");
        }
    }
    // Refresh: a login's own client and endpoint, or the provisioned ones.
    let stored = state.as_ref().map(|t| t.value.clone());
    let (token_url, client_id, client_secret, refresh, resource_param) = match (&stored, provisioned) {
        (Some(v), None) => (
            v["token_url"].as_str().unwrap_or("").to_string(),
            v["client_id"].as_str().unwrap_or("").to_string(),
            v["client_secret"].as_str().map(String::from),
            v["refresh_token"].as_str().map(String::from).with_context(|| format!("the MCP sign-in for `{server}` expired; {}", login_hint(server)))?,
            Some(resource.to_string()),
        ),
        (_, Some(cfg)) => (
            cfg.token_url.clone().unwrap_or_default(),
            cfg.client_id.clone().unwrap_or_default(),
            cfg.client_secret.clone(),
            stored.as_ref().and_then(|v| v["refresh_token"].as_str().map(String::from)).or(cfg.refresh_token.clone()).unwrap_or_default(),
            None,
        ),
        (None, None) => unreachable!("returned above"),
    };
    checked_url(&token_url, "the token endpoint")?;
    let mut form = vec![("grant_type", "refresh_token".to_string()), ("refresh_token", refresh.clone()), ("client_id", client_id.clone())];
    if let Some(secret) = &client_secret { form.push(("client_secret", secret.clone())); }
    if let Some(r) = &resource_param { form.push(("resource", r.clone())); }
    let result = token_request(&token_url, &form).await.map_err(|e| anyhow!("{e:#}; {}", login_hint(server)))?;
    let access = result["access_token"].as_str().filter(|s| !s.is_empty()).context("OAuth endpoint omitted access_token")?;
    let lifetime = result["expires_in"].as_u64().unwrap_or(300).min(365 * 86400);
    let mut value = stored.unwrap_or_else(|| json!({}));
    if let Some(cfg) = provisioned {
        value["token_url"] = json!(cfg.token_url);
        value["client_id"] = json!(cfg.client_id);
        value["provisioned_refresh_token"] = json!(cfg.refresh_token);
    }
    value["access_token"] = json!(access);
    value["refresh_token"] = json!(result["refresh_token"].as_str().unwrap_or(&refresh));
    value["expires_at"] = json!(now().saturating_add(lifetime));
    save(server, &value)?;
    *state = Some(Token { value });
    Ok(Some(access.to_string()))
}

/// POST a token request; the JSON answer if it's a Bearer token. Errors never quote the body.
pub(super) async fn token_request(url: &str, form: &[(&str, String)]) -> Result<Value> {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(30)).build()?;
    let response = client.post(url).header("Accept", "application/json").form(form).send().await.map_err(|_| anyhow!("OAuth token endpoint could not be reached"))?;
    let status = response.status();
    anyhow::ensure!(status.is_success(), "OAuth token request failed (HTTP {status})");
    let result = read_json(response).await.map_err(|_| anyhow!("invalid OAuth token response"))?;
    anyhow::ensure!(result["token_type"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("bearer")), "OAuth endpoint must return a Bearer token");
    Ok(result)
}

/// A JSON body of at most 64 KB.
pub(super) async fn read_json(response: reqwest::Response) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow!("the response could not be read"))?;
        anyhow::ensure!(bytes.len() + chunk.len() <= 64 * 1024, "response too large");
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Write a server's credentials (mode 600, through a temporary file in the private directory).
pub(super) fn save(server: &str, value: &Value) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let dir = directory();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    anyhow::ensure!(!std::fs::symlink_metadata(&dir)?.file_type().is_symlink(), "OAuth credential directory must not be a symlink");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let temporary = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        f.write_all(serde_json::to_string(value)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&temporary, path(server))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_endpoints_require_tls_and_explicit_credentials() {
        let parse = |v: Value| OAuthConfig::parse(&v, &[], &mut Vec::new());
        for url in ["http://example.com/token", "https://user:pass@example.com/token", "https://example.com/token#secret"] {
            assert!(parse(json!({"token_url":url,"client_id":"client","refresh_token":"refresh"})).is_err());
        }
        assert!(parse(json!({"token_url":"https://example.com/token","client_id":"client","refresh_token":"refresh"})).is_ok());
        assert!(parse(json!({"refresh_token":"refresh"})).is_err(), "provisioned needs its endpoint and client");
        let login = parse(json!({"client_id":"pre-registered","scope":"read"})).unwrap();
        assert!(!login.provisioned() && login.client_id.as_deref() == Some("pre-registered"));
    }

    #[test]
    fn credentials_are_bound_to_their_source() {
        let login = json!({"login":true,"resource":"https://mcp.example.com/mcp"});
        assert!(binding_matches(&login, None, "https://mcp.example.com/mcp"));
        assert!(!binding_matches(&login, None, "https://other.example.com/mcp"), "a changed URL needs a new login");
        let cfg = OAuthConfig::parse(&json!({"token_url":"https://example.com/token","client_id":"c","refresh_token":"r"}), &[], &mut Vec::new()).unwrap();
        assert!(!binding_matches(&login, Some(&cfg), "https://mcp.example.com/mcp"), "provisioned config ignores a login file");
        assert!(binding_matches(&json!({"token_url":"https://example.com/token","client_id":"c","provisioned_refresh_token":"r"}), Some(&cfg), "x"));
    }
}
