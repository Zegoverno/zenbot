//! Plugins (ADR 0001, "Host API (v0)"; the author's side is PLUGINS.md): an MCP server with optional
//! MCP App UIs (`ui://` resources), installed from a release archive and run sandboxed.
//!
//! **Install without running anyone's code.** A plugin comes from the marketplace (a name: its
//! entry's archive, checked against the entry's sha256), a local `.tgz`, or a local folder holding a
//! built plugin. It is unpacked here, in Rust, into `plugins/.staging/<id>/`, entry by entry: only
//! regular files and folders (no links of either kind, devices or fifos), no absolute paths, no
//! `..`, no dot files or folders, no `node_modules`, at most 50 MB downloaded and 50 MB unpacked
//! (the entire decompressed stream is capped, including tar metadata), 10,000 entries. A folder is
//! copied by the same rules, except that dot entries and `node_modules` are skipped rather than
//! refused (a development checkout has `.git`), and links still refuse it. Then `plugin.json` is
//! validated (`validate`): the name `[a-z0-9-]{2,40}` (for a marketplace install, the name and the
//! version equal to the entry's), every `ui` a `ui://<name>/…` URI, only the known slots with their
//! fields (a command's optional `choices`, its first argument's declared values, included), only
//! the known permissions, `server.command` `node` with a script inside the plugin.
//! Nothing is run, and `npm` never is. The marketplace only replaces an installed version with
//! a strictly newer one; an explicit local archive or folder can still be used to roll back.
//!
//! **Permissions are the owner's.** An install shows what the manifest asks for (`kernel`) and goes
//! ahead only with `approve` naming exactly that list; an upgrade asking for nothing beyond what
//! was approved goes ahead without asking. What runs is what was approved (the `plugins` table),
//! never what a manifest says later. The installed folder moves to `plugins/<name>/` (the old one
//! to `backups/plugins/`), its data lives in `plugin-data/<name>/` and stays when it's removed.
//!
//! **The server** runs as MCP stdio server `plugin-<name>` in the global registry (`mcp.rs`), so
//! agents reach it with `find_tools`/`call_tool` like any server, and only ever in bubblewrap
//! (`command`, `sandbox::plugin_mounts`): its folder read-only, its data folder read-write, the
//! files root (`ZEN_PLUGIN_FILES`, default the vault `~/.zenbot/global`, other agents' vaults
//! hidden) read-only with `files:read`, read-write with `files:write`, read-only but one subfolder
//! with `files:write:<sub>`, nothing of it otherwise, and nothing else of the owner's home (an empty
//! tmpfs: no tokens, no `.ssh`, no zen home). The network through pasta with `network`, as agents
//! have it; without it bubblewrap's own empty network namespace (loopback only). Its environment is
//! cleared: `PATH`, `HOME`, `LANG`, `TMPDIR`, `ZEN_PLUGIN_DATA` and, with a files permission,
//! `ZEN_PLUGIN_FILES`. Host grants are pinned by no-symlink descriptor walks and passed to
//! bubblewrap as bind-fds; scoped creation stays relative to the pinned files root. Without
//! bubblewrap it doesn't start (an error, never unsandboxed).
//!
//! **Sampling (plugin AI).** Only a server granted `ai` is offered sampling at `initialize`, and
//! only its `sampling/createMessage` is answered (mcp.rs `read_stdio`), by `sample`, while the
//! owner's per-plugin AI switch (`plugins.ai_enabled`, on by default, `PATCH { ai }`) is on: the
//! kernel's tool-free completion (`crate::complete`, as `/api/complete`), text only, two at a time
//! across plugins. The model is the assist model unless `modelPreferences.hints` name one the kernel
//! knows. There is no token cap (the owner's call: the CLI engines have no native one, so
//! `maxTokens` is ignored rather than faked). Every request, answered or refused, is recorded in
//! `plugin_calls` (caller `sampling`: model, tokens, cost); Settings shows the last 30 days' count.
//!
//! **The web shell's calls** (`api_*` below, under `/api/plugins`): a UI reads only its own plugin's
//! `ui://<name>/…` resources and calls only its own plugin's tools (the server is looked up by the
//! plugin's name, never by a name the caller gives), and not those whose `_meta.ui.visibility`
//! leaves out `app`. A call has the server's timeout, the 8 MB message cap and the secret masking
//! of an agent's calls, and is recorded in `plugin_calls` (caller `ui`) and, when it names a
//! session, in `tool_calls` as that session's `call_tool` with origin `ui`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::api::{ApiError, ApiResult};
use crate::mcp::ServerConfig;
use crate::sandbox::Access;
use crate::AppState;

/// The largest archive downloaded or read, and the most its files may add up to unpacked.
pub const MAX_ARCHIVE: u64 = 50 * 1024 * 1024;
pub const MAX_UNPACKED: u64 = 50 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_MANIFEST: u64 = 256 * 1024;
/// The marketplace file: fetched within 10 s, at most 1 MB, kept a minute.
const DEFAULT_MARKETPLACE: &str = "https://raw.githubusercontent.com/Zegoverno/zenbot/main/marketplace/plugins.json";
const MARKET_TIMEOUT: Duration = Duration::from_secs(10);
const MARKET_MAX: u64 = 1024 * 1024;
const MARKET_TTL: Duration = Duration::from_secs(60);
/// The longest prompt and system prompt a plugin's sampling request may send (as `/api/complete`).
const SAMPLING_MAX_PROMPT: usize = 100_000;
const SAMPLING_MAX_SYSTEM: usize = 20_000;
/// How long a plugin server's request may take (as an mcp.json server's default).
const SERVER_TIMEOUT: Duration = Duration::from_secs(60);

pub const PERMISSIONS: &[&str] = &["files:read", "files:write", "network", "ai"];

// ---------- the manifest ----------

/// A validated `plugin.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub skills: Option<String>,
    pub name: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub args: Vec<String>,
    /// The slots, as given (validated).
    pub contributes: Value,
    /// The permissions it asks for, sorted, without repeats.
    pub permissions: Vec<String>,
}

/// Whether `n` is a plugin name: `[a-z0-9-]{2,40}`.
pub fn valid_name(n: &str) -> bool {
    (2..=40).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `v` is a semantic version (`1.2.3`, `1.2.3-beta.1`, `1.2.3+build`).
pub fn valid_version(v: &str) -> bool {
    v.split('+').next().is_some_and(|v| v.len() <= 64) && semver::Version::parse(v).is_ok()
}

/// Strict SemVer precedence: build metadata doesn't make a release newer.
fn newer_version(candidate: &str, installed: &str) -> bool {
    match (semver::Version::parse(candidate), semver::Version::parse(installed)) {
        (Ok(candidate), Ok(installed)) => candidate.cmp_precedence(&installed).is_gt(),
        _ => false,
    }
}

/// A relative path made only of plain names, none hidden: what a subfolder permission and the
/// server's script may name.
fn plain_relative(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && Path::new(p).components().all(|c| matches!(c, Component::Normal(n) if n.to_str().is_some_and(|n| !n.starts_with('.') && n != "node_modules")))
}

/// Check a parsed `plugin.json`. The error says what's wrong, for the owner and the author.
pub fn validate(v: &Value) -> Result<Manifest, String> {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let name = s("name");
    if !valid_name(&name) {
        return Err(format!("name `{name}`: 2 to 40 lowercase letters, digits and -"));
    }
    let version = s("version");
    if !valid_version(&version) {
        return Err(format!("version `{version}` isn't a semantic version (1.2.3)"));
    }
    for k in ["title", "description"] {
        if !v[k].is_null() && (!v[k].is_string() || s(k).len() > 1000) {
            return Err(format!("`{k}` must be a string of at most 1000 bytes"));
        }
    }
    let skills = match &v["skills"] {
        Value::Null => None,
        Value::String(p) if plain_relative(p) => Some(p.clone()),
        _ => return Err("skills must be a relative directory inside the plugin".into()),
    };
    let server = &v["server"];
    if server["command"] != "node" {
        return Err("server.command must be `node`".into());
    }
    let args: Vec<String> = match server["args"].as_array() {
        Some(a)
            if !a.is_empty()
                && a.len() <= 32
                && a.iter().all(|x| x.as_str().is_some_and(|x| x.len() <= 512)) =>
        {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        }
        _ => {
            return Err("server.args must be 1 to 32 strings, the first the server's script".into())
        }
    };
    if !plain_relative(&args[0]) || args[0].starts_with('-') {
        return Err(format!("server.args[0] `{}` must be a relative path inside the plugin", args[0]));
    }
    let contributes = if v["contributes"].is_null() { json!({}) } else { v["contributes"].clone() };
    let Some(slots) = contributes.as_object() else { return Err("`contributes` must be an object".into()) };
    let ui_prefix = format!("ui://{name}/");
    let text = |x: &Value, k: &str, max: usize, need: bool| -> Result<(), String> {
        match &x[k] {
            Value::Null if !need => Ok(()),
            Value::String(t) if !t.trim().is_empty() && t.len() <= max => Ok(()),
            _ => Err(format!("`{k}` must be a non-empty string of at most {max} bytes")),
        }
    };
    let id = |x: &Value, k: &str| -> Result<(), String> {
        let i = x[k].as_str().unwrap_or("");
        if i.is_empty() || i.len() > 40 || !i.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            return Err(format!("`{k}` `{i}`: 1 to 40 lowercase letters, digits and -"));
        }
        Ok(())
    };
    for (slot, items) in slots {
        if !["views", "openers", "games", "widgets", "commands"].contains(&slot.as_str()) {
            return Err(format!("contributes.{slot}: not a slot zenbot knows (views, openers, games, widgets, commands)"));
        }
        let Some(items) = items.as_array() else { return Err(format!("contributes.{slot} must be a list")) };
        if items.len() > 50 {
            return Err(format!("contributes.{slot}: at most 50"));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, x) in items.iter().enumerate() {
            let at = |e: String| format!("contributes.{slot}[{i}]: {e}");
            if !x.is_object() {
                return Err(at("must be an object".into()));
            }
            let ui = || -> Result<(), String> {
                match x["ui"].as_str() {
                    Some(u) if u.starts_with(&ui_prefix) && u.len() > ui_prefix.len() && u.len() <= 200 && !u.contains(char::is_whitespace) => Ok(()),
                    _ => Err(format!("`ui` must be a {ui_prefix}… URI")),
                }
            };
            let key = match slot.as_str() {
                "views" => {
                    id(x, "id").and(text(x, "title", 80, true)).and_then(|_| ui()).map_err(at)?;
                    x["id"].clone()
                }
                "openers" => {
                    id(x, "id").and(text(x, "title", 80, false)).and_then(|_| ui()).map_err(at)?;
                    let patterns = x["patterns"].as_array().filter(|p| !p.is_empty() && p.len() <= 20);
                    let good = |p: &Value| p.as_str().is_some_and(|p| !p.is_empty() && p.len() <= 100 && !p.contains(['/', ':']) && !p.chars().any(char::is_control));
                    if !patterns.is_some_and(|p| p.iter().all(good)) {
                        return Err(at("`patterns` must be 1 to 20 file name globs (no / or :)".into()));
                    }
                    if !["read", "edit"].contains(&x["mode"].as_str().unwrap_or("")) {
                        return Err(at("`mode` must be read or edit".into()));
                    }
                    x["id"].clone()
                }
                "games" => {
                    id(x, "id").and(text(x, "title", 80, true)).and(text(x, "description", 300, false)).and_then(|_| ui()).map_err(at)?;
                    x["id"].clone()
                }
                // A card on Home: its `ui://` resource in a frame of the size it asks for (1 to 3 columns).
                "widgets" => {
                    id(x, "id").and(text(x, "title", 80, true)).and(text(x, "description", 300, false)).and_then(|_| ui()).map_err(at)?;
                    if !(x["size"].is_null() || x["size"].as_u64().is_some_and(|n| (1..=3).contains(&n))) {
                        return Err(at("`size` must be 1, 2 or 3 (columns)".into()));
                    }
                    x["id"].clone()
                }
                "commands" => {
                    id(x, "name").and(text(x, "description", 300, false)).and(text(x, "args", 100, false)).map_err(at)?;
                    let tool = x["tool"].as_str().unwrap_or("");
                    if tool.is_empty() || tool.len() > 128 || !tool.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
                        return Err(at("`tool` must name one of the plugin's tools".into()));
                    }
                    validate_choices(x).map_err(at)?;
                    x["name"].clone()
                }
                _ => unreachable!("slots are checked above"),
            };
            if !seen.insert(key.to_string()) {
                return Err(at(format!("{key} is listed twice")));
            }
        }
    }
    let mut permissions: Vec<String> = Vec::new();
    match &v["kernel"] {
        Value::Null => {}
        Value::Array(a) => {
            for p in a {
                let p = p.as_str().unwrap_or("");
                let ok = PERMISSIONS.contains(&p) || p.strip_prefix("files:write:").is_some_and(plain_relative);
                if !ok {
                    return Err(format!("permission `{p}` isn't one zenbot knows (files:read, files:write, files:write:<subfolder>, network, ai)"));
                }
                permissions.push(p.to_string());
            }
        }
        _ => return Err("`kernel` must be a list of permissions".into()),
    }
    permissions.sort();
    permissions.dedup();
    if permissions.iter().filter(|p| p.starts_with("files:write:")).count() > 1 {
        return Err("at most one files:write:<subfolder>".into());
    }
    Ok(Manifest { skills, title: if v["title"].is_string() { s("title") } else { name.clone() }, description: s("description"), name, version, args, contributes, permissions })
}

