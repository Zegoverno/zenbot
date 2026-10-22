//! The owner's browser sign-in to a remote MCP server, driven from the terminal (`zen mcp-login`,
//! `/mcp login` in zen; D-072). The kernel and the owner's browser are usually on different
//! machines, so nothing listens for the redirect: the owner opens the link, approves, and pastes
//! back the address the browser ended on (a "can't connect" page on 127.0.0.1 is expected).
//!
//! Following the MCP authorization spec: the server's protected resource metadata (RFC 9728, from
//! the 401's `resource_metadata` or the well-known path) names the authorization server; its
//! metadata (RFC 8414 or OpenID discovery) must match its issuer and offer PKCE S256. The client is
//! the one the owner put in mcp.json, the one an earlier login registered, or a new one registered
//! dynamically (RFC 7591, a native app with a loopback redirect). Authorization code with PKCE and
//! the resource indicator (RFC 8707); the returned `iss` is checked (RFC 9207). The resulting
//! credentials are stored by auth.rs, bound to the server's URL.
//!
//! Only the owner can start or finish a login (the API's token); a flow lives 10 minutes in memory,
//! and a restart forgets it. Endpoints must be https (http only on loopback), no redirects are
//! followed, and errors never quote a token endpoint's body.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::auth;

/// The redirect URI a login registers and sends: fixed, so a hand-registered app can name it.
pub(crate) const REDIRECT: &str = "http://127.0.0.1:33418/callback";
const FLOW_TTL: Duration = Duration::from_secs(600);

struct Flow {
    server: String,
    resource: String,
    issuer: String,
    token_url: String,
    client_id: String,
    client_secret: Option<String>,
    verifier: String,
    state: String,
    scope: Option<String>,
    iss_required: bool,
    started: Instant,
}

static FLOWS: LazyLock<std::sync::Mutex<HashMap<String, Flow>>> = LazyLock::new(Default::default);

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(15)).build()?)
}

async fn get_json(url: &reqwest::Url) -> Result<Option<Value>> {
    let response = http()?.get(url.clone()).header("Accept", "application/json").send().await
        .with_context(|| format!("couldn't reach {}", url.host_str().unwrap_or("")))?;
    if !response.status().is_success() { return Ok(None); }
    Ok(auth::read_json(response).await.ok())
}

/// `key="value"` from a WWW-Authenticate header.
fn challenge_param(header: &str, key: &str) -> Option<String> {
    let at = header.find(&format!("{key}=\""))? + key.len() + 2;
    let end = header[at..].find('"')?;
    Some(header[at..at + end].to_string())
}

/// `{origin}/.well-known/{name}{path}`, and the same at the root when the URL has a path.
fn well_known(url: &reqwest::Url, name: &str) -> Vec<reqwest::Url> {
    let path = url.path().trim_end_matches('/');
    let mut out = Vec::new();
    for p in [path, ""] {
        if let Ok(u) = url.join(&format!("/.well-known/{name}{p}")) {
            if !out.contains(&u) { out.push(u); }
        }
    }
    out
}

/// The server's protected resource metadata and the scope its challenge asked for.
async fn protected_resource(server: &reqwest::Url) -> Result<(Value, Option<String>)> {
    let probe = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "zenbot", "version": env!("CARGO_PKG_VERSION") } } });
    let response = http()?.post(server.clone()).header("Accept", "application/json, text/event-stream").json(&probe).send().await
        .with_context(|| format!("couldn't reach {}", server.host_str().unwrap_or("")))?;
    anyhow::ensure!(response.status() == reqwest::StatusCode::UNAUTHORIZED || response.status() == reqwest::StatusCode::FORBIDDEN,
        "the server answered HTTP {} without asking for sign-in", response.status());
    let header = response.headers().get_all("www-authenticate").iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join(", ");
    let scope = challenge_param(&header, "scope");
    let mut candidates = Vec::new();
    if let Some(u) = challenge_param(&header, "resource_metadata") {
        candidates.push(auth::checked_url(&u, "resource_metadata")?);
    }
    candidates.extend(well_known(server, "oauth-protected-resource"));
    for url in candidates {
        if let Some(meta) = get_json(&url).await? {
            return Ok((meta, scope));
        }
    }
    bail!("the server didn't say where to sign in (no protected resource metadata)")
}

