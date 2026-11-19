//! Authenticated engine sign-in endpoints. The device process is owned by a bounded task.
use super::*;
use tokio::io::{AsyncBufReadExt, BufReader};
use std::sync::OnceLock;

fn state() -> &'static Mutex<Value> {
    static STATE: OnceLock<Mutex<Value>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(json!({"status":"idle"})))
}
fn valid_engine(name: &str) -> ApiResult<()> {
    if ["codex", "claude", "openrouter"].contains(&name) { Ok(()) }
    else { Err(ApiError(StatusCode::NOT_FOUND, "unknown engine".into())) }
}

pub(crate) async fn list(State(app): State<AppState>) -> Json<Value> {
    let models = collect_models(&app).await;
    let pending = state().lock().await.clone();
    let engines: Vec<_> = ["codex", "claude", "openrouter"].iter().map(|name| json!({
        "id": name, "signed_in": models["authenticated"][*name].as_bool().unwrap_or(false),
        "login": if *name == "codex" { pending.clone() } else { json!({"status":"idle"}) },
        "runtime_note": if *name == "openrouter" { Some("Changes require a kernel service restart; signed_in reflects the running process.") } else { None }
    })).collect();
    Json(json!({"engines": engines}))
}

// Strip terminal formatting before parsing the CLI's documented device prompt.
fn plain(s: &str) -> String {
    let mut out = String::new(); let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' { if chars.next() == Some('[') { for x in chars.by_ref() { if x.is_ascii_alphabetic() { break; } } } }
        else { out.push(c); }
    }
    out
}
fn parse_prompt(s: &str) -> Option<(String, String)> {
    let s = plain(s);
    let url = s.split_whitespace().find(|w| *w == "https://auth.openai.com/codex/device")?;
    let after = s.split_once("Enter this one-time code")?.1;
    let code = after.lines().skip(1).flat_map(str::split_whitespace).find(|w| {
        let Some((a,b)) = w.split_once('-') else { return false; };
        (4..=8).contains(&a.len()) && (4..=8).contains(&b.len()) && a.chars().chain(b.chars()).all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    })?;
    Some((url.into(), code.into()))
}

pub(crate) async fn login(Path(engine): Path<String>) -> ApiResult<Json<Value>> {
    valid_engine(&engine)?;
    if engine == "claude" { return Ok(Json(json!({"status":"instructions", "instructions":"Run `claude auth login` in a terminal on this machine; its interactive browser flow is not exposed here."}))); }
    if engine == "openrouter" { return Err(ApiError(StatusCode::BAD_REQUEST, "Use PUT /api/engines/openrouter/key".into())); }
    let mut current = state().lock().await;
    if current["status"] == "starting" || current["status"] == "pending" { return Err(ApiError(StatusCode::CONFLICT, "Codex login already in progress".into())); }
    let mut child = tokio::process::Command::new("codex").args(["login", "--device-auth"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "Could not start codex login --device-auth".into()))?;
    let stdout = child.stdout.take().unwrap(); let stderr = child.stderr.take().unwrap();
    *current = json!({"status":"starting"});
    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::channel::<String>(32);
        for reader in [stdout_to_box(stdout), stdout_to_box(stderr)] {
            let tx = tx.clone(); tokio::spawn(async move { let mut lines = BufReader::new(reader).lines(); while let Ok(Some(line)) = lines.next_line().await { if tx.send(line).await.is_err() { break; } } });
        }
        drop(tx);
        let task = async {
            let mut output = String::new(); let mut parsed = false;
            let deadline = tokio::time::sleep(Duration::from_secs(30)); tokio::pin!(deadline);
            loop { tokio::select! {
                _ = &mut deadline, if !parsed => { let _ = child.kill().await; *state().lock().await = json!({"status":"error", "error":"Unsupported Codex device-auth output: no device URL and code within 30 seconds."}); return; },
                line = rx.recv() => match line { Some(line) => { if output.len() < 16384 { output.push_str(&line); output.push('\n'); } if !parsed { if let Some((url,code)) = parse_prompt(&output) { *state().lock().await = json!({"status":"pending", "url":url, "code":code}); parsed = true; } } }, None => break },
                result = child.wait() => { *state().lock().await = if result.is_ok_and(|s| s.success()) && parsed { json!({"status":"complete"}) } else if !parsed { json!({"status":"error", "error":"Unsupported Codex device-auth output or login failed; check the installed CLI."}) } else { json!({"status":"error", "error":"Codex login failed."}) }; return; }
            }}
            let result = child.wait().await;
            *state().lock().await = if result.is_ok_and(|s| s.success()) && parsed { json!({"status":"complete"}) } else { json!({"status":"error", "error":"Unsupported Codex device-auth output or login failed; check the installed CLI."}) };
        };
        if tokio::time::timeout(Duration::from_secs(960), task).await.is_err() { let _ = child.kill().await; *state().lock().await = json!({"status":"expired"}); }
    });
    Ok(Json(json!({"status":"starting"})))
}
fn stdout_to_box<T: tokio::io::AsyncRead + Unpin + Send + 'static>(r: T) -> Box<dyn tokio::io::AsyncRead + Unpin + Send> { Box::new(r) }

pub(crate) async fn logout(Path(engine): Path<String>) -> ApiResult<Json<Value>> {
    valid_engine(&engine)?;
    let args: &[&str] = match engine.as_str() { "codex" => &["logout"], "claude" => &["auth", "logout"], _ => return Err(ApiError(StatusCode::BAD_REQUEST, "OpenRouter key removal is not supported here".into())) };
    if engine == "codex" && ["starting", "pending"].contains(&state().lock().await["status"].as_str().unwrap_or("")) { return Err(ApiError(StatusCode::CONFLICT, "Wait for the pending login to finish before logging out".into())); }
    let result = tokio::time::timeout(Duration::from_secs(30), tokio::process::Command::new(&engine).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true).status()).await;
    if !matches!(result, Ok(Ok(s)) if s.success()) { return Err(ApiError(StatusCode::BAD_GATEWAY, "Engine logout failed".into())); }
    Ok(Json(json!({"status":"logged_out"})))
}
#[derive(Deserialize)]
pub(crate) struct KeyBody { key: String }
pub(crate) async fn set_key(Json(body): Json<KeyBody>) -> ApiResult<Json<Value>> {
    crate::secrets::set_openrouter_key(&body.key).map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Invalid key or unable to store it".into()))?;
    Ok(Json(json!({"status":"saved", "message":"Restart the kernel service to use this key; the running process is unchanged."})))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn prompt() { let p = "Open\n\u{1b}[94mhttps://auth.openai.com/codex/device\u{1b}[0m\nEnter this one-time code (expires in 15 minutes)\n\u{1b}[94mABCD-12345\u{1b}[0m\n"; assert_eq!(parse_prompt(p), Some(("https://auth.openai.com/codex/device".into(), "ABCD-12345".into()))); assert!(parse_prompt("other output").is_none()); }
    #[test] fn unknown() { assert!(valid_engine("bad").is_err()); }
    #[test] fn secret_redaction() { assert!(!crate::secrets::mask("sk-or-abcdefghijklmnopqrstuvwxyz123456").contains("abcdefghijklmnopqrstuvwxyz")); }
}