/// A command's optional `choices`: `{ value, description, then? }` entries (at most 20) that fill the
/// command's first positional argument, so a client with no argument can offer a menu. `value` is
/// one word `[a-z0-9_-]{1,32}`, unique; `description` at most 120 bytes; `then` (at most 60) says the
/// choice needs more input and is an `args` hint. Unknown keys are refused. The command's `args`
/// needs a `<name>` or `[name]` placeholder for the value to fill.
fn validate_choices(x: &Value) -> Result<(), String> {
    let list = match &x["choices"] {
        Value::Null => return Ok(()),
        Value::Array(a) => a,
        _ => return Err("`choices` must be a list".into()),
    };
    if list.is_empty() || list.len() > 20 {
        return Err("`choices` must have 1 to 20 entries".into());
    }
    if !x["args"].as_str().is_some_and(|a| a.contains(['<', '['])) {
        return Err("`choices` need `args` with a <placeholder> for the value to fill".into());
    }
    let mut seen = std::collections::HashSet::new();
    for (i, c) in list.iter().enumerate() {
        let at = |e: String| format!("choices[{i}]: {e}");
        let Some(o) = c.as_object() else { return Err(at("must be an object".into())) };
        if let Some(k) = o.keys().find(|k| !["value", "description", "then"].contains(&k.as_str())) {
            return Err(at(format!("unknown key `{k}` (value, description, then)")));
        }
        let v = c["value"].as_str().unwrap_or("");
        if v.is_empty() || v.len() > 32 || !v.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
            return Err(at(format!("`value` `{v}`: one word of 1 to 32 lowercase letters, digits, _ and -")));
        }
        for (k, max, need) in [("description", 120, true), ("then", 60, false)] {
            match &c[k] {
                Value::Null if !need => {}
                Value::String(t) if !t.trim().is_empty() && t.len() <= max => {}
                _ => return Err(at(format!("`{k}` must be a non-empty string of at most {max} bytes"))),
            }
        }
        if !seen.insert(v) {
            return Err(at(format!("`value` {v} is listed twice")));
        }
    }
    Ok(())
}

// ---------- what runs ----------

/// What a plugin's files permission gives its server.
#[derive(Clone, Debug, PartialEq)]
pub enum Files {
    None,
    Read,
    Write,
    /// Read all, write only this subfolder (relative to the files root).
    WriteSub(String),
}

/// A plugin's server as it runs: from its approved permissions, never its manifest's.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub name: String,
    pub dir: PathBuf,
    pub data: PathBuf,
    pub files: Files,
    pub network: bool,
    pub ai: bool,
}

pub fn run_for(name: &str, permissions: &[String]) -> Run {
    let has = |p: &str| permissions.iter().any(|x| x == p);
    let files = if has("files:write") {
        Files::Write
    } else if let Some(sub) = permissions.iter().find_map(|p| p.strip_prefix("files:write:")) {
        Files::WriteSub(sub.to_string())
    } else if has("files:read") {
        Files::Read
    } else {
        Files::None
    };
    Run { name: name.to_string(), dir: plugins_dir().join(name), data: data_dir().join(name), files, network: has("network"), ai: has("ai") }
}

pub fn plugins_dir() -> PathBuf {
    crate::zen_home().join("plugins")
}

fn data_dir() -> PathBuf {
    crate::zen_home().join("plugin-data")
}

fn backups_dir() -> PathBuf {
    crate::zen_home().join("backups/plugins")
}

/// The files root plugins may be granted: `ZEN_PLUGIN_FILES`, else the vault.
pub fn files_root() -> PathBuf {
    std::env::var("ZEN_PLUGIN_FILES").ok().filter(|p| !p.trim().is_empty()).map(PathBuf::from).unwrap_or_else(crate::layout::global_dir)
}

/// The node runtime, resolved (on `PATH`, else where zenbot's installer puts it).
fn find_node() -> Result<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    dirs.extend([PathBuf::from(&home).join(".local/node/bin"), "/usr/local/bin".into(), "/usr/bin".into()]);
    dirs.into_iter()
        .map(|d| d.join("node"))
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
        .context("plugins run on node, and it isn't installed (on PATH or ~/.local/node/bin)")
}

/// The command starting `run`'s server with `args`, inside its sandbox (never outside it).
pub fn command(run: &Run, args: &[String]) -> Result<tokio::process::Command> {
    use std::os::unix::fs::DirBuilderExt;
    let user_home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    anyhow::ensure!(run.dir.join("plugin.json").is_file(), "plugin `{}` isn't installed in {}", run.name, run.dir.display());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&run.data)?;
    let node = find_node()?;
    // A runtime outside the system mounts may belong to a different HOME (dev/test kernels).
    // Bind only the executable, never its parent: a PATH entry must not expose an owner folder.
    let runtime = (!node.starts_with("/usr") && !node.starts_with("/bin")).then(|| node.clone());
    let root = files_root();
    // Never canonicalize a mutable grant into a different host directory. The
    // descriptor walk below rejects symlinks in every component.
    let real_root = root;
    let (files, sub): (Option<Access>, Option<PathBuf>) = match &run.files {
        Files::None => (None, None),
        Files::Read => (Some(Access::Read), None),
        Files::Write => (Some(Access::Write), None),
        Files::WriteSub(sub) => {
            let path = Path::new(sub);
            anyhow::ensure!(!path.as_os_str().is_empty()
                && path.components().all(|p| matches!(p, std::path::Component::Normal(_)))
                && path.components().next().unwrap().as_os_str() != "agents", "invalid scoped files grant");
            (Some(Access::Read), Some(real_root.join(path)))
        }
    };
    let zen = crate::zen_home();
    let mounts = crate::sandbox::plugin_mounts(&crate::sandbox::PluginLayout {
        user_home: &user_home,
        zen: &zen,
        plugin: &run.dir,
        data: &run.data,
        runtime: runtime.as_deref(),
        files: files.map(|a| (real_root.as_path(), a)),
        write_sub: sub.as_deref(),
        network: run.network,
    });
    let mut sandbox = crate::sandbox::Sandbox::new(&format!("plugin `{}`", run.name), mounts);
    sandbox.pin_plugin_sources(
        &[&run.dir, &run.data],
        files.map(|_| real_root.as_path()),
        runtime.as_deref(),
    )?;
    if let Some(sub) = &sub {
        sandbox.pin_plugin_scope(&real_root, sub)?;
    }
    if !run.network {
        sandbox = sandbox.offline();
    }
    let mut cmd = sandbox.wrap(&run.dir, &node.display().to_string(), args).map_err(|e| anyhow!(e))?;
    let path = format!("{}:/usr/local/bin:/usr/bin:/bin", node.parent().unwrap_or(Path::new("/usr/bin")).display());
    cmd.env_clear()
        .env("PATH", path)
        .env("HOME", &user_home)
        .env("TMPDIR", "/tmp")
        .env("LANG", std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()))
        .env("ZEN_PLUGIN_DATA", &run.data);
    if files.is_some() {
        cmd.env("ZEN_PLUGIN_FILES", &real_root);
    }
    if let Ok(tz) = std::env::var("TZ") {
        cmd.env("TZ", tz);
    }
    Ok(cmd)
}

/// The kernel, for the sampling requests that arrive on a server's own reader task.
static APP: OnceLock<AppState> = OnceLock::new();

/// Enabled installed plugins' skill roots, refreshed with every plugin lifecycle change.
static SKILL_ROOTS: LazyLock<std::sync::RwLock<Vec<PathBuf>>> = LazyLock::new(Default::default);

