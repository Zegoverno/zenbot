//! Refresh-token OAuth for owner-provisioned clients. Browser authorization is a separate owner
//! operation; this module never guesses endpoints, follows redirects, or exposes token responses.
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub struct OAuthConfig {
    token_url: String,
    client_id: String,
    refresh_token: String,
    client_secret: Option<String>,
}

impl OAuthConfig {
    pub fn parse(value: &Value, allow: &[String], missing: &mut Vec<String>) -> Result<Self> {
        let field = |key: &str, missing: &mut Vec<String>| super::expand(value[key].as_str().unwrap_or(""), allow, missing);
        let cfg = Self {
            token_url: field("token_url", missing),
            client_id: field("client_id", missing),
            refresh_token: field("refresh_token", missing),
            client_secret: value["client_secret"].as_str().map(|s| super::expand(s, allow, missing)),
        };
        let url = reqwest::Url::parse(&cfg.token_url).context("oauth.token_url must be an absolute URL")?;
        let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        anyhow::ensure!(url.scheme() == "https" || (url.scheme() == "http" && local), "oauth.token_url requires HTTPS (HTTP is allowed on loopback only)");
        anyhow::ensure!(url.username().is_empty() && url.password().is_none() && url.fragment().is_none(), "oauth.token_url must not contain credentials or fragments");
        anyhow::ensure!(!cfg.client_id.is_empty() && !cfg.refresh_token.is_empty(), "oauth requires client_id and refresh_token; provision them as the owner");
        Ok(cfg)
    }
}

#[derive(Clone)]
pub(super) struct Token {
    value: Value,
}

pub(crate) fn directory() -> PathBuf { crate::zen_home().join("mcp-auth") }

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }

fn binding_matches(value: &Value, cfg: &OAuthConfig) -> bool {
    value["token_url"] == cfg.token_url && value["client_id"] == cfg.client_id
        && value["provisioned_refresh_token"] == cfg.refresh_token
}

pub(super) async fn expire(state: &Mutex<Option<Token>>) {
    if let Some(token) = &mut *state.lock().await { token.value["expires_at"] = json!(0); }
}

/// All reads and writes are under the server's mutex. Rotated refresh tokens are persisted before
/// using the access token, so a restart cannot silently fall back to a consumed refresh token.
pub(super) async fn access_token(cfg: &OAuthConfig, server: &str, state: &Mutex<Option<Token>>) -> Result<String> {
    let mut state = state.lock().await;
    let path = directory().join(format!("{server}.json"));
    if state.is_none() {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&path) {
            Ok(mut f) => {
                anyhow::ensure!(f.metadata()?.len() <= 64 * 1024, "OAuth credential file too large");
                let mut text = String::new();
                f.read_to_string(&mut text)?;
                let value: Value = serde_json::from_str(&text).context("invalid OAuth credential file")?;
                if binding_matches(&value, cfg) { *state = Some(Token { value }); }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => bail!("cannot read private OAuth credentials"),
        }
    }
    if let Some(token) = &*state {
        if token.value["expires_at"].as_u64().unwrap_or(0) > now() + 30 {
            return token.value["access_token"].as_str().map(String::from).context("OAuth credentials lack access_token");
        }
    }
    let refresh = state.as_ref().and_then(|t| t.value["refresh_token"].as_str()).unwrap_or(&cfg.refresh_token);
    let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", cfg.client_id.as_str())];
    if let Some(secret) = &cfg.client_secret { form.push(("client_secret", secret)); }
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(30)).build()?;
    let response = client.post(&cfg.token_url).form(&form).send().await.map_err(|_| anyhow!("OAuth token endpoint could not be reached"))?;
    let status = response.status();
    // Never include endpoint bodies/errors: they can reflect the submitted credentials.
    anyhow::ensure!(status.is_success(), "OAuth refresh failed (HTTP {status}); owner authorization may be required");
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow!("OAuth token response could not be read"))?;
        anyhow::ensure!(bytes.len() + chunk.len() <= 64 * 1024, "OAuth token response too large");
        bytes.extend_from_slice(&chunk);
    }
    let result: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid OAuth token response"))?;
    anyhow::ensure!(result["token_type"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("bearer")), "OAuth endpoint must return a Bearer token");
    let access = result["access_token"].as_str().filter(|s| !s.is_empty()).context("OAuth endpoint omitted access_token")?;
    let lifetime = result["expires_in"].as_u64().unwrap_or(300).min(365 * 86400);
    let value = json!({"token_url":cfg.token_url,"client_id":cfg.client_id,
        "provisioned_refresh_token":cfg.refresh_token,"access_token":access,
        "refresh_token":result["refresh_token"].as_str().unwrap_or(refresh),
        "expires_at":now().saturating_add(lifetime)});
    save(&path, &value)?;
    *state = Some(Token { value });
    Ok(access.to_string())
}

fn save(path: &std::path::Path, value: &Value) -> Result<()> {
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
        std::fs::rename(&temporary, path)?;
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
        for url in ["http://example.com/token", "https://user:pass@example.com/token", "https://example.com/token#secret"] {
            assert!(OAuthConfig::parse(&json!({"token_url":url,"client_id":"client","refresh_token":"refresh"}), &[], &mut Vec::new()).is_err());
        }
        assert!(OAuthConfig::parse(&json!({"token_url":"https://example.com/token","client_id":"client","refresh_token":"refresh"}), &[], &mut Vec::new()).is_ok());
    }
}