/// Whether the metadata's `resource` names this server (the same URL, or a prefix of it).
fn resource_matches(resource: &str, server: &str) -> bool {
    let (r, s) = (resource.trim_end_matches('/'), server.trim_end_matches('/'));
    s == r || s.starts_with(&format!("{r}/"))
}

/// The authorization server's metadata, checked against its issuer.
async fn authorization_server(issuer: &str) -> Result<Value> {
    let url = auth::checked_url(issuer, "the authorization server")?;
    let mut candidates = well_known(&url, "oauth-authorization-server");
    candidates.extend(well_known(&url, "openid-configuration"));
    if let Ok(u) = reqwest::Url::parse(&format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'))) {
        if !candidates.contains(&u) { candidates.push(u); }
    }
    for candidate in candidates {
        let Some(meta) = get_json(&candidate).await? else { continue };
        anyhow::ensure!(meta["issuer"].as_str().map(|i| i.trim_end_matches('/')) == Some(issuer.trim_end_matches('/')),
            "the authorization server's metadata names another issuer");
        for key in ["authorization_endpoint", "token_endpoint"] {
            auth::checked_url(meta[key].as_str().unwrap_or(""), key)?;
        }
        anyhow::ensure!(meta["code_challenge_methods_supported"].as_array().is_some_and(|m| m.iter().any(|v| v == "S256")),
            "the authorization server doesn't offer PKCE (S256), which MCP requires");
        return Ok(meta);
    }
    bail!("no metadata found for the authorization server {}", url.host_str().unwrap_or(""))
}

/// Register zenbot as a native client with the loopback redirect.
async fn register(endpoint: &str) -> Result<(String, Option<String>)> {
    let url = auth::checked_url(endpoint, "registration_endpoint")?;
    let mut body = json!({ "client_name": "zenbot", "redirect_uris": [REDIRECT], "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"], "token_endpoint_auth_method": "none", "application_type": "native" });
    for attempt in 0..2 {
        let response = http()?.post(url.clone()).json(&body).send().await.context("couldn't reach the registration endpoint")?;
        if response.status().is_success() {
            let v = auth::read_json(response).await.context("invalid registration response")?;
            let id = v["client_id"].as_str().filter(|s| !s.is_empty()).context("registration returned no client_id")?;
            return Ok((id.to_string(), v["client_secret"].as_str().map(String::from)));
        }
        // Some servers refuse OpenID's `application_type`: try once without it.
        if attempt == 0 && response.status().is_client_error() {
            body.as_object_mut().unwrap().remove("application_type");
            continue;
        }
        bail!("client registration failed (HTTP {})", response.status());
    }
    unreachable!()
}