pub fn skill_roots() -> Vec<PathBuf> {
    SKILL_ROOTS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The enabled plugins' servers and a generation that changes with them (`mcp.rs` reloads on it).
static SERVERS: LazyLock<std::sync::RwLock<(u64, Vec<ServerConfig>)>> = LazyLock::new(Default::default);

/// One install, upgrade or removal at a time.
static CHANGING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

/// Sampling requests answered at once, across plugins (more wait their turn).
static SAMPLING: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(2));

/// The current generation and enabled plugin servers.
pub fn servers() -> (u64, Vec<ServerConfig>) {
    SERVERS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// At start: remember the kernel, clear a half-done install, and run the enabled plugins.
pub async fn init(app: &AppState) -> Result<()> {
    let _ = APP.set(app.clone());
    let _ = std::fs::remove_dir_all(plugins_dir().join(".staging"));
    reload(&app.db).await
}

/// Read the enabled plugins from the database and hand their servers to the MCP registry.
async fn reload(db: &SqlitePool) -> Result<()> {
    let rows = sqlx::query("SELECT name, manifest, permissions FROM plugins WHERE enabled = 1 ORDER BY name").fetch_all(db).await?;
    let mut cfgs = Vec::new();
    let skill_roots = enabled_skill_roots(db, &plugins_dir()).await?;
    for r in rows {
        let name: String = r.get("name");
        let manifest: Value = serde_json::from_str(&r.get::<String, _>("manifest")).unwrap_or_default();
        let permissions: Vec<String> = serde_json::from_str(&r.get::<String, _>("permissions")).unwrap_or_default();
        let args = manifest["server"]["args"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
        cfgs.push(ServerConfig {
            name: format!("plugin-{name}"),
            command: Some("node".into()),
            args,
            env: Vec::new(),
            url: None,
            headers: Vec::new(),
            oauth: None,
            timeout: SERVER_TIMEOUT,
            include: Vec::new(),
            exclude: Vec::new(),
            plugin: Some(run_for(&name, &permissions)),
        });
    }
    {
        let mut s = SERVERS.write().unwrap_or_else(|e| e.into_inner());
        *s = (s.0 + 1, cfgs);
    }
    *SKILL_ROOTS.write().unwrap_or_else(|e| e.into_inner()) = skill_roots;
    crate::mcp::reload_plugins().await;
    Ok(())
}

// ---------- archives and folders ----------

/// Why the archive or folder can't be installed.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refuse<T>(why: impl Into<String>) -> Result<T> {
    Err(Refused(why.into()).into())
}

/// The path an archive entry or a folder's file may have: plain names only. None for the root
/// itself (`./`); refused for anything absolute, `..`, hidden or `node_modules`.
fn entry_path(p: &Path) -> Result<Option<PathBuf>> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(n) => {
                let Some(n) = n.to_str() else { return refuse(format!("{}: names must be UTF-8", p.display())) };
                if n.starts_with('.') {
                    return refuse(format!("{}: dot files aren't allowed", p.display()));
                }
                if n == "node_modules" {
                    return refuse(format!("{}: node_modules isn't allowed (bundle the server into one file)", p.display()));
                }
                out.push(n);
            }
            Component::ParentDir => return refuse(format!("{}: `..` isn't allowed", p.display())),
            Component::RootDir | Component::Prefix(_) => {
                return refuse(format!("{}: absolute paths aren't allowed", p.display()))
            }
        }
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

fn new_file(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // create_new: an entry twice (or a file where a folder was) is refused, never overwritten.
    Ok(std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).custom_flags(libc::O_NOFOLLOW).open(path)?)
}

/// Limit before tar sees any bytes, including GNU/PAX records it consumes internally.
/// Probe one extra byte instead of turning the cap into a misleading clean EOF.
struct UnpackedLimit<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for UnpackedLimit<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut extra = [0];
            return match self.inner.read(&mut extra)? {
                0 => Ok(0),
                _ => Err(std::io::Error::other(
                    Refused(format!("the archive unpacks to over {} MB (decompressed stream exceeds unpacked limit)", MAX_UNPACKED / 1024 / 1024)),
                )),
            };
        }
        let len = buf.len().min(self.remaining as usize);
        let n = self.inner.read(&mut buf[..len])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Unpack a gzipped tar into the empty folder `into`, by the rules above.
pub fn unpack(gz: &[u8], into: &Path) -> Result<()> {
    unpack_inner(gz, into).map_err(|error| {
        for cause in error.chain() {
            if let Some(refusal) = cause.downcast_ref::<std::io::Error>()
                .and_then(|e| e.get_ref()).and_then(|e| e.downcast_ref::<Refused>()) {
                return anyhow!(Refused(refusal.0.clone()));
            }
        }
        error
    })
}

fn unpack_inner(gz: &[u8], into: &Path) -> Result<()> {
    if gz.len() as u64 > MAX_ARCHIVE {
        return refuse(format!("the archive is over {} MB", MAX_ARCHIVE / 1024 / 1024));
    }
    let mut stream = UnpackedLimit {
        inner: flate2::read::MultiGzDecoder::new(gz),
        remaining: MAX_UNPACKED,
    };
    let mut archive = tar::Archive::new(&mut stream);
    let (mut total, mut count) = (0u64, 0usize);
    for entry in archive.entries().context("reading the archive")? {
        let mut entry = entry.context("reading the archive")?;
        count += 1;
        if count > MAX_ENTRIES {
            return refuse(format!("the archive has over {MAX_ENTRIES} entries"));
        }
        let kind = entry.header().entry_type();
        let path = entry.path().context("an entry's path")?.into_owned();
        use tar::EntryType as T;
        match kind {
            T::XGlobalHeader | T::XHeader => continue,
            T::Directory => {
                if let Some(rel) = entry_path(&path)? {
                    std::fs::create_dir_all(into.join(rel))?;
                }
            }
            T::Regular | T::Continuous => {
                let Some(rel) = entry_path(&path)? else { return refuse("a file entry without a name") };
                let mut file = new_file(&into.join(rel))?;
                let n = std::io::copy(&mut (&mut entry).take(MAX_UNPACKED - total + 1), &mut file)?;
                total += n;
                if total > MAX_UNPACKED {
                    return refuse(format!("the archive unpacks to over {} MB", MAX_UNPACKED / 1024 / 1024));
                }
            }
            T::Symlink | T::Link => {
                return refuse(format!("{}: links aren't allowed", path.display()))
            }
            other => {
                return refuse(format!(
                    "{}: {other:?} entries aren't allowed (only files and folders)",
                    path.display()
                ))
            }
        }
    }
    // tar stops at its end marker; count trailing data and concatenated gzip members too.
    std::io::copy(archive.into_inner(), &mut std::io::sink())?;
    Ok(())
}

/// Copy a folder holding a built plugin into the empty folder `into`: dot entries and
/// node_modules are left out; a link, a device or anything over the size cap refuses it.
pub fn copy_folder(from: &Path, into: &Path) -> Result<()> {
    let (mut total, mut count) = (0u64, 0usize);
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for e in std::fs::read_dir(from.join(&rel))? {
            let e = e?;
            let name = e.file_name();
            let Some(name) = name.to_str() else { return refuse(format!("{}: names must be UTF-8", e.path().display())) };
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            count += 1;
            if count > MAX_ENTRIES {
                return refuse(format!("the folder has over {MAX_ENTRIES} entries"));
            }
            let rel = rel.join(name);
            let meta = std::fs::symlink_metadata(e.path())?;
            if meta.file_type().is_symlink() {
                return refuse(format!("{}: links aren't allowed", rel.display()));
            } else if meta.is_dir() {
                std::fs::create_dir_all(into.join(&rel))?;
                stack.push(rel);
            } else if meta.is_file() {
                use std::os::unix::fs::OpenOptionsExt;
                let mut src = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(e.path())?;
                let mut file = new_file(&into.join(&rel))?;
                total += std::io::copy(&mut (&mut src).take(MAX_UNPACKED - total + 1), &mut file)?;
                if total > MAX_UNPACKED {
                    return refuse(format!("the folder is over {} MB", MAX_UNPACKED / 1024 / 1024));
                }
            } else {
                return refuse(format!("{}: only files and folders are allowed", rel.display()));
            }
        }
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// An archive's bytes checked against the sha256 the marketplace pins.
pub fn check_sha256(bytes: &[u8], want: &str) -> Result<()> {
    let got = sha256_hex(bytes);
    if !want.eq_ignore_ascii_case(&got) {
        return refuse(format!("the archive's sha256 is {got}, not the {want} the marketplace pins"));
    }
    Ok(())
}

async fn enabled_skill_roots(db: &SqlitePool, base: &Path) -> Result<Vec<PathBuf>> {
    let rows = sqlx::query("SELECT name, manifest, permissions FROM plugins WHERE enabled = 1 ORDER BY name").fetch_all(db).await?;
    Ok(rows.into_iter().filter_map(|r| {
        let manifest: Value = serde_json::from_str(&r.get::<String, _>("manifest")).ok()?;
        let permissions: Vec<String> = serde_json::from_str(&r.get::<String, _>("permissions")).ok()?;
        installed_skill_root(base, &r.get::<String, _>("name"), &manifest, &permissions)
    }).collect())
}

fn installed_skill_root(base: &Path, name: &str, manifest: &Value, permissions: &[String]) -> Option<PathBuf> {
    if !valid_name(name) { return None; }
    let m = validate(manifest).ok()?;
    if m.name != name || !m.permissions.iter().all(|p| permissions.contains(p)) { return None; }
    let root = base.join(name);
    let dir = root.join(m.skills?);
    (safe_skill_path(base, &dir) && dir.is_dir() && std::fs::read_dir(&dir).is_ok()).then_some(dir)
}

/// Reject symlinks in every component. These are trusted installed files, not a sandbox
/// against concurrent mutation by another process running as the owner.
pub(crate) fn safe_skill_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else { return false };
    let mut current = root.to_path_buf();
    if std::fs::symlink_metadata(&current).map_or(true, |m| m.file_type().is_symlink()) { return false; }
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) { return false; }
        current.push(component);
        if std::fs::symlink_metadata(&current).map_or(true, |m| m.file_type().is_symlink()) { return false; }
    }
    true
}

/// Read the staged plugin's manifest and check it, with its server's script.
fn staged_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join("plugin.json");
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return refuse("no plugin.json at the top of the plugin") };
    if !meta.is_file() || meta.len() > MAX_MANIFEST {
        return refuse("plugin.json must be a file of at most 256 KB");
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path)?).map_err(|e| Refused(format!("plugin.json isn't valid JSON: {e}")))?;
    let m = validate(&v).map_err(|e| Refused(format!("plugin.json: {e}")))?;
    if !dir.join(&m.args[0]).is_file() {
        return refuse(format!("plugin.json: the server's script {} isn't in the plugin", m.args[0]));
    }
    if let Some(path) = &m.skills {
        let skills = dir.join(path);
        if !safe_skill_path(dir, &skills) || !skills.is_dir() {
            return refuse("plugin.json: skills must be a non-symlink directory inside the plugin");
        }
    }
    Ok(m)
}

