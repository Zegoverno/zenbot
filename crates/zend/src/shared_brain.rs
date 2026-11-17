//! Narrow shared-brain API for external stdio harnesses. Authentication is the normal kernel
//! bearer middleware. Each connection owns a real, agent-bound session; mode lives in SQLite,
//! never in tool arguments. No shell, generic file reads or downstream MCP forwarding.
use super::*;
use axum::extract::Path as AxPath;
use std::path::Path as FsPath;
use crate::api::{ApiError, ApiResult};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Open {
    agent: String,
    #[serde(default)]
    write: bool,
}

pub(crate) async fn open(State(app): State<AppState>, Json(body): Json<Open>) -> ApiResult<Json<Value>> {
    let who = crate::agents::load(&body.agent).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id,title,model,agent,kind) VALUES ($1,'External shared brain (MCP)',$2,$3,$4)")
        .bind(id).bind(&app.default_model).bind(&who.name).bind(if body.write { "mcp_write" } else { "mcp_read" })
        .execute(&app.db).await?;
    Ok(Json(json!({"id": id})))
}

async fn scope(app: &AppState, id: Uuid) -> ApiResult<(crate::agents::Agent, bool)> {
    let row = sqlx::query("SELECT agent,kind FROM sessions WHERE id=$1 AND NOT archived")
        .bind(id).fetch_optional(&app.db).await?.ok_or_else(crate::api::not_found)?;
    let kind: Option<String> = row.get("kind");
    let write = match kind.as_deref() {
        Some("mcp_write") => true,
        Some("mcp_read") => false,
        _ => return Err(ApiError(StatusCode::FORBIDDEN, "not a shared-brain session".into())),
    };
    Ok((crate::agents::load(&row.get::<String,_>("agent"))?, write))
}

fn allowed(name: &str, args: &Value, write: bool) -> bool {
    match name {
        "history" | "search" | "wiki_read" => true,
        "brain" => matches!(args["action"].as_str(), Some("get" | "list" | "schema")) || (write && args["action"] == "put"),
        "remember" => write,
        _ => false,
    }
}

fn specs(who: &crate::agents::Agent, write: bool) -> Value {
    let mut tools: Vec<Value> = crate::agent::specs_for(None, who).as_array().into_iter().flatten()
        .filter(|s| matches!(s["name"].as_str(), Some("history" | "search" | "brain")) || (write && s["name"] == "remember"))
        .map(|s| {
            let mut schema = s["parameters"].clone();
            if !write && s["name"] == "brain" {
                schema["properties"]["action"]["enum"] = json!(["get","list","schema"]);
            }
            json!({"name":s["name"],"description":s["description"],"inputSchema":schema})
        }).collect();
    if who.allows("read") {
        tools.push(json!({"name":"wiki_read","description":"Read a Markdown page relative to your wiki root. Symlinks and traversal are refused. Set global=true for the shared wiki if permitted.",
            "inputSchema":{"type":"object","properties":{"path":{"type":"string"},"global":{"type":"boolean"}},"required":["path"],"additionalProperties":false}}));
    }
    json!({"tools":tools})
}

pub(crate) async fn list(State(app): State<AppState>, AxPath(id): AxPath<Uuid>) -> ApiResult<Json<Value>> {
    let (who, write) = scope(&app, id).await?;
    Ok(Json(specs(&who, write)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Call {
    name: String,
    #[serde(default = "empty_args")]
    arguments: Value,
}
fn empty_args() -> Value { json!({}) }

pub(crate) async fn call(State(app): State<AppState>, AxPath(id): AxPath<Uuid>, Json(body): Json<Call>) -> ApiResult<Json<Value>> {
    let (who, write) = scope(&app, id).await?;
    let permitted = allowed(&body.name, &body.arguments, write)
        && who.allows(if body.name == "wiki_read" { "read" } else { &body.name });
    // Even rejected calls use the kernel bookkeeping path.
    let result = crate::dispatch::external_brain_call(&app, id, &who, &body.name, &body.arguments, permitted).await?;
    Ok(Json(result))
}

pub(crate) fn read_wiki(who: &crate::agents::Agent, args: &Value) -> Result<String> {
    let global = args["global"].as_bool().unwrap_or(false);
    if global && who.global_level() == crate::agents::Global::None {
        anyhow::bail!("shared wiki is not available to this agent");
    }
    let root = if global || who.name == crate::agents::DEFAULT { crate::wiki::root() } else { who.vault().join("wiki") };
    confined_read(&root, args["path"].as_str().unwrap_or(""))
}

/// Walk with directory FDs and O_NOFOLLOW, not canonicalize-then-open (a symlink swap race).
/// Neither the root nor a descendant may be a symlink; no FIFO, device or non-md is read.
fn confined_read(root: &FsPath, path: &str) -> Result<String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::io::Read;
    let path = FsPath::new(path);
    let parts: Vec<_> = path.components().collect();
    if parts.is_empty() || parts.iter().any(|p| !matches!(p, std::path::Component::Normal(_)))
        || path.extension().is_none_or(|e| e != "md") {
        anyhow::bail!("expected a relative Markdown path without traversal");
    }
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW).open(root)?;
    for (i, part) in parts.iter().enumerate() {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(part.as_os_str().as_bytes())?;
        let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
            | if i + 1 < parts.len() { libc::O_DIRECTORY } else { 0 };
        // SAFETY: valid owned directory FD and NUL-terminated name; result is owned once.
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
        file = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    if !file.metadata()?.is_file() { anyhow::bail!("not a regular wiki page"); }
    let mut text = String::new();
    file.take(200_001).read_to_string(&mut text)?;
    if text.len() > 200_000 { anyhow::bail!("wiki page exceeds 200000 bytes"); }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_is_allowlisted_and_read_only_by_default() {
        for name in ["bash","read","write","call_tool","find_tools","brain_action","remember"] {
            assert!(!allowed(name, &json!({}), false), "{name}");
        }
        for action in ["get","list","schema"] { assert!(allowed("brain",&json!({"action":action}),false)); }
        for action in ["put","lint","index","delete",""] { assert!(!allowed("brain",&json!({"action":action}),false)); }
        assert!(allowed("brain",&json!({"action":"put"}),true));
        assert!(!allowed("bash",&json!({}),true));
    }
    #[test]
    fn wiki_confines_traversal_symlinks_and_special_files() {
        let root = std::env::temp_dir().join(format!("wiki-mcp-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("ok.md"),"hello").unwrap();
        std::fs::write(root.join("sub/nested.md"),"nested").unwrap();
        std::os::unix::fs::symlink(root.join("ok.md"),root.join("link.md")).unwrap();
        std::os::unix::fs::symlink(root.join("sub"),root.join("linked")).unwrap();
        let linked_root = root.with_extension("linked");
        std::os::unix::fs::symlink(&root, &linked_root).unwrap();
        assert!(confined_read(&linked_root,"ok.md").is_err());
        std::fs::remove_file(linked_root).unwrap();
        assert_eq!(confined_read(&root,"ok.md").unwrap(),"hello");
        assert_eq!(confined_read(&root,"sub/nested.md").unwrap(),"nested");
        for p in ["../ok.md","/etc/passwd","link.md","linked/nested.md","sub/../ok.md","ok.txt",""] {
            assert!(confined_read(&root,p).is_err(), "{p}");
        }
        std::fs::create_dir(root.join("dir.md")).unwrap();
        assert!(confined_read(&root,"dir.md").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