fn random() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Start a login: `{flow, authorize_url, auth_server}`. The owner opens the URL in a browser.
pub(crate) async fn start(server: &str) -> Result<Value> {
    let cfg = super::server_config(server).await.with_context(|| format!("no MCP server named `{server}` in mcp.json"))?;
    let url = cfg.url.as_deref().context("only a remote (url) server signs in; a local one gets its credentials from env")?;
    let server_url = auth::checked_url(url, "the server's url")?;
    let (prm, challenge_scope) = protected_resource(&server_url).await?;
    let resource = prm["resource"].as_str().unwrap_or(url).to_string();
    anyhow::ensure!(resource_matches(&resource, url), "the server's metadata names another resource ({resource}); not signing in");
    let issuer = prm["authorization_servers"].as_array().and_then(|a| a.first()).and_then(Value::as_str)
        .context("the server's metadata names no authorization server")?.to_string();
    let meta = authorization_server(&issuer).await?;
    let oauth = cfg.oauth.as_ref();
    let earlier = auth::registration(server, &issuer);
    let (client_id, client_secret) = match (oauth.and_then(|o| o.client_id.clone()), earlier) {
        (Some(id), _) => (id, oauth.and_then(|o| o.client_secret.clone())),
        (None, Some(found)) => found,
        (None, None) => match meta["registration_endpoint"].as_str() {
            Some(endpoint) => register(endpoint).await?,
            None => bail!("{} doesn't register apps automatically: register one there with the redirect URI {REDIRECT}, then put its client_id (and client_secret) under `oauth` for `{server}` in mcp.json",
                reqwest::Url::parse(&issuer).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or(issuer.clone())),
        },
    };
    let scope = challenge_scope.or(oauth.and_then(|o| o.scope.clone())).or_else(|| {
        prm["scopes_supported"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty())
    });
    let (verifier, state) = (random(), uuid::Uuid::new_v4().simple().to_string());
    let mut authorize = auth::checked_url(meta["authorization_endpoint"].as_str().unwrap_or(""), "authorization_endpoint")?;
    {
        let mut q = authorize.query_pairs_mut();
        q.append_pair("response_type", "code").append_pair("client_id", &client_id).append_pair("redirect_uri", REDIRECT)
            .append_pair("code_challenge", &challenge(&verifier)).append_pair("code_challenge_method", "S256")
            .append_pair("state", &state).append_pair("resource", &resource);
        if let Some(s) = &scope { q.append_pair("scope", s); }
    }
    let flow = uuid::Uuid::new_v4().simple().to_string();
    let auth_server = authorize.host_str().unwrap_or("").to_string();
    let mut flows = FLOWS.lock().unwrap_or_else(|e| e.into_inner());
    flows.retain(|_, f| f.started.elapsed() < FLOW_TTL);
    flows.insert(flow.clone(), Flow {
        server: server.to_string(), resource, issuer, token_url: meta["token_endpoint"].as_str().unwrap_or("").to_string(),
        client_id, client_secret, verifier, state, scope,
        iss_required: meta["authorization_response_iss_parameter_supported"] == true, started: Instant::now(),
    });
    Ok(json!({ "flow": flow, "authorize_url": authorize.as_str(), "auth_server": auth_server, "redirect": REDIRECT }))
}

/// What the owner pasted: the code, after checking the state and issuer of a full redirect URL.
fn code_from(flow: &Flow, pasted: &str) -> Result<String> {
    let pasted = pasted.trim();
    if !pasted.contains("://") {
        anyhow::ensure!(!pasted.is_empty() && pasted.len() <= 4096 && !pasted.contains(char::is_whitespace), "paste the whole address your browser ended on");
        anyhow::ensure!(!flow.iss_required, "paste the whole address your browser ended on (this server's answer must be checked)");
        return Ok(pasted.to_string());
    }
    let url = reqwest::Url::parse(pasted).context("that isn't an address")?;
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if let Some(error) = q.get("error") {
        bail!("the sign-in was refused: {error} {}", zen_proto::head(q.get("error_description").map(String::as_str).unwrap_or(""), 200));
    }
    anyhow::ensure!(q.get("state") == Some(&flow.state), "that address belongs to another sign-in (state doesn't match); start again");
    match q.get("iss") {
        Some(iss) => anyhow::ensure!(iss.trim_end_matches('/') == flow.issuer.trim_end_matches('/'), "the answer came from another authorization server; not signing in"),
        None => anyhow::ensure!(!flow.iss_required, "the answer lacks the issuer this server promised; not signing in"),
    }
    q.get("code").filter(|c| !c.is_empty()).cloned().context("the address has no code")
}