// ---------- the marketplace ----------

static MARKET: LazyLock<tokio::sync::Mutex<Option<(Instant, String, Value)>>> = LazyLock::new(Default::default);

fn marketplace_url() -> String {
    std::env::var("ZEN_MARKETPLACE").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| DEFAULT_MARKETPLACE.into())
}

/// Whether `url` may be fetched: https, or http to this machine (a local marketplace, for tests).
fn fetchable(url: &reqwest::Url) -> bool {
    url.scheme() == "https" || (url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
}

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    // Release assets redirect to GitHub's storage: follow a few, never down to plain http.
    let policy = reqwest::redirect::Policy::custom(|a| if a.previous().len() >= 5 || !fetchable(a.url()) { a.stop() } else { a.follow() });
    reqwest::Client::builder().redirect(policy).connect_timeout(MARKET_TIMEOUT).build().expect("http client")
});

/// GET `url` within `timeout`, at most `max` bytes.
async fn fetch(url: &str, timeout: Duration, max: u64) -> Result<Vec<u8>> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("`{url}` isn't a URL"))?;
    anyhow::ensure!(fetchable(&parsed), "{url}: only https (or http on this machine)");
    let body = async {
        let mut resp = HTTP.get(parsed).send().await?;
        anyhow::ensure!(resp.status().is_success(), "HTTP {}", resp.status());
        anyhow::ensure!(resp.content_length().is_none_or(|n| n <= max), "over {} KB", max / 1024);
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            anyhow::ensure!((body.len() + chunk.len()) as u64 <= max, "over {} KB", max / 1024);
            body.extend_from_slice(&chunk);
        }
        Ok::<_, anyhow::Error>(body)
    };
    tokio::time::timeout(timeout, body).await.map_err(|_| anyhow!("no answer within {}s", timeout.as_secs()))?.with_context(|| format!("fetching {url}"))
}

/// The marketplace file (`ZEN_MARKETPLACE`: a URL, or a local file for development), kept a minute.
pub async fn marketplace() -> Result<Value> {
    let url = marketplace_url();
    let mut cache = MARKET.lock().await;
    if let Some((at, u, v)) = cache.as_ref() {
        if *u == url && at.elapsed() < MARKET_TTL {
            return Ok(v.clone());
        }
    }
    let bytes = if url.starts_with("http://") || url.starts_with("https://") {
        fetch(&url, MARKET_TIMEOUT, MARKET_MAX).await?
    } else {
        let path = url.strip_prefix("file://").unwrap_or(&url);
        let mut out = Vec::new();
        std::fs::File::open(path).with_context(|| format!("reading the marketplace {path}"))?.take(MARKET_MAX + 1).read_to_end(&mut out)?;
        anyhow::ensure!(out.len() as u64 <= MARKET_MAX, "the marketplace file is over 1 MB");
        out
    };
    let v: Value = serde_json::from_slice(&bytes).context("the marketplace isn't valid JSON")?;
    anyhow::ensure!(v["plugins"].is_array(), "the marketplace has no `plugins` list");
    *cache = Some((Instant::now(), url, v.clone()));
    Ok(v)
}

// ---------- install, remove, enable ----------

/// Where a plugin comes from.
#[derive(Debug)]
pub enum Source {
    Marketplace(String),
    Archive(PathBuf),
    Folder(PathBuf),
}

pub enum Outcome {
    /// Nothing installed: the owner must approve these.
    NeedsApproval { name: String, version: String, permissions: Vec<String>, upgrade: bool },
    Installed(Value),
}

/// A staging folder, removed unless kept.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Install or upgrade a plugin. Without `approve`, or with a list that isn't exactly what it asks
/// for, a plugin asking for permissions not yet approved isn't installed.
pub async fn install(db: &SqlitePool, source: Source, approve: Option<Vec<String>>) -> Result<Outcome> {
    let _changing = CHANGING.lock().await;
    let staging = Staging(plugins_dir().join(".staging").join(Uuid::new_v4().to_string()));
    std::fs::create_dir_all(&staging.0)?;
    let (manifest, from) = match &source {
        Source::Marketplace(name) => {
            let market = marketplace().await?;
            let Some(entry) = market["plugins"].as_array().into_iter().flatten().find(|e| e["name"] == name.as_str()).cloned() else {
                return refuse(format!("`{name}` isn't in the marketplace"));
            };
            let candidate = entry["version"].as_str().unwrap_or("");
            if !valid_version(candidate) {
                return refuse(format!("the marketplace version for `{name}` is not valid SemVer"));
            }
            let installed: Option<String> = sqlx::query_scalar("SELECT version FROM plugins WHERE name = $1")
                .bind(name).fetch_optional(db).await?;
            if let Some(installed) = installed {
                if !newer_version(candidate, &installed) {
                    return refuse(format!("marketplace `{name}` {candidate} is not newer than installed {installed}"));
                }
            }
            let (url, sha) = (entry["archive"].as_str().unwrap_or(""), entry["sha256"].as_str().unwrap_or(""));
            if sha.len() != 64 {
                return refuse(format!("the marketplace entry for `{name}` has no sha256"));
            }
            let bytes = fetch(url, Duration::from_secs(300), MAX_ARCHIVE).await?;
            check_sha256(&bytes, sha)?;
            let dir = staging.0.clone();
            tokio::task::spawn_blocking(move || unpack(&bytes, &dir)).await??;
            let m = staged_manifest(&staging.0)?;
            if m.name != *name || Some(m.version.as_str()) != entry["version"].as_str() {
                return refuse(format!("the archive holds {} {}, not the {name} {} the marketplace lists", m.name, m.version, entry["version"].as_str().unwrap_or("?")));
            }
            (m, "marketplace")
        }
        Source::Archive(path) => {
            let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
            if meta.len() > MAX_ARCHIVE {
                return refuse(format!("the archive is over {} MB", MAX_ARCHIVE / 1024 / 1024));
            }
            let bytes = std::fs::read(path)?;
            let dir = staging.0.clone();
            tokio::task::spawn_blocking(move || unpack(&bytes, &dir)).await??;
            (staged_manifest(&staging.0)?, "archive")
        }
        Source::Folder(path) => {
            let (from, dir) = (path.clone(), staging.0.clone());
            tokio::task::spawn_blocking(move || copy_folder(&from, &dir)).await??;
            (staged_manifest(&staging.0)?, "folder")
        }
    };
    let name = manifest.name.clone();
    let old = sqlx::query("SELECT version, permissions, enabled FROM plugins WHERE name = $1").bind(&name).fetch_optional(db).await?;
    let approved: Vec<String> = old.as_ref().and_then(|r| serde_json::from_str(&r.get::<String, _>("permissions")).ok()).unwrap_or_default();
    let covered = old.is_some() && manifest.permissions.iter().all(|p| approved.contains(p));
    if !covered {
        let Some(mut given) = approve else {
            return Ok(Outcome::NeedsApproval { name, version: manifest.version, permissions: manifest.permissions, upgrade: old.is_some() });
        };
        given.sort();
        given.dedup();
        if given != manifest.permissions {
            return Ok(Outcome::NeedsApproval { name, version: manifest.version, permissions: manifest.permissions, upgrade: old.is_some() });
        }
    }
    // Stop the old server before its folder moves.
    let enabled = old.as_ref().is_none_or(|r| r.get::<bool, _>("enabled"));
    if old.is_some() {
        sqlx::query("UPDATE plugins SET enabled = 0 WHERE name = $1").bind(&name).execute(db).await?;
        reload(db).await?;
    }
    let dest = plugins_dir().join(&name);
    if dest.exists() {
        let version = old.as_ref().map(|r| r.get::<String, _>("version")).unwrap_or_else(|| "unknown".into());
        backup(&dest, &name, &version)?;
    }
    std::fs::rename(&staging.0, &dest)?;
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(data_dir().join(&name))?;
    }
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(dest.join("plugin.json"))?)?;
    sqlx::query(
        "INSERT INTO plugins (name, version, manifest, permissions, enabled, source) VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT(name) DO UPDATE SET version = excluded.version, manifest = excluded.manifest, permissions = excluded.permissions,
           enabled = excluded.enabled, source = excluded.source, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')",
    )
    .bind(&name)
    .bind(&manifest.version)
    .bind(raw.to_string())
    .bind(json!(manifest.permissions).to_string())
    .bind(enabled)
    .bind(from)
    .execute(db)
    .await?;
    // Installing an opener is an explicit choice to use it, per pattern and mode.
    // Upgrades retain the owner's choices; removal already clears this plugin's defaults.
    if old.is_none() {
        install_defaults(db, &name, &raw).await?;
    }
    reload(db).await?;
    tracing::info!(plugin = %name, version = %manifest.version, from, "plugin installed");
    Ok(Outcome::Installed(one(db, &name).await?.context("the plugin went missing")?))
}

/// A newly installed opener becomes the default, replacing even a prior plugin choice.
async fn install_defaults(db: &SqlitePool, name: &str, manifest: &Value) -> Result<()> {
    for o in manifest["contributes"]["openers"].as_array().into_iter().flatten() {
        let id = o["id"].as_str().context("opener id")?;
        let mode = o["mode"].as_str().context("opener mode")?;
        for pattern in o["patterns"].as_array().into_iter().flatten() {
            let key = format!("{}:{mode}", pattern.as_str().context("opener pattern")?);
            sqlx::query("INSERT INTO plugin_defaults (key, value) VALUES ($1, $2) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')")
                .bind(key).bind(format!("{name}/{id}")).execute(db).await?;
        }
    }
    Ok(())
}

/// Move an installed plugin's folder to `backups/plugins/<name>-<version>-<time>`.
fn backup(dir: &Path, name: &str, version: &str) -> Result<PathBuf> {
    let to = backups_dir().join(format!("{name}-{version}-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")));
    std::fs::create_dir_all(backups_dir())?;
    std::fs::rename(dir, &to).with_context(|| format!("moving {} to {}", dir.display(), to.display()))?;
    Ok(to)
}

/// Remove a plugin: its server stops, its folder moves to backups, its data stays. The defaults
/// naming it go back to the built-in.
pub async fn remove(db: &SqlitePool, name: &str) -> Result<Option<PathBuf>> {
    let _changing = CHANGING.lock().await;
    let Some(row) = sqlx::query("SELECT version FROM plugins WHERE name = $1").bind(name).fetch_optional(db).await? else { return Ok(None) };
    sqlx::query("DELETE FROM plugins WHERE name = $1").bind(name).execute(db).await?;
    sqlx::query("DELETE FROM plugin_defaults WHERE value LIKE $1 || '/%'").bind(name).execute(db).await?;
    reload(db).await?;
    let dir = plugins_dir().join(name);
    let moved = if dir.exists() { Some(backup(&dir, name, &row.get::<String, _>("version"))?) } else { Some(dir) };
    tracing::info!(plugin = %name, "plugin removed");
    Ok(moved)
}

fn plugin_json(r: &sqlx::sqlite::SqliteRow) -> Value {
    let manifest: Value = serde_json::from_str(&r.get::<String, _>("manifest")).unwrap_or_default();
    let permissions: Value = serde_json::from_str(&r.get::<String, _>("permissions")).unwrap_or_default();
    let name: String = r.get("name");
    json!({
        "name": name,
        "version": r.get::<String, _>("version"),
        "title": manifest["title"].as_str().unwrap_or(&name),
        "description": manifest["description"].as_str().unwrap_or(""),
        "contributes": if manifest["contributes"].is_object() { manifest["contributes"].clone() } else { json!({}) },
        "permissions": permissions,
        "enabled": r.get::<bool, _>("enabled"),
        "ai_enabled": r.get::<bool, _>("ai_enabled"),
        "ai_calls_30d": r.get::<i64, _>("ai_calls_30d"),
        "source": r.get::<String, _>("source"),
        "installed_at": r.get::<String, _>("installed_at"),
        "updated_at": r.get::<String, _>("updated_at"),
    })
}

/// A plugin's row, with its answered sampling requests of the last 30 days.
const COLUMNS: &str = "name, version, manifest, permissions, enabled, ai_enabled, source, installed_at, updated_at,
    (SELECT count(*) FROM plugin_calls c WHERE c.plugin = plugins.name AND c.caller = 'sampling' AND c.is_error = 0
       AND c.created_at >= strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-30 days')) AS ai_calls_30d";

async fn one(db: &SqlitePool, name: &str) -> Result<Option<Value>> {
    Ok(sqlx::query(&format!("SELECT {COLUMNS} FROM plugins WHERE name = $1")).bind(name).fetch_optional(db).await?.as_ref().map(plugin_json))
}

async fn all(db: &SqlitePool) -> Result<Vec<Value>> {
    Ok(sqlx::query(&format!("SELECT {COLUMNS} FROM plugins ORDER BY name")).fetch_all(db).await?.iter().map(plugin_json).collect())
}

// ---------- sampling ----------

/// The text of a sampling message's content (a block or a list of blocks): text only.
fn content_text(c: &Value) -> Result<String> {
    let blocks: Vec<&Value> = match c {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for b in blocks {
        anyhow::ensure!(b["type"] == "text", "only text messages can be sampled here");
        out.push(b["text"].as_str().unwrap_or(""));
    }
    Ok(out.join("\n"))
}

/// The model for a sampling request: the first of `modelPreferences.hints` that names a model the
/// kernel knows (an exact id, or a family such as `sonnet` matched against the configured models,
/// then the catalog), else the assist model.
fn sampling_model(params: &Value, known: &[String]) -> String {
    let config = crate::config::get();
    let configured = [config.assist_model.as_str(), config.summary_model.as_str()];
    let hints = params["modelPreferences"]["hints"].as_array().into_iter().flatten().filter_map(|h| h["name"].as_str());
    for hint in hints.map(|h| h.trim().to_lowercase()).filter(|h| !h.is_empty()) {
        if let Some(id) = known.iter().find(|id| id.to_lowercase() == hint || id.rsplit('/').next().is_some_and(|m| m.to_lowercase() == hint)) {
            return id.clone();
        }
        for family in ["opus", "sonnet", "haiku"] {
            if hint.contains(family) {
                if let Some(id) = configured.iter().find(|m| m.contains(family)) {
                    return id.to_string();
                }
                if let Some(id) = known.iter().filter(|id| id.contains(family)).max() {
                    return id.clone();
                }
            }
        }
    }
    config.assist_model.clone()
}

/// One sampling request in `plugin_calls`: the text answered (or the refusal), model, tokens, cost.
async fn record(db: &SqlitePool, plugin: &str, is_error: bool, ms: i64, text: &str, model: Option<&str>, usage: &Value) -> Result<()> {
    sqlx::query(
        "INSERT INTO plugin_calls (plugin, caller, is_error, duration_ms, output_bytes, model, input_tokens, output_tokens, cost_usd, result_sha256)
         VALUES ($1, 'sampling', $2, $3, $4, $5, $6, $7, $8, sha256_hex($9))",
    )
    .bind(plugin)
    .bind(is_error)
    .bind(ms)
    .bind(text.len() as i64)
    .bind(model)
    .bind(usage["input"].as_i64())
    .bind(usage["output"].as_i64())
    .bind(usage["cost_usd"].as_f64())
    .bind(text)
    .execute(db)
    .await?;
    Ok(())
}

/// Answer a plugin server's `sampling/createMessage` (mcp.rs asks only for a plugin granted `ai`):
/// the kernel's tool-free completion, text only, while the owner's AI switch for the plugin is on.
/// No token cap (the CLI engines have no native one; `maxTokens` is ignored); every call is
/// recorded in `plugin_calls` with its model, tokens and cost.
pub async fn sample(plugin: &str, params: &Value) -> Result<Value> {
    let app = APP.get().context("the kernel isn't ready")?;
    let allowed = servers().1.iter().any(|c| c.plugin.as_ref().is_some_and(|r| r.name == plugin && r.ai));
    anyhow::ensure!(allowed, "plugin `{plugin}` may not ask for completions (it has no `ai` permission)");
    let on: bool = sqlx::query_scalar("SELECT ai_enabled FROM plugins WHERE name = $1").bind(plugin).fetch_optional(&app.db).await?.unwrap_or(false);
    if !on {
        let why = format!("AI is turned off for this plugin (`{plugin}`); the owner can turn it on in Settings → Plugins");
        record(&app.db, plugin, true, 0, &why, None, &Value::Null).await?;
        anyhow::bail!("{why}");
    }
    let messages = params["messages"].as_array().filter(|m| !m.is_empty()).context("no messages")?;
    let mut parts = Vec::new();
    for m in messages {
        let text = content_text(&m["content"])?;
        parts.push(if messages.len() == 1 { text } else { format!("{}: {text}", if m["role"] == "assistant" { "Assistant" } else { "User" }) });
    }
    let prompt = parts.join("\n\n");
    let system = params["systemPrompt"].as_str().unwrap_or("").trim().to_string();
    anyhow::ensure!(!prompt.trim().is_empty() && prompt.len() <= SAMPLING_MAX_PROMPT && system.len() <= SAMPLING_MAX_SYSTEM, "the request is empty or too long");
    let known: Vec<String> = app.catalog.lock().await.keys().cloned().collect();
    let model = sampling_model(params, &known);
    let _turn = SAMPLING.acquire().await?;
    let started = Instant::now();
    let reply = crate::complete(app, &model, &system, &prompt).await;
    let ms = started.elapsed().as_millis() as i64;
    match reply {
        Ok(r) => {
            let answered = if r.model.is_empty() { model } else { r.model };
            tracing::info!(purpose = %format!("plugin {plugin} sampling"), model = %answered, ms, usage = %r.usage, "completion for a plugin");
            if let Err(e) = record(&app.db, plugin, false, ms, &r.text, Some(&answered), &r.usage).await {
                tracing::error!("recording plugin `{plugin}`'s sampling: {e:#}");
            }
            Ok(json!({ "role": "assistant", "content": { "type": "text", "text": r.text }, "model": answered, "stopReason": "endTurn" }))
        }
        Err(e) => {
            if let Err(e) = record(&app.db, plugin, true, ms, &format!("{e:#}"), Some(&model), &Value::Null).await {
                tracing::error!("recording plugin `{plugin}`'s sampling: {e:#}");
            }
            Err(e)
        }
    }
}

// ---------- HTTP (`/api/plugins`) ----------

fn bad(why: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, why.into())
}

fn missing(name: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("plugin `{name}` isn't installed"))
}

/// An install's error: a refused archive or manifest is the client's (422), the rest a failure.
fn install_error(e: anyhow::Error) -> ApiError {
    match e.downcast_ref::<Refused>() {
        Some(r) => ApiError(StatusCode::UNPROCESSABLE_ENTITY, r.0.clone()),
        None => ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")),
    }
}

async fn defaults(db: &SqlitePool) -> Result<Value> {
    let rows = sqlx::query("SELECT key, value FROM plugin_defaults ORDER BY key").fetch_all(db).await?;
    Ok(Value::Object(rows.iter().map(|r| (r.get::<String, _>("key"), json!(r.get::<String, _>("value")))).collect()))
}

/// `GET /api/plugins`: the installed plugins and the owner's defaults.
pub(crate) async fn api_list(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "plugins": all(&app.db).await?, "defaults": defaults(&app.db).await? })))
}

#[derive(Deserialize)]
pub(crate) struct DefaultBody {
    key: String,
    value: Option<String>,
}

/// `PUT /api/plugins/defaults` `{ key: "<pattern>:<mode>", value: "<plugin>/<opener id>" | "builtin" | null }`.
pub(crate) async fn api_set_default(State(app): State<AppState>, Json(b): Json<DefaultBody>) -> ApiResult<Json<Value>> {
    let Some((pattern, mode)) = b.key.rsplit_once(':').filter(|(p, m)| !p.is_empty() && p.len() <= 100 && ["read", "edit"].contains(m)) else {
        return Err(bad("key must be `<pattern>:<mode>`, the mode read or edit"));
    };
    match b.value.as_deref() {
        None => {
            sqlx::query("DELETE FROM plugin_defaults WHERE key = $1").bind(&b.key).execute(&app.db).await?;
        }
        Some(value) => {
            if value != "builtin" {
                let Some((plugin, id)) = value.split_once('/') else { return Err(bad("value must be `<plugin>/<opener id>` or `builtin`")) };
                let p = one(&app.db, plugin).await?.ok_or_else(|| missing(plugin))?;
                let fits = p["contributes"]["openers"].as_array().into_iter().flatten().any(|o| {
                    o["id"] == id && o["mode"] == mode && o["patterns"].as_array().into_iter().flatten().any(|x| x == pattern)
                });
                if !fits {
                    return Err(bad(format!("plugin `{plugin}` has no opener `{id}` for {pattern} in {mode} mode")));
                }
            }
            sqlx::query("INSERT INTO plugin_defaults (key, value) VALUES ($1, $2) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')")
                .bind(&b.key)
                .bind(value)
                .execute(&app.db)
                .await?;
        }
    }
    Ok(Json(json!({ "defaults": defaults(&app.db).await? })))
}