/// Finish a login with what the owner pasted; stores the credentials and returns a summary.
pub(crate) async fn finish(flow_id: &str, pasted: &str) -> Result<Value> {
    let flow = {
        let mut flows = FLOWS.lock().unwrap_or_else(|e| e.into_inner());
        flows.retain(|_, f| f.started.elapsed() < FLOW_TTL);
        flows.remove(flow_id).context("this sign-in expired or was already used; start again")?
    };
    let code = code_from(&flow, pasted)?;
    let mut form = vec![("grant_type", "authorization_code".to_string()), ("code", code), ("redirect_uri", REDIRECT.to_string()),
        ("client_id", flow.client_id.clone()), ("code_verifier", flow.verifier.clone()), ("resource", flow.resource.clone())];
    if let Some(secret) = &flow.client_secret { form.push(("client_secret", secret.clone())); }
    let result = auth::token_request(&flow.token_url, &form).await?;
    let access = result["access_token"].as_str().filter(|s| !s.is_empty()).context("the token endpoint returned no access token")?;
    let lifetime = result["expires_in"].as_u64().unwrap_or(3600).min(365 * 86400);
    let expires_at = auth::now().saturating_add(lifetime);
    let mut value = json!({ "login": true, "resource": flow.resource, "issuer": flow.issuer, "token_url": flow.token_url,
        "client_id": flow.client_id, "redirect": REDIRECT, "scope": result["scope"].as_str().map(String::from).or(flow.scope),
        "access_token": access, "expires_at": expires_at });
    if let Some(s) = &flow.client_secret { value["client_secret"] = json!(s); }
    let refresh = result["refresh_token"].as_str().filter(|s| !s.is_empty());
    if let Some(r) = refresh { value["refresh_token"] = json!(r); }
    auth::save(&flow.server, &value)?;
    super::signed_in(&flow.server).await;
    Ok(json!({ "server": flow.server, "expires_at": expires_at, "renews": refresh.is_some() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(iss_required: bool) -> Flow {
        Flow { server: "s".into(), resource: "https://mcp.example.com/mcp".into(), issuer: "https://auth.example.com".into(),
            token_url: "https://auth.example.com/token".into(), client_id: "c".into(), client_secret: None,
            verifier: "v".into(), state: "st".into(), scope: None, iss_required, started: Instant::now() }
    }

    #[test]
    fn pasted_answers_are_checked() {
        let f = flow(false);
        assert_eq!(code_from(&f, "http://127.0.0.1:33418/callback?code=abc&state=st").unwrap(), "abc");
        assert_eq!(code_from(&f, " http://127.0.0.1:33418/callback?code=abc&state=st&iss=https%3A%2F%2Fauth.example.com%2F ").unwrap(), "abc");
        assert!(code_from(&f, "http://127.0.0.1:33418/callback?code=abc&state=other").is_err(), "another flow's state");
        assert!(code_from(&f, "http://127.0.0.1:33418/callback?code=abc&state=st&iss=https%3A%2F%2Fevil.example").is_err(), "another issuer");
        assert!(code_from(&f, "http://127.0.0.1:33418/callback?error=access_denied&state=st").unwrap_err().to_string().contains("refused"));
        assert_eq!(code_from(&f, "abc").unwrap(), "abc", "a bare code");
        let strict = flow(true);
        assert!(code_from(&strict, "abc").is_err() && code_from(&strict, "http://127.0.0.1:33418/callback?code=abc&state=st").is_err());
    }

    #[test]
    fn discovery_helpers() {
        assert_eq!(challenge_param(r#"Bearer error="invalid_token", resource_metadata="https://x/.well-known/oauth-protected-resource", scope="a b""#, "scope").as_deref(), Some("a b"));
        let u = reqwest::Url::parse("https://mcp.example.com/v1/mcp").unwrap();
        let wk: Vec<String> = well_known(&u, "oauth-protected-resource").iter().map(|u| u.to_string()).collect();
        assert_eq!(wk, ["https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp", "https://mcp.example.com/.well-known/oauth-protected-resource"]);
        assert!(resource_matches("https://mcp.example.com", "https://mcp.example.com/mcp"));
        assert!(resource_matches("https://mcp.example.com/mcp/", "https://mcp.example.com/mcp"));
        assert!(!resource_matches("https://mcp.example.com/m", "https://mcp.example.com/mcp"));
        assert!(!resource_matches("https://evil.example.com", "https://mcp.example.com/mcp"));
        // RFC 7636 appendix B.
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }
}