#[derive(Deserialize)]
pub(crate) struct PatchBody {
    enabled: Option<bool>,
    /// The AI switch for a plugin granted `ai`: off refuses its sampling requests.
    ai: Option<bool>,
}

/// `PATCH /api/plugins/{name}` `{ enabled?, ai? }`: start or stop its server; turn its AI on or off.
pub(crate) async fn api_patch(State(app): State<AppState>, UrlPath(name): UrlPath<String>, Json(b): Json<PatchBody>) -> ApiResult<Json<Value>> {
    let _changing = CHANGING.lock().await;
    if b.enabled.is_none() && b.ai.is_none() {
        return Err(bad("nothing to change: give `enabled` or `ai`"));
    }
    let done = sqlx::query(
        "UPDATE plugins SET enabled = coalesce($2, enabled), ai_enabled = coalesce($3, ai_enabled),
         updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE name = $1",
    )
    .bind(&name)
    .bind(b.enabled)
    .bind(b.ai)
    .execute(&app.db)
    .await?;
    if done.rows_affected() == 0 {
        return Err(missing(&name));
    }
    if let Some(ai) = b.ai {
        tracing::info!(plugin = %name, ai, "plugin AI switched");
    }
    if b.enabled.is_some() {
        reload(&app.db).await?;
    }
    Ok(Json(one(&app.db, &name).await?.ok_or_else(|| missing(&name))?))
}

/// `GET /api/plugins/marketplace`: the marketplace file, each entry with `installed` (its
/// installed version or null) and `update_available` (strictly newer than the installed version).
pub(crate) async fn api_marketplace(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let mut market = marketplace().await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    let installed: HashMap<String, String> = sqlx::query("SELECT name, version FROM plugins").fetch_all(&app.db).await?.iter().map(|r| (r.get("name"), r.get("version"))).collect();
    for e in market["plugins"].as_array_mut().into_iter().flatten() {
        let v = e["name"].as_str().and_then(|n| installed.get(n)).cloned();
        e["update_available"] = json!(v.as_deref().zip(e["version"].as_str())
            .is_some_and(|(installed, candidate)| valid_version(candidate) && newer_version(candidate, installed)));
        e["installed"] = json!(v);
    }
    Ok(Json(market))
}

#[derive(Deserialize)]
pub(crate) struct InstallBody {
    /// A marketplace name.
    name: Option<String>,
    /// Or an absolute path on this machine: a `.tgz` or a folder holding a built plugin.
    path: Option<String>,
    #[serde(default)]
    approve: bool,
    /// With `approve`, the permissions being approved: exactly what the plugin asks for.
    permissions: Option<Vec<String>>,
}

/// `POST /api/plugins/install` `{ name | path, approve?, permissions? }`.
pub(crate) async fn api_install(State(app): State<AppState>, Json(b): Json<InstallBody>) -> ApiResult<Json<Value>> {
    let source = match (b.name.as_deref(), b.path.as_deref()) {
        (Some(n), None) if valid_name(n) => Source::Marketplace(n.to_string()),
        (None, Some(p)) => {
            let p = PathBuf::from(p);
            if !p.is_absolute() {
                return Err(bad("path must be absolute"));
            }
            match std::fs::metadata(&p) {
                Ok(m) if m.is_dir() => Source::Folder(p),
                Ok(m) if m.is_file() => Source::Archive(p),
                _ => return Err(bad(format!("{} isn't a file or a folder", p.display()))),
            }
        }
        _ => return Err(bad("give a marketplace `name` or a local `path`")),
    };
    let approve = if b.approve { Some(b.permissions.unwrap_or_default()) } else { None };
    match install(&app.db, source, approve).await.map_err(install_error)? {
        Outcome::NeedsApproval { name, version, permissions, upgrade } => Ok(Json(json!({ "needs_approval": true, "name": name, "version": version, "permissions": permissions, "upgrade": upgrade }))),
        Outcome::Installed(p) => Ok(Json(p)),
    }
}

/// `DELETE /api/plugins/{name}`.
pub(crate) async fn api_remove(State(app): State<AppState>, UrlPath(name): UrlPath<String>) -> ApiResult<Json<Value>> {
    match remove(&app.db, &name).await? {
        Some(to) => Ok(Json(json!({ "removed": name, "moved_to": to }))),
        None => Err(missing(&name)),
    }
}

#[derive(Deserialize)]
pub(crate) struct ResourceQuery {
    uri: String,
}

/// Whether the plugin is installed and enabled (its server is one the registry runs).
async fn running(db: &SqlitePool, name: &str) -> ApiResult<()> {
    let enabled: Option<bool> = sqlx::query_scalar("SELECT enabled FROM plugins WHERE name = $1").bind(name).fetch_optional(db).await?;
    match enabled {
        None => Err(missing(name)),
        Some(false) => Err(ApiError(StatusCode::CONFLICT, format!("plugin `{name}` is disabled"))),
        Some(true) => Ok(()),
    }
}

/// `GET /api/plugins/{name}/resource?uri=ui://<name>/…` → `{ uri, mimeType, text, _meta }`.
pub(crate) async fn api_resource(State(app): State<AppState>, UrlPath(name): UrlPath<String>, Query(q): Query<ResourceQuery>) -> ApiResult<Json<Value>> {
    let prefix = format!("ui://{name}/");
    if !q.uri.starts_with(&prefix) || q.uri.len() == prefix.len() {
        return Err(ApiError(StatusCode::FORBIDDEN, format!("a plugin's UI reads only its own {prefix}… resources")));
    }
    running(&app.db, &name).await?;
    let r = crate::mcp::plugin_request(&name, "resources/read", json!({ "uri": q.uri })).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    let contents = r["contents"].as_array().cloned().unwrap_or_default();
    let Some(c) = contents.iter().find(|c| c["uri"] == q.uri.as_str()).or(contents.first()) else {
        return Err(ApiError(StatusCode::NOT_FOUND, format!("{} has no contents", q.uri)));
    };
    let text = match (c["text"].as_str(), c["blob"].as_str()) {
        (Some(t), _) => t.to_string(),
        (None, Some(b)) => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(b).map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "the resource's blob isn't base64".into()))?;
            String::from_utf8(bytes).map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "the resource isn't text".into()))?
        }
        _ => {
            return Err(ApiError(
                StatusCode::BAD_GATEWAY,
                "the resource has neither text nor blob".into(),
            ))
        }
    };
    Ok(Json(json!({ "uri": q.uri, "mimeType": c["mimeType"], "text": text, "_meta": c["_meta"] })))
}

#[derive(Deserialize)]
pub(crate) struct CallBody {
    tool: String,
    #[serde(default)]
    arguments: Value,
    session: Option<String>,
}

/// Whether a tool's UI may call it: `_meta.ui.visibility` absent, or listing `app`.
pub fn app_visible(meta: &Value) -> bool {
    match meta["ui"]["visibility"].as_array() {
        Some(v) => v.iter().any(|x| x == "app"),
        None => true,
    }
}

/// `POST /api/plugins/{name}/call` `{ tool, arguments, session? }` → the tool's `CallToolResult`.
pub(crate) async fn api_call(State(app): State<AppState>, UrlPath(name): UrlPath<String>, Json(b): Json<CallBody>) -> ApiResult<Json<Value>> {
    running(&app.db, &name).await?;
    let args = match b.arguments {
        Value::Null => json!({}),
        Value::Object(_) => b.arguments,
        _ => return Err(bad("arguments must be an object")),
    };
    let session = match b.session.as_deref() {
        None => None,
        Some(s) => {
            let id = Uuid::parse_str(s).map_err(|_| bad("session must be a session id"))?;
            let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sessions WHERE id = $1").bind(id).fetch_optional(&app.db).await?;
            known.ok_or_else(crate::api::not_found)?;
            Some(id)
        }
    };
    let tools = crate::mcp::plugin_tools(&name).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    // Only this plugin's server is asked, so another plugin's tool isn't found here.
    let Some(tool) = tools.iter().find(|t| t.name == b.tool) else {
        return Err(ApiError(StatusCode::NOT_FOUND, format!("plugin `{name}` has no tool `{}`", b.tool)));
    };
    if !app_visible(&tool.meta) {
        return Err(ApiError(StatusCode::FORBIDDEN, format!("`{}` isn't for the plugin's UI (_meta.ui.visibility)", b.tool)));
    }
    let started = Instant::now();
    let result = crate::mcp::plugin_request(&name, "tools/call", json!({ "name": b.tool, "arguments": args })).await;
    let ms = started.elapsed().as_millis() as i64;
    let (result, failed) = match result {
        Ok(r) => {
            // The same masking as an agent's calls, over the whole result.
            let masked = crate::secrets::mask_off_thread(r.to_string()).await;
            (serde_json::from_str(&masked).unwrap_or_else(|_| json!({ "content": [{ "type": "text", "text": "(the result was withheld: masking secrets broke it)" }], "isError": true })), None)
        }
        Err(e) => (Value::Null, Some(crate::secrets::mask_off_thread(format!("{e:#}")).await)),
    };
    let text = result.to_string();
    let is_error = failed.is_some() || result["isError"] == true;
    let recorded = sqlx::query(
        "INSERT INTO plugin_calls (plugin, caller, tool, session_id, is_error, duration_ms, output_bytes, result_sha256) VALUES ($1, 'ui', $2, $3, $4, $5, $6, sha256_hex($7))",
    )
    .bind(&name)
    .bind(&b.tool)
    .bind(session)
    .bind(is_error)
    .bind(ms)
    .bind(text.len() as i64)
    .bind(&text)
    .execute(&app.db)
    .await;
    if let Err(e) = recorded {
        tracing::error!("recording plugin `{name}`'s UI call: {e:#}");
    }
    if let Some(id) = session {
        let shown = crate::secrets::mask(&json!({ "name": format!("plugin-{name}_{}", b.tool), "arguments": args }).to_string());
        let recorded = sqlx::query(
            "INSERT INTO tool_calls (session_id, call_id, name, args, is_error, duration_ms, output_bytes, result_sha256, origin) VALUES ($1, $2, 'call_tool', $3, $4, $5, $6, sha256_hex($7), 'ui')",
        )
        .bind(id)
        .bind(format!("ui-{}", Uuid::new_v4()))
        .bind(shown)
        .bind(is_error)
        .bind(ms)
        .bind(text.len() as i64)
        .bind(&text)
        .execute(&app.db)
        .await;
        if let Err(e) = recorded {
            tracing::error!("recording plugin `{name}`'s UI call in session {id}: {e:#}");
        }
    }
    match failed {
        Some(e) => Err(ApiError(StatusCode::BAD_GATEWAY, e)),
        None => Ok(Json(result)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_openers_replace_defaults_per_mode_and_remove_clears_them() {
        let (_dir, db) = crate::test_util::database().await;
        sqlx::query("INSERT INTO plugin_defaults (key, value) VALUES ('*.hello:read', 'other/read'), ('*.hello:edit', 'builtin')")
            .execute(&db).await.unwrap();
        let manifest = json!({"contributes":{"openers":[{"id":"open","mode":"read","patterns":["*.hello"]}]}});
        install_defaults(&db, "hello", &manifest).await.unwrap();
        let d = defaults(&db).await.unwrap();
        assert_eq!(d["*.hello:read"], "hello/open");
        assert_eq!(d["*.hello:edit"], "builtin");
        // Removal's exact cleanup query must not remove other modes/plugins.
        sqlx::query("DELETE FROM plugin_defaults WHERE value LIKE $1 || '/%'").bind("hello").execute(&db).await.unwrap();
        let d = defaults(&db).await.unwrap();
        assert!(d.get("*.hello:read").is_none());
        assert_eq!(d["*.hello:edit"], "builtin");
    }

    #[test]
    fn gnu_metadata_bomb_and_trailing_gzip_are_bounded() {
        use std::io::Write;
        let t = crate::test_util::TestDir::new("metadata-bomb");
        for kind in [tar::EntryType::GNULongName, tar::EntryType::XHeader] {
            let mut header = tar::Header::new_gnu();
            header.set_path("././@LongLink").unwrap();
            header.set_entry_type(kind);
            header.set_size(MAX_UNPACKED + 1);
            header.set_cksum();
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(header.as_bytes()).unwrap();
            // Generate zeros through a fixed-size copy buffer, not a 50 MB allocation.
            std::io::copy(&mut std::io::repeat(0).take(MAX_UNPACKED + 1), &mut gz).unwrap();
            let error = unpack(&gz.finish().unwrap(), &t.join("out")).unwrap_err();
            assert!(format!("{error:#}").contains("unpacked limit"), "{error:#}");
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::copy(&mut std::io::repeat(0).take(MAX_UNPACKED + 1), &mut gz).unwrap();
        assert!(
            unpack(&gz.finish().unwrap(), &t.join("out")).is_err(),
            "tar EOF must not hide trailing decompressed bytes"
        );
        // A valid empty first gzip member must not hide a decompression bomb in a second.
        let mut first = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        first.write_all(&[0; 1024]).unwrap();
        let mut concatenated = first.finish().unwrap();
        let mut second = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::copy(&mut std::io::repeat(0).take(MAX_UNPACKED), &mut second).unwrap();
        concatenated.extend(second.finish().unwrap());
        let error = unpack(&concatenated, &t.join("out")).unwrap_err();
        assert!(error.downcast_ref::<Refused>().is_some(), "{error:#}");
        assert!(error.to_string().contains("unpacked limit"), "{error:#}");
        let mut exact = UnpackedLimit {
            inner: &b"abc"[..],
            remaining: 3,
        };
        assert_eq!(std::io::copy(&mut exact, &mut std::io::sink()).unwrap(), 3);
        let mut extra = UnpackedLimit {
            inner: &b"abcd"[..],
            remaining: 3,
        };
        assert!(std::io::copy(&mut extra, &mut std::io::sink()).is_err());
    }

    fn manifest() -> Value {
        json!({
            "name": "hello", "version": "0.1.0", "title": "Hello",
            "server": { "command": "node", "args": ["server.js"] },
            "contributes": {
                "views": [{ "id": "main", "title": "Hello", "ui": "ui://hello/app" }],
                "openers": [{ "id": "open", "title": "Open", "ui": "ui://hello/app", "patterns": ["*.hello"], "mode": "read" }],
                "games": [{ "id": "guess", "title": "Guess", "ui": "ui://hello/app", "description": "A game" }],
                "widgets": [{ "id": "today", "title": "Today", "ui": "ui://hello/app", "size": 2 }],
                "commands": [{ "name": "hello", "tool": "hello_greet", "args": "<name>", "description": "Greet" }]
            },
            "kernel": ["files:read", "ai", "files:read"]
        })
    }

    fn with(path: &str, v: Value) -> Value {
        let mut m = manifest();
        let mut at = &mut m;
        let parts: Vec<&str> = path.split('.').collect();
        for p in &parts[..parts.len() - 1] {
            at = match p.parse::<usize>() {
                Ok(i) => &mut at[i],
                Err(_) => &mut at[*p],
            };
        }
        at[*parts.last().unwrap()] = v;
        m
    }

    #[tokio::test]
    async fn installed_skills_follow_enable_permissions_and_uninstall() {
        let tmp = crate::test_util::TestDir::new("plugin-skills");
        let db = crate::storage::connect(&format!("sqlite:{}", tmp.join("db.sqlite3").display())).await.unwrap();
        let mut m = manifest();
        m["skills"] = json!("skills/");
        std::fs::create_dir_all(tmp.join("hello/skills")).unwrap();
        sqlx::query("INSERT INTO plugins(name,version,manifest,permissions,enabled,source) VALUES('hello','0.1.0',$1,$2,1,'local')")
            .bind(m.to_string()).bind(json!(["ai","files:read"]).to_string()).execute(&db).await.unwrap();
        assert_eq!(enabled_skill_roots(&db, &tmp).await.unwrap(), [tmp.join("hello/skills/")]);
        sqlx::query("UPDATE plugins SET enabled=0").execute(&db).await.unwrap();
        assert!(enabled_skill_roots(&db, &tmp).await.unwrap().is_empty());
        sqlx::query("UPDATE plugins SET enabled=1, permissions='[]'").execute(&db).await.unwrap();
        assert!(enabled_skill_roots(&db, &tmp).await.unwrap().is_empty(), "unapproved manifests stay invisible");
        sqlx::query("DELETE FROM plugins").execute(&db).await.unwrap();
        assert!(enabled_skill_roots(&db, &tmp).await.unwrap().is_empty(), "leftover disk folders are not installed");
    }

    #[test]
    fn skill_paths_reject_escapes_and_links() {
        for path in ["../skills", "/skills", "a/../../skills", "./skills", ".hidden", "node_modules/x"] {
            assert!(validate(&with("skills", json!(path))).is_err(), "{path}");
        }
        let tmp = crate::test_util::TestDir::new("skill-paths");
        std::fs::create_dir_all(tmp.join("skills/work")).unwrap();
        assert!(safe_skill_path(&tmp, &tmp.join("skills/work")));
        std::os::unix::fs::symlink(tmp.join("skills"), tmp.join("link")).unwrap();
        assert!(!safe_skill_path(&tmp, &tmp.join("link/work")));
        assert!(!safe_skill_path(&tmp, &tmp.join("skills/../skills")));
        assert!(!safe_skill_path(&tmp, &tmp.join("missing")));
    }

    #[test]
    fn a_good_manifest_validates() {
        let m = validate(&manifest()).unwrap();
        assert_eq!((m.name.as_str(), m.version.as_str(), m.title.as_str()), ("hello", "0.1.0", "Hello"));
        assert_eq!(m.permissions, vec!["ai", "files:read"], "sorted, without repeats");
        assert_eq!(m.args, vec!["server.js"]);
    }

    #[test]
    fn bad_manifests_are_refused_with_a_reason() {
        let cases = [
            ("name", json!("a"), "name"),
            ("name", json!("Hello"), "name"),
            ("name", json!("x".repeat(41)), "name"),
            ("name", json!("hel_lo"), "name"),
            ("version", json!("1.0"), "version"),
            ("version", json!("01.0.0"), "version"),
            ("server", json!({ "command": "python", "args": ["s.py"] }), "server.command"),
            ("server", json!({ "command": "node", "args": [] }), "server.args"),
            ("server", json!({ "command": "node", "args": ["../escape.js"] }), "relative path"),
            ("server", json!({ "command": "node", "args": ["-e", "evil()"] }), "relative path"),
            ("server", json!({ "command": "node", "args": ["/abs.js"] }), "relative path"),
            ("server", json!({ "command": "node", "args": [".hidden.js"] }), "relative path"),
            ("contributes.views.0.ui", json!("ui://other/app"), "ui://hello/"),
            ("contributes.views.0.ui", json!("https://evil.example/app"), "ui://hello/"),
            ("contributes.views.0.ui", json!("ui://hello/"), "ui://hello/"),
            ("contributes.cards", json!([]), "not a slot"),
            ("contributes.widgets", json!([{ "id": "w", "title": "W", "ui": "ui://hello/app", "size": 4 }]), "size"),
            ("contributes.widgets", json!([{ "id": "w", "title": "W" }]), "ui://hello/"),
            ("contributes.openers.0.mode", json!("write"), "mode"),
            ("contributes.openers.0.patterns", json!(["a/b.md"]), "patterns"),
            ("contributes.openers.0.patterns", json!([]), "patterns"),
            ("contributes.commands.0.tool", json!("x y"), "tool"),
            ("contributes.commands.0.choices", json!("a"), "choices"),
            ("contributes.commands.0.choices", json!([]), "1 to 20"),
            ("contributes.commands.0.choices", json!([{ "value": "Go", "description": "d" }]), "value"),
            ("contributes.commands.0.choices", json!([{ "value": "a b", "description": "d" }]), "value"),
            ("contributes.commands.0.choices", json!([{ "value": "x".repeat(33), "description": "d" }]), "value"),
            ("contributes.commands.0.choices", json!([{ "value": "a" }]), "description"),
            ("contributes.commands.0.choices", json!([{ "value": "a", "description": "d".repeat(121) }]), "description"),
            ("contributes.commands.0.choices", json!([{ "value": "a", "description": "d", "then": "t".repeat(61) }]), "then"),
            ("contributes.commands.0.choices", json!([{ "value": "a", "description": "d", "extra": 1 }]), "unknown key"),
            ("contributes.commands.0.choices", json!(["a"]), "object"),
            ("contributes.commands.0.choices", json!([{ "value": "a", "description": "d" }, { "value": "a", "description": "e" }]), "twice"),
            ("contributes.views.0.id", json!("Main"), "id"),
            ("kernel", json!(["files:read", "shell"]), "permission `shell`"),
            ("kernel", json!(["files:write:../x"]), "permission"),
            ("kernel", json!(["files:write:/etc"]), "permission"),
            ("kernel", json!(["files:write:.git"]), "permission"),
            ("kernel", json!(["files:write:a", "files:write:b"]), "at most one"),
            ("kernel", json!("files:read"), "list"),
        ];
        for (path, value, want) in cases {
            let e = validate(&with(path, value.clone())).unwrap_err();
            assert!(e.contains(want), "{path}={value}: {e}");
        }
        let mut twice = manifest();
        twice["contributes"]["views"] = json!([{ "id": "a", "title": "A", "ui": "ui://hello/a" }, { "id": "a", "title": "B", "ui": "ui://hello/b" }]);
        assert!(validate(&twice).unwrap_err().contains("twice"));
        assert!(validate(&with("kernel", json!(["files:write:Reading/notes", "network"]))).is_ok());
    }

    #[test]
    fn command_choices() {
        let ok = json!([{ "value": "list", "description": "All of them" }, { "value": "open", "description": "One note", "then": "<note>" }]);
        let m = validate(&with("contributes.commands.0.choices", ok.clone())).unwrap();
        assert_eq!(m.contributes["commands"][0]["choices"], ok);
        let many: Vec<Value> = (0..21).map(|i| json!({ "value": format!("c{i}"), "description": "d" })).collect();
        assert!(validate(&with("contributes.commands.0.choices", json!(many[..20]))).is_ok());
        assert!(validate(&with("contributes.commands.0.choices", json!(many))).unwrap_err().contains("1 to 20"));
        // The value fills a positional argument: without a placeholder there is none.
        let mut bare = with("contributes.commands.0.choices", ok.clone());
        bare["contributes"]["commands"][0]["args"] = json!("name");
        assert!(validate(&bare).unwrap_err().contains("placeholder"));
        bare["contributes"]["commands"][0]["args"] = json!("[action] [rest]");
        assert!(validate(&bare).is_ok());
        bare["contributes"]["commands"][0].as_object_mut().unwrap().remove("args");
        assert!(validate(&bare).unwrap_err().contains("placeholder"));
        assert!(validate(&manifest()).is_ok(), "choices are optional");
    }

    #[test]
    fn versions() {
        for good in ["0.1.0", "1.2.3", "10.0.0-beta.1", "1.0.0+build.5", "1.0.0-rc-1+x"] {
            assert!(valid_version(good), "{good}");
        }
        for bad in ["", "1", "1.2", "1.2.3.4", "v1.2.3", "1.02.3", "1.2.3-", "1.2.3-a..b", "1.2.x"] {
            assert!(!valid_version(bad), "{bad}");
        }
    }

    #[test]
    fn marketplace_update_precedence() {
        for (candidate, installed, newer) in [
            ("0.3.0", "0.7.0", false),
            ("0.7.0", "0.3.0", true),
            ("0.7.0", "0.7.0", false),
            ("0.7.0+new", "0.7.0+old", false),
            ("0.7.0-rc.2", "0.7.0", false),
            ("0.7.0", "0.7.0-rc.2", true),
            ("0.7.0-rc.10", "0.7.0-rc.2", true),
            ("0.10.0", "0.9.0", true),
            ("0.7.0-01", "0.3.0", false),
            ("0.7.0", "unknown", false),
        ] {
            assert_eq!(newer_version(candidate, installed), newer, "{candidate} vs {installed}");
        }
    }

    #[test]
    fn permissions_become_a_run() {
        let p = |l: &[&str]| run_for("x", &l.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(p(&[]).files, Files::None);
        assert!(!p(&[]).network && !p(&[]).ai);
        assert_eq!(p(&["files:read"]).files, Files::Read);
        assert_eq!(p(&["files:read", "files:write:Reading"]).files, Files::WriteSub("Reading".into()));
        assert_eq!(p(&["files:write", "files:write:Reading"]).files, Files::Write);
        assert!(p(&["network", "ai"]).network && p(&["network", "ai"]).ai);
    }

    #[test]
    fn sampling_model_follows_known_hints() {
        let known = ["claude/claude-opus-5-5", "claude/claude-sonnet-5-5", "codex/gpt-6.1-sol"].map(String::from);
        let hints = |names: &[&str]| json!({ "modelPreferences": { "hints": names.iter().map(|n| json!({ "name": n })).collect::<Vec<_>>() } });
        let assist = crate::config::get().assist_model.clone();
        assert_eq!(sampling_model(&json!({}), &known), assist);
        assert_eq!(sampling_model(&hints(&["unknown-model"]), &known), assist);
        assert_eq!(sampling_model(&hints(&["codex/gpt-6.1-sol"]), &known), "codex/gpt-6.1-sol");
        assert_eq!(sampling_model(&hints(&["GPT-6.1-sol"]), &known), "codex/gpt-6.1-sol");
        assert_eq!(sampling_model(&hints(&["nope", "claude-3-opus"]), &known), "claude/claude-opus-5-5");
        assert_eq!(sampling_model(&hints(&["sonnet"]), &known), crate::config::get().summary_model);
    }

    #[test]
    fn app_visibility() {
        assert!(app_visible(&Value::Null));
        assert!(app_visible(&json!({ "ui": { "resourceUri": "ui://x/a" } })));
        assert!(app_visible(&json!({ "ui": { "visibility": ["model", "app"] } })));
        assert!(!app_visible(&json!({ "ui": { "visibility": ["model"] } })));
    }

    // ---------- archives ----------

    /// A raw tar entry: its name, type and content, written byte by byte (so the tests can make
    /// the entries a careful writer refuses to: `..`, absolute paths).
    fn raw_entry(out: &mut Vec<u8>, name: &str, kind: tar::EntryType, data: &[u8], link: &str) {
        let mut h = tar::Header::new_gnu();
        {
            let old = h.as_old_mut();
            old.name[..name.len()].copy_from_slice(name.as_bytes());
            old.linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        h.set_entry_type(kind);
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }

    fn tgz(entries: &[(&str, tar::EntryType, &[u8], &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut t = Vec::new();
        for (n, k, d, l) in entries {
            raw_entry(&mut t, n, *k, d, l);
        }
        t.extend_from_slice(&[0; 1024]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&t).unwrap();
        gz.finish().unwrap()
    }

    const MANIFEST: &[u8] = br#"{"name":"hello","version":"0.1.0","server":{"command":"node","args":["server.js"]}}"#;

    fn unpacked(entries: &[(&str, tar::EntryType, &[u8], &str)]) -> Result<crate::test_util::TestDir> {
        let dir = crate::test_util::TestDir::new("plugin-unpack");
        unpack(&tgz(entries), &dir)?;
        Ok(dir)
    }

    fn refused(r: Result<crate::test_util::TestDir>, want: &str) {
        let e = r.err().expect("refused");
        assert!(e.downcast_ref::<Refused>().is_some(), "a refusal, not a failure: {e:#}");
        assert!(e.to_string().contains(want), "{e}");
    }

    use tar::EntryType as T;

    #[test]
    fn a_good_archive_unpacks() {
        let dir = unpacked(&[("./", T::Directory, b"", ""), ("plugin.json", T::Regular, MANIFEST, ""), ("mcp/dist/server.js", T::Regular, b"//", "")]).unwrap();
        assert!(dir.join("mcp/dist/server.js").is_file());
        assert_eq!(staged_manifest(&dir).unwrap_err().to_string(), "plugin.json: the server's script server.js isn't in the plugin");
        let ok = unpacked(&[("plugin.json", T::Regular, MANIFEST, ""), ("server.js", T::Regular, b"//", "")]).unwrap();
        assert_eq!(staged_manifest(&ok).unwrap().name, "hello");
    }

    #[test]
    fn archive_rules() {
        refused(unpacked(&[("plugin.json", T::Regular, MANIFEST, ""), ("link", T::Symlink, b"", "/etc/passwd")]), "links");
        refused(unpacked(&[("plugin.json", T::Regular, MANIFEST, ""), ("hard", T::Link, b"", "plugin.json")]), "links");
        refused(unpacked(&[("../escape.js", T::Regular, b"x", "")]), "`..`");
        refused(unpacked(&[("a/../../escape.js", T::Regular, b"x", "")]), "`..`");
        refused(unpacked(&[("/etc/cron.d/x", T::Regular, b"x", "")]), "absolute");
        refused(unpacked(&[(".env", T::Regular, b"x", "")]), "dot files");
        refused(unpacked(&[("ui/.hidden/x", T::Regular, b"x", "")]), "dot files");
        refused(unpacked(&[("node_modules/x/index.js", T::Regular, b"x", "")]), "node_modules");
        refused(unpacked(&[("dev", T::Char, b"", "")]), "only files and folders");
        refused(unpacked(&[("fifo", T::Fifo, b"", "")]), "only files and folders");
        assert!(unpacked(&[("twice", T::Regular, b"x", ""), ("twice", T::Regular, b"y", "")]).is_err(), "an entry twice is never overwritten");
    }

    #[test]
    fn oversize_archives_are_refused() {
        // Unpacked: 51 MB of zeros compress to almost nothing.
        let big = vec![0u8; (MAX_UNPACKED + 1024 * 1024) as usize];
        refused(unpacked(&[("big.bin", T::Regular, &big, "")]), "unpacks to over 50 MB");
        // Downloaded or read: over 50 MB as it is.
        let dir = crate::test_util::TestDir::new("plugin-big");
        let e = unpack(&vec![0u8; (MAX_ARCHIVE + 1) as usize], &dir).unwrap_err();
        assert!(e.to_string().contains("the archive is over 50 MB"), "{e}");
    }

    #[test]
    fn the_sha256_must_match() {
        let a = tgz(&[("plugin.json", T::Regular, MANIFEST, "")]);
        assert!(check_sha256(&a, &sha256_hex(&a)).is_ok());
        assert!(check_sha256(&a, &sha256_hex(&a).to_uppercase()).is_ok());
        let e = check_sha256(&a, &"0".repeat(64)).unwrap_err();
        assert!(e.downcast_ref::<Refused>().is_some() && e.to_string().contains("not the"), "{e}");
    }

    #[test]
    fn a_folder_copies_without_dot_files_and_refuses_links() {
        let src = crate::test_util::TestDir::new("plugin-src");
        std::fs::write(src.join("plugin.json"), MANIFEST).unwrap();
        std::fs::write(src.join("server.js"), "//").unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join(".git/config"), "x").unwrap();
        std::fs::create_dir_all(src.join("node_modules/x")).unwrap();
        let into = crate::test_util::TestDir::new("plugin-into");
        copy_folder(&src, &into).unwrap();
        assert!(into.join("server.js").is_file() && !into.join(".git").exists() && !into.join("node_modules").exists());
        std::os::unix::fs::symlink("/etc/passwd", src.join("passwd")).unwrap();
        let into = crate::test_util::TestDir::new("plugin-into2");
        let e = copy_folder(&src, &into).unwrap_err();
        assert!(e.to_string().contains("links aren't allowed"), "{e}");
    }
}
