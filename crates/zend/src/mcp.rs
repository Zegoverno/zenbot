//! MCP client: tools from the owner's MCP servers, found and loaded on demand (DESIGN.md, "Skills
//! and tools that improve themselves"; Phase 2).
//!
//! Servers are configured in `~/.zenbot/mcp.json`, the `mcpServers` shape Claude Code and FastMCP
//! use: `command`/`args`/`env` for a local server (stdio), or `url`/`headers` for a remote one
//! (streamable HTTP). `${VAR}` in any string is filled from the kernel's environment, so secrets
//! stay out of the file and out of the model's sight; a secret-named variable (`secrets::is_secret_var`:
//! the kernel's own token, API keys, the database URL) is filled only for a server that lists it in
//! `allow_env`, so a server can't be handed the kernel's credentials by accident. Per server:
//! `enabled`, `timeout_s`, `include` / `exclude` (tool names), `allow_env`, and `untrusted` (wrap
//! its output as untrusted and raise the session's risk to 4; default true for local or remote,
//! until the owner sets `"untrusted": false`; D-052). The agent can't change the file with `edit` or
//! `write` (agent.rs, `Protected::OwnerOnly`): only the owner does.
//!
//! The model's tool list never changes mid-session (that would break the prompt cache on every
//! engine; the Phase 0 spike), so MCP tools aren't added to it. Instead the model gets three fixed
//! tools: `find_tools` (names and one-line descriptions), `load_tool` (a tool's full definition, as
//! a tool result) and `call_tool` (run it). Every call goes through the kernel: recorded like any
//! tool call, with a timeout, output capped and secrets masked. Made-tool artifacts are no longer
//! published in the catalog, and stale made_ names are refused (D-070).
//!
//! Calls run at the same time, to one server and to several: a server's lock is held only to
//! connect, and its tools are looked up from its last listing (listed again when a stdio server
//! says they changed). A stdio server gets each request written under a stdin lock and a reader
//! task hands each answer to its call by id; it runs in its own process group, killed whole when
//! it's dropped. Stdio runs contained, with a private writable cache folder and network through
//! a proxy governed by the server's own trust level; status only reads cached counts. A message from a server (a stdio line, an HTTP body) over 8 MB fails the call.
//!
//! The client asks for MCP 2025-06-18 and accepts a server that answers 2025-03-26 or 2024-11-05;
//! it refuses other revisions. Resources and prompts
//! appear as reserved `zen_` operations through the same three wrappers; server tools may not use
//! that prefix (such a tool is left out). Structured JSON and validated images survive tool results. HTTP SSE responses are
//! processed as complete events arrive. Named calls connect only their server; discovery has a
//! three-second per-server budget and reports incomplete catalogs instead of hiding delays.
//! Form elicitation is routed to owner-only pending-request endpoints, with HTTP request context
//! or one unambiguous active stdio call; ambiguous stdio requests are refused. OAuth refresh uses
//! owner-provisioned credentials (auth.rs), not an automatic browser login. Sampling, roots,
//! subscriptions, legacy HTTP+SSE and the newer 2026 protocol lifecycle remain unsupported.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, Mutex};

type Elicitation = (Value, oneshot::Sender<Value>);
use uuid::Uuid;

use crate::{tools, App};

mod auth;
mod login;

pub(crate) use login::{finish as login_finish, start as login_start};

const PROTOCOL: &str = "2025-06-18";

#[derive(Clone, Debug, PartialEq)]
pub struct ServerConfig {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub url: Option<String>,
    pub headers: Vec<(String, String)>,
    pub oauth: Option<auth::OAuthConfig>,
    pub timeout: Duration,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub untrusted: bool,
}

#[derive(Clone, Debug)]
pub struct ToolEntry {
    pub server: String,
    pub name: String,
    pub description: String,
    pub schema: Value,
    /// From a server marked untrusted: its description and schema are wrapped and taint the session.
    pub untrusted: bool,
    /// A made tool's manifest, the exact text its name, description and schema were read from:
    /// what its catalog entry is rated by (D-062). None for a server's tool.
    pub manifest: Option<String>,
    /// The server's tool annotations (MCP `readOnlyHint`, `destructiveHint`, `openWorldHint`): facts
    /// for the action gate's effect (D-067). Null for a made tool.
    pub annotations: Value,
}

impl ToolEntry {
    /// The name the model uses: `<server>_<tool>` (namespaced, as FastMCP mounts servers).
    pub fn full(&self) -> String {
        format!("{}_{}", self.server, self.name)
    }
}

/// The config file: ZEN_MCP_CONFIG, else `~/.zenbot/mcp.json`.
pub fn config_path() -> PathBuf {
    std::env::var("ZEN_MCP_CONFIG").map(PathBuf::from).unwrap_or_else(|_| crate::zen_home().join("mcp.json"))
}

/// `${VAR}` filled from the environment. An unset variable becomes empty, and is reported in
/// `missing`; so is a secret-named one that `allow` doesn't list (never filled).
pub fn expand(s: &str, allow: &[String], missing: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find('}') {
            Some(j) => {
                let var = &after[..j];
                if crate::secrets::is_secret_var(var) && !allow.iter().any(|a| a == var) {
                    missing.push(format!("{var} (a secret: list it in `allow_env` to pass it to this server)"));
                } else {
                    match std::env::var(var) {
                        Ok(v) => out.push_str(&v),
                        Err(_) => missing.push(var.to_string()),
                    }
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse the config. Disabled servers are left out; problems are returned alongside.
pub fn parse_config(text: &str) -> (Vec<ServerConfig>, Vec<String>) {
    let mut problems = Vec::new();
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (Vec::new(), vec![format!("mcp.json is not valid JSON: {e}")]),
    };
    let Some(servers) = v["mcpServers"].as_object() else { return (Vec::new(), vec!["mcp.json has no `mcpServers` object".into()]) };
    let mut out = Vec::new();
    for (name, s) in servers {
        if s["enabled"] == false || s["disabled"] == true || crate::trust::mcp_level(name) == Some(5) {
            continue;
        }
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            problems.push(format!("server name `{name}`: letters, digits, - and _ only"));
            continue;
        }
        let mut missing = Vec::new();
        let allow: Vec<String> = s["allow_env"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
        let strs = |k: &str, missing: &mut Vec<String>| -> Vec<String> { s[k].as_array().into_iter().flatten().filter_map(Value::as_str).map(|x| expand(x, &allow, missing)).collect() };
        let map = |k: &str, missing: &mut Vec<String>| -> Vec<(String, String)> {
            s[k].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), expand(v.as_str().unwrap_or(""), &allow, missing))).collect()
        };
        let command = s["command"].as_str().map(|c| expand(c, &allow, &mut missing));
        let url = s["url"].as_str().map(|u| expand(u, &allow, &mut missing));
        if command.is_none() == url.is_none() {
            problems.push(format!("server `{name}`: give either `command` (local) or `url` (remote)"));
            continue;
        }
        let oauth = if s["oauth"].is_object() {
            match auth::OAuthConfig::parse(&s["oauth"], &allow, &mut missing) {
                Ok(value) => Some(value),
                Err(e) => { problems.push(format!("server `{name}`: {e}")); continue; }
            }
        } else { None };
        if oauth.is_some() && s["headers"].as_object().is_some_and(|h| h.keys().any(|k| k.eq_ignore_ascii_case("authorization"))) {
            problems.push(format!("server `{name}`: choose oauth or a static Authorization header"));
            continue;
        }
        let cfg = ServerConfig {
            name: name.clone(),
            oauth,
            args: strs("args", &mut missing),
            env: map("env", &mut missing),
            headers: map("headers", &mut missing),
            timeout: Duration::from_secs(s["timeout_s"].as_u64().unwrap_or(60).clamp(1, 600)),
            include: s["include"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect(),
            exclude: s["exclude"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect(),
            // The owner's trust list (D-067) decides first; `"untrusted": false` in mcp.json still counts.
            untrusted: match crate::trust::mcp_level(name) {
                Some(level) => level > 1,
                None => s["untrusted"].as_bool().unwrap_or(true),
            },
            command,
            url,
        };
        if !missing.is_empty() {
            problems.push(format!("server `{name}`: environment variables left empty: {}", missing.join(", ")));
        }
        out.push(cfg);
    }
    (out, problems)
}

// ---------- connections ----------

/// The largest message a server may send (a stdio line, an HTTP body): past it the call fails
/// instead of the kernel holding whatever the server streams.
const MAX_MESSAGE: usize = 8 * 1024 * 1024;

/// A local server's process and the calls waiting for its answers. Calls run at the same time: each
/// writes its request under the stdin lock only, and a reader task hands each response to the call
/// with its id (as mind.rs does for workers).
struct StdioConn {
    stdin: Mutex<ChildStdin>,
    pending: std::sync::Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    elicitation: std::sync::Mutex<HashMap<i64, mpsc::UnboundedSender<Elicitation>>>,
    /// Set by the reader when the server exited or broke the protocol: connect again.
    closed: AtomicBool,
    /// Set when the server said its tool list changed (`notifications/tools/list_changed`).
    stale: AtomicBool,
    /// The server runs in its own process group (an `npx` or `sh` starts children): dropping the
    /// connection kills all of it, not just the first process.
    _group: tools::GroupKill,
    _child: Child,
}

impl StdioConn {
    fn fail_pending(&self, why: &str) {
        self.closed.store(true, Ordering::SeqCst);
        let waiters: Vec<_> = self.pending.lock().unwrap_or_else(|e| e.into_inner()).drain().collect();
        for (_, w) in waiters {
            let _ = w.send(Err(why.to_string()));
        }
    }

    async fn write(&self, msg: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(format!("{msg}\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

/// Hand a local server's messages to the calls waiting for them, until it exits.
async fn read_stdio(conn: std::sync::Weak<StdioConn>, mut stdout: BufReader<ChildStdout>) {
    let why = loop {
        let line = match read_line_capped(&mut stdout, MAX_MESSAGE).await {
            Ok(Some(line)) => line,
            Ok(None) => break "the server exited".to_string(),
            Err(e) => break format!("{e:#}"),
        };
        let Some(conn) = conn.upgrade() else { return };
        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
        match (v.get("method").and_then(Value::as_str), v.get("id")) {
            (None, Some(id)) => {
                if let Some(w) = id.as_i64().and_then(|id| conn.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id)) {
                    let _ = w.send(Ok(v));
                }
            }
            (Some("elicitation/create"), Some(id)) => {
                // Legacy stdio has no mandatory parent request ID. Never guess between sessions.
                let context = {
                    let pending = conn.pending.lock().unwrap_or_else(|e| e.into_inner());
                    if pending.len() == 1 {
                        pending.keys().next().and_then(|id| conn.elicitation.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned())
                    } else { None }
                };
                if let Some(context) = context {
                    let (tx, rx) = oneshot::channel();
                    if context.send((v.clone(), tx)).is_ok() {
                        let io = conn.clone();
                        let id = id.clone();
                        tokio::spawn(async move {
                            if let Ok(result) = rx.await { let _ = io.write(&json!({"jsonrpc":"2.0","id":id,"result":result})).await; }
                        });
                        continue;
                    }
                }
                let _ = conn.write(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"elicitation requires one unambiguous active owner tool call"}})).await;
            }
            // Sampling and filesystem roots remain unsupported.
            (Some(_), Some(id)) => {
                let reply = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "not supported by zenbot" } });
                let _ = conn.write(&reply).await;
            }
            (Some("notifications/tools/list_changed"), None) => conn.stale.store(true, Ordering::SeqCst),
            _ => {}
        }
    };
    if let Some(conn) = conn.upgrade() {
        conn.fail_pending(&why);
    }
}

/// One line (without its newline), None at the end of the stream; an error past `max` bytes.
async fn read_line_capped<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R, max: usize) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let (take, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        if line.len() + take > max + 1 {
            bail!("the server sent a message over {} MB", max / 1024 / 1024);
        }
        line.extend_from_slice(&buf[..take]);
        r.consume(take);
        if done {
            line.pop();
            return Ok(Some(line));
        }
    }
}

enum Conn {
    Stdio(Arc<StdioConn>),
    /// `closed` after a failed call: the next one starts a new MCP session.
    Http { session: std::sync::Mutex<Option<String>>, closed: AtomicBool },
}

impl Conn {
    fn usable(&self) -> bool {
        match self {
            Conn::Stdio(io) => io.usable(),
            Conn::Http { closed, .. } => !closed.load(Ordering::SeqCst),
        }
    }

    /// Don't use this connection again: connect afresh next time.
    fn close(&self, why: &str) {
        match self {
            Conn::Stdio(io) => io.fail_pending(why),
            Conn::Http { closed, .. } => closed.store(true, Ordering::SeqCst),
        }
    }

    fn stale(&self) -> bool {
        matches!(self, Conn::Stdio(io) if io.stale.load(Ordering::SeqCst))
    }
}

/// A configured server. Its lock is held only to connect (or list its tools again), never across a
/// call, so one slow call holds up neither the server's other calls nor any other server.
struct Server {
    cfg: ServerConfig,
    next_id: AtomicI64,
    state: Mutex<State>,
    auth: Mutex<Option<auth::Token>>,
}

#[derive(Default)]
struct State {
    conn: Option<Arc<Conn>>,
    /// The server's tools as last listed: what find_tools, load_tool and call_tool look up.
    tools: Vec<ToolEntry>,
    error: Option<String>,
    /// When connecting last failed: a server that is down isn't retried for `RETRY_AFTER`, so it
    /// doesn't cost its connect timeout on every find_tools / call_tool.
    failed_at: Option<std::time::Instant>,
}

const RETRY_AFTER: Duration = Duration::from_secs(60);

/// The servers, keyed by name, with the config they were started from and when it was read.
struct Registry {
    servers: HashMap<String, Arc<Server>>,
    loaded: Option<SystemTime>,
    problems: Vec<String>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry { servers: HashMap::new(), loaded: None, problems: Vec::new() }));

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(10)).build().expect("http client"));

/// Read the config again when it changed since it was loaded; servers whose config changed are
/// restarted, removed ones dropped.
async fn refresh() {
    let path = config_path();
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut reg = REGISTRY.lock().await;
    if reg.loaded.is_some() && reg.loaded == mtime {
        return;
    }
    let (cfgs, problems) = match std::fs::read_to_string(&path) {
        Ok(text) => parse_config(&text),
        Err(_) => (Vec::new(), Vec::new()),
    };
    // A config change replaces the handles without waiting for active calls. Those calls finish on
    // their old Arc; subsequent calls use the new server. Never hold the registry lock over I/O.
    reg.servers = cfgs.into_iter().map(|cfg| (cfg.name.clone(), Arc::new(Server { cfg, next_id: AtomicI64::new(1), state: Mutex::default(), auth: Mutex::default() }))).collect();
    reg.loaded = mtime;
    reg.problems = problems;
}

impl Server {
    /// The live connection and the tools, connecting (or listing the tools again) when needed.
    async fn ensure(&self, db: &sqlx::PgPool) -> Result<(Arc<Conn>, Vec<ToolEntry>)> {
        let mut st = self.state.lock().await;
        if let Some(conn) = st.conn.clone().filter(|c| c.usable()) {
            if !conn.stale() {
                return Ok((conn, st.tools.clone()));
            }
            if let Conn::Stdio(io) = &*conn {
                io.stale.store(false, Ordering::SeqCst);
            }
            match self.list_tools(&conn).await {
                Ok(mut tools) => {
                    tools.extend(st.tools.iter().filter(|t| service_method(&t.name).is_some()).cloned());
                    st.tools = tools;
                },
                Err(e) => tracing::warn!("listing the tools of MCP server `{}` again: {e:#}", self.cfg.name),
            }
            return Ok((conn, st.tools.clone()));
        }
        st.conn = None;
        if let (Some(at), Some(e)) = (st.failed_at, &st.error) {
            if at.elapsed() < RETRY_AFTER {
                bail!("{e} (tried {}s ago; retrying after {}s)", at.elapsed().as_secs(), RETRY_AFTER.as_secs());
            }
        }
        match self.connect(db).await {
            Ok((conn, tools)) => {
                *st = State { conn: Some(conn.clone()), tools: tools.clone(), error: None, failed_at: None };
                Ok((conn, tools))
            }
            Err(e) => {
                st.error = Some(format!("{e:#}"));
                st.failed_at = Some(std::time::Instant::now());
                Err(e)
            }
        }
    }

    async fn connect(&self, db: &sqlx::PgPool) -> Result<(Arc<Conn>, Vec<ToolEntry>)> {
        let conn = match (&self.cfg.command, &self.cfg.url) {
            (Some(cmd), _) => {
                // A clean environment: the kernel's own (its token, API keys) never reaches a
                // server; only the basics and what the config gives it.
                let keep = ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TZ", "TMPDIR"];
                let dir = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home".into())).join(".cache/zenbot/mcp").join(&self.cfg.name);
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
                let socket = crate::proxy::ensure_mcp(db, &self.cfg.name, if self.cfg.untrusted {4} else {1}).await?;
                let filter = tools::contained_filter(true).map_err(|e| anyhow!(e))?;
                let tmp = tools::session_tmp(&format!("mcp-{}", self.cfg.name));
                let mut command = tools::contained_command(&filter, &dir, true, tmp.as_deref(), Some(&socket), &[]);
                command.env_clear();
                command.env("XDG_CACHE_HOME", dir.join("cache"))
                    .env("npm_config_cache", dir.join("cache/npm"))
                    .env("PIP_CACHE_DIR", dir.join("cache/pip"))
                    .env("UV_CACHE_DIR", dir.join("cache/uv"));
                // Restore proxy variables after clearing inherited credentials.
                let proxy = format!("http://127.0.0.1:{}", crate::proxy::BRIDGE_PORT);
                for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "ALL_PROXY"] {
                    command.env(key, &proxy);
                }
                let mut child = command.arg(cmd)
                    .args(&self.cfg.args)
                    .envs(keep.iter().filter_map(|k| std::env::var(k).ok().map(|v| (*k, v))))
                    .envs(self.cfg.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                    .env("TMPDIR", "/tmp")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .kill_on_drop(true)
                    .spawn()
                    .with_context(|| format!("starting `{cmd}`"))?;
                let group = tools::GroupKill(child.id().map(|p| p as i32));
                let stdin = child.stdin.take().context("no stdin")?;
                let stdout = BufReader::new(child.stdout.take().context("no stdout")?);
                let io = Arc::new(StdioConn {
                    stdin: Mutex::new(stdin),
                    pending: Default::default(),
                    elicitation: Default::default(),
                    closed: AtomicBool::new(false),
                    stale: AtomicBool::new(false),
                    _group: group,
                    _child: child,
                });
                tokio::spawn(read_stdio(Arc::downgrade(&io), stdout));
                Arc::new(Conn::Stdio(io))
            }
            _ => Arc::new(Conn::Http { session: std::sync::Mutex::new(None), closed: AtomicBool::new(false) }),
        };
        let init = self
            .request(&conn, "initialize", json!({ "protocolVersion": PROTOCOL, "capabilities": {"elicitation":{}}, "clientInfo": { "name": "zenbot", "version": env!("CARGO_PKG_VERSION") } }))
            .await?;
        validate_protocol(&init)?;
        self.notify(&conn, "notifications/initialized", json!({})).await?;
        let mut tools = if init["capabilities"].get("tools").is_some() { self.list_tools(&conn).await? } else { Vec::new() };
        tools.extend(service_entries(&self.cfg, &init["capabilities"]));
        Ok((conn, tools))
    }

    async fn list_tools(&self, conn: &Arc<Conn>) -> Result<Vec<ToolEntry>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = self.request(conn, "tools/list", params).await?;
            for t in page["tools"].as_array().into_iter().flatten() {
                let name = t["name"].as_str().unwrap_or("").to_string();
                if name.starts_with("zen_") {
                    // Reserved for the resource and prompt operations: skip it, keep the rest.
                    tracing::warn!("MCP server `{}`: tool `{name}` uses the reserved zen_ prefix; left out", self.cfg.name);
                    continue;
                }
                anyhow::ensure!(tools.len() < 10000, "tools/list exceeds 10000 entries; catalog withheld");
                if name.is_empty() || (!self.cfg.include.is_empty() && !self.cfg.include.contains(&name)) || self.cfg.exclude.contains(&name) {
                    continue;
                }
                tools.push(ToolEntry {
                    server: self.cfg.name.clone(),
                    name,
                    description: t["description"].as_str().unwrap_or("").to_string(),
                    schema: t["inputSchema"].clone(),
                    untrusted: self.cfg.untrusted,
                    manifest: None,
                    annotations: t["annotations"].clone(),
                });
            }
            cursor = page["nextCursor"].as_str().map(String::from);
            if let Some(c) = &cursor {
                anyhow::ensure!(seen.insert(c.clone()), "tools/list repeated a pagination cursor");
                anyhow::ensure!(seen.len() < 1000, "tools/list exceeded 1000 pages; catalog withheld rather than truncated");
            } else {
                break;
            }
        }
        Ok(tools)
    }

    async fn notify(&self, conn: &Conn, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        match conn {
            Conn::Stdio(io) => io.write(&msg).await,
            Conn::Http { session, .. } => {
                let current = session.lock().unwrap_or_else(|e| e.into_inner()).clone();
                self.post(&msg, current.as_deref()).await.map(|_| ())
            }
        }
    }

    /// A request on `conn`: its result, or its JSON-RPC error. A broken connection (an exited
    /// server, a failed HTTP call, a message too large) is an error too, and isn't used again.
    async fn request(&self, conn: &Arc<Conn>, method: &str, params: Value) -> Result<Value> {
        self.request_inner(conn, method, params, None).await
    }

    async fn request_inner(&self, conn: &Arc<Conn>, method: &str, params: Value, context: Option<(&App, Uuid)>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let timeout = if context.is_some() { self.cfg.timeout.max(Duration::from_secs(600)) } else { self.cfg.timeout };
        let res = tokio::time::timeout(timeout, async {
            match &**conn {
                Conn::Stdio(io) => {
                    let (tx, rx) = oneshot::channel();
                    io.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
                    // Unanswered (a timeout, a dropped call): forget the waiter.
                    let _forget = Forget(io.clone(), id);
                    anyhow::ensure!(io.usable(), "the server exited");
                    let (elicits, mut requests) = mpsc::unbounded_channel();
                    if context.is_some() { io.elicitation.lock().unwrap_or_else(|e| e.into_inner()).insert(id, elicits); }
                    io.write(&msg).await?;
                    let mut rx = rx;
                    // As for HTTP: the server's timeout restarts once the owner answered a form.
                    let mut deadline = tokio::time::Instant::now() + self.cfg.timeout;
                    loop {
                        tokio::select! {
                            result = &mut rx => break result.map_err(|_| anyhow!("the server exited"))?.map_err(|e| anyhow!(e)),
                            Some((request, reply)) = requests.recv(), if context.is_some() => {
                                let (app, session) = context.unwrap();
                                let result = crate::mcp_interaction::elicit(app, session, &self.cfg.name, &request["id"], &request["params"]).await.unwrap_or_else(|_| json!({"action":"cancel"}));
                                let _ = reply.send(result);
                                deadline = tokio::time::Instant::now() + self.cfg.timeout;
                            }
                            _ = tokio::time::sleep_until(deadline) => anyhow::bail!("timed out after {}s", self.cfg.timeout.as_secs()),
                        }
                    }
                }
                Conn::Http { session, .. } => {
                    let current = session.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let (body, new_session) = self.post_inner(&msg, current.as_deref(), context).await?;
                    if let Some(s) = new_session {
                        *session.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
                    }
                    body.into_iter().find(|v| v["id"] == json!(id)).context("the server's answer had no response for the request")
                }
            }
        })
        .await;
        match res {
            // The server answered (perhaps with a JSON-RPC error): the connection is fine.
            Ok(Ok(v)) => answer(v),
            // Connect again next time instead of failing every later call until the config changes.
            Ok(Err(e)) => {
                conn.close(&format!("{e:#}"));
                Err(e)
            }
            // A late answer is dropped. A stdio server with other calls running keeps going; one
            // with none is started again next time (it may be stuck).
            Err(_) => {
                if let Conn::Stdio(io) = &**conn {
                    if io.pending.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
                        conn.close("it stopped answering");
                    }
                }
                bail!("no answer within {}s", timeout.as_secs())
            }
        }
    }

    /// POST a message (streamable HTTP): the answer is JSON or an event stream of JSON messages,
    /// read up to MAX_MESSAGE.
    async fn post(&self, msg: &Value, session: Option<&str>) -> Result<(Vec<Value>, Option<String>)> {
        self.post_inner(msg, session, None).await
    }

    async fn post_inner(&self, msg: &Value, session: Option<&str>, context: Option<(&App, Uuid)>) -> Result<(Vec<Value>, Option<String>)> {
        let url = self.cfg.url.as_deref().context("no url")?;
        // The server's timeout counts only time spent waiting for it: an owner's answer to a form
        // restarts it (below). The whole call stays under `request_inner`'s cap.
        let mut deadline = tokio::time::Instant::now() + self.cfg.timeout;
        let build = |token: Option<String>| {
            let mut req = HTTP
                .post(url)
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", PROTOCOL)
                .json(msg);
            for (k, v) in &self.cfg.headers {
                req = req.header(k.as_str(), v.as_str());
            }
            if let Some(token) = token {
                req = req.bearer_auth(token);
            }
            if let Some(s) = session {
                req = req.header("Mcp-Session-Id", s);
            }
            req
        };
        let token = auth::bearer(self.cfg.oauth.as_ref(), &self.cfg.name, url, &self.auth).await?;
        let send = |req: reqwest::RequestBuilder| tokio::time::timeout_at(deadline, req.send());
        let mut resp = send(build(token)).await.context("the server didn't answer in time")?.context("contacting the server")?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            // Refresh once and retry with a fresh request (appending would send both tokens).
            auth::expire(&self.auth).await;
            if let Some(token) = auth::bearer(self.cfg.oauth.as_ref(), &self.cfg.name, url, &self.auth).await? {
                resp = send(build(Some(token))).await.context("the server didn't answer in time")?.context("contacting the server after OAuth refresh")?;
            }
            anyhow::ensure!(resp.status() != reqwest::StatusCode::UNAUTHORIZED, "the server needs sign-in: {}", auth::login_hint(&self.cfg.name));
        }
        let status = resp.status();
        let new_session = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
        let kind = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let mut body: Vec<u8> = Vec::new();
        let streaming = status.is_success() && kind.contains("text/event-stream");
        let wanted = msg.get("id");
        // Notifications may return 202 without a body.
        if status.is_success() && wanted.is_none() { return Ok((Vec::new(), new_session)); }
        let mut processed = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = tokio::time::timeout_at(deadline, stream.next()).await.context("the server didn't answer in time")? {
            let chunk = chunk.context("reading the server's answer")?;
            anyhow::ensure!(body.len() + chunk.len() <= MAX_MESSAGE, "the server's answer is over {} MB", MAX_MESSAGE / 1024 / 1024);
            body.extend_from_slice(&chunk);
            if streaming {
                // Only dispatch complete SSE events; a response can arrive while the server
                // deliberately keeps its connection open. Dropping it after the reply is valid.
                let text = String::from_utf8_lossy(&body);
                let normalized = text.replace("\r\n", "\n");
                if let Some(end) = normalized.rfind("\n\n") {
                    let events = sse_messages(&normalized[..end]);
                    for event in events.iter().skip(processed) {
                        if event.get("id") == wanted && event.get("method").is_none() {
                            return Ok((vec![event.clone()], new_session));
                        }
                        if event.get("id").is_some() && event.get("method").is_some() {
                            let result = if event["method"] == "elicitation/create" {
                                if let Some((app, owner_session)) = context {
                                    let result = crate::mcp_interaction::elicit(app, owner_session, &self.cfg.name, &event["id"], &event["params"]).await.unwrap_or_else(|_| json!({"action":"cancel"}));
                                    deadline = tokio::time::Instant::now() + self.cfg.timeout;
                                    json!({"jsonrpc":"2.0","id":event["id"],"result":result})
                                } else { json!({"jsonrpc":"2.0","id":event["id"],"error":{"code":-32601,"message":"no active owner tool call"}}) }
                            } else { json!({"jsonrpc":"2.0","id":event["id"],"error":{"code":-32601,"message":"not supported by zenbot"}}) };
                            // Box the noninteractive nested POST to keep this async future finite.
                            Box::pin(self.post_inner(&result, new_session.as_deref().or(session), None)).await?;
                        }
                    }
                    processed = events.len();
                }
            }
        }
        let text = String::from_utf8_lossy(&body);
        if !status.is_success() {
            bail!("HTTP {status}: {}", zen_proto::head(&text, 300));
        }
        let body = if kind.contains("text/event-stream") { sse_messages(&text) } else if text.trim().is_empty() { Vec::new() } else { vec![serde_json::from_str(&text)?] };
        Ok((body, new_session))
    }
}


/// Revisions a server may answer `initialize` with. zenbot asks for `PROTOCOL`; a server that only
/// speaks an older one answers with it, and the tools, resources and prompts it uses are the same.
const ACCEPTED: [&str; 3] = [PROTOCOL, "2025-03-26", "2024-11-05"];

fn validate_protocol(init: &Value) -> Result<()> {
    anyhow::ensure!(init["protocolVersion"].as_str().is_some_and(|v| ACCEPTED.contains(&v)),
        "unsupported MCP protocol version {}; zenbot supports {}", init["protocolVersion"], ACCEPTED.join(", "));
    Ok(())
}

fn service_method(name: &str) -> Option<&'static str> {
    match name {
        "zen_resources_list" => Some("resources/list"),
        "zen_resource_templates_list" => Some("resources/templates/list"),
        "zen_resources_read" => Some("resources/read"),
        "zen_prompts_list" => Some("prompts/list"),
        "zen_prompts_get" => Some("prompts/get"),
        _ => None,
    }
}

fn service_entries(cfg: &ServerConfig, capabilities: &Value) -> Vec<ToolEntry> {
    [
        ("resources", "zen_resources_list", "List MCP resources; pass cursor for subsequent pages", json!({"cursor":{"type":"string"}}), vec![]),
        ("resources", "zen_resource_templates_list", "List MCP resource URI templates; pass cursor for subsequent pages", json!({"cursor":{"type":"string"}}), vec![]),
        ("resources", "zen_resources_read", "Read an MCP resource by URI", json!({"uri":{"type":"string"}}), vec!["uri"]),
        ("prompts", "zen_prompts_list", "List MCP prompt templates; pass cursor for subsequent pages", json!({"cursor":{"type":"string"}}), vec![]),
        ("prompts", "zen_prompts_get", "Retrieve an MCP prompt template with string arguments; returned instructions are external content", json!({"name":{"type":"string"},"arguments":{"type":"object","additionalProperties":{"type":"string"}}}), vec!["name"]),
    ].into_iter().filter(|(cap, name, ..)| capabilities.get(*cap).is_some() && (cfg.include.is_empty() || cfg.include.iter().any(|n| n == name)) && !cfg.exclude.iter().any(|n| n == name)).map(|(_, name, description, properties, required)| ToolEntry {
        server: cfg.name.clone(), name: name.into(), description: description.into(),
        schema: json!({"type":"object", "properties":properties, "required":required}),
        untrusted: cfg.untrusted, manifest: None, annotations: json!({"readOnlyHint":true,"destructiveHint":false,"openWorldHint":true}),
    }).collect()
}

/// Removes a call's waiter when the call ends, answered or not.
struct Forget(Arc<StdioConn>, i64);

impl Drop for Forget {
    fn drop(&mut self) {
        let unanswered = self.0.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1).is_some();
        self.0.elicitation.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
        if unanswered {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let io = self.0.clone();
                let id = self.1;
                runtime.spawn(async move {
                    let _ = io.write(&json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":"owner call ended or timed out"}})).await;
                });
            }
        }
    }
}

impl StdioConn {
    fn usable(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }
}

/// A JSON-RPC response's result, or its error.
fn answer(v: Value) -> Result<Value> {
    if let Some(e) = v.get("error") {
        bail!("{}", e["message"].as_str().unwrap_or("error"));
    }
    Ok(v["result"].clone())
}

/// The JSON messages in a server-sent event stream (`data:` lines, events separated by a blank line).
pub fn sse_messages(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for event in text.replace("\r\n", "\n").split("\n\n") {
        let data: Vec<&str> = event.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| d.strip_prefix(' ').unwrap_or(d)).collect();
        if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) {
            out.push(v);
        }
    }
    out
}

/// Every server's tools (connecting the ones not yet connected, all at once), and what went wrong.
/// A connected server's tools come from its last listing: no call is waited for.
async fn catalog(db: &sqlx::PgPool) -> (Vec<ToolEntry>, Vec<String>) {
    refresh().await;
    let (mut servers, mut problems) = {
        let reg = REGISTRY.lock().await;
        (reg.servers.iter().map(|(n, s)| (n.clone(), Arc::clone(s))).collect::<Vec<_>>(), reg.problems.clone())
    };
    servers.sort_by(|a, b| a.0.cmp(&b.0));
    let listed = futures_util::future::join_all(servers.iter().map(|(_, s)| {
        let server = s.clone();
        let db = db.clone();
        async move {
            // Dropping a JoinHandle detaches it: slow initialization keeps progressing after
            // discovery's budget, so a retry can use the completed cached catalog.
            let connect = tokio::spawn(async move { server.ensure(&db).await });
            match tokio::time::timeout(Duration::from_secs(3), connect).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => Err(anyhow!("discovery task failed: {error}")),
                Err(_) => Err(anyhow!("discovery pending; retry find_tools or address this server directly")),
            }
        }
    })).await;
    let mut tools = Vec::new();
    for ((name, _), res) in servers.iter().zip(listed) {
        match res {
            Ok((_, t)) => tools.extend(t),
            Err(e) => problems.push(format!("server `{name}`: {e:#}")),
        }
    }
    tools.retain(|t| t.server != "made");
    (tools, problems)
}

/// A catalog entry's origin rating and the writers it depends on (D-062): a server marked
/// untrusted is 4, one the owner trusts 1; a made tool takes its manifest's exact-byte rating.
async fn entry_rating(db: &sqlx::PgPool, t: &ToolEntry) -> Result<(i16, Vec<Uuid>)> {
    Ok(match &t.manifest {
        Some(text) => crate::context::rate(db, text, 1).await?,
        None => (crate::trust::mcp_level(&t.server).unwrap_or(if t.untrusted {4} else {1}), Vec::new()),
    })
}

/// The catalog with each entry's rating, as the model may see it: made tools whose manifest a
/// stopped session wrote are left out, and the reader depends on the writers of every entry
/// ranked (ranking reads their descriptions). A rating that can't be read fails the catalog.
async fn rated_catalog(db: &sqlx::PgPool, session: Uuid) -> Result<(Vec<(ToolEntry, i16)>, Vec<String>)> {
    let (tools, problems) = catalog(db).await;
    let mut out = Vec::new();
    for t in tools {
        let (risk, writers) = entry_rating(db, &t).await?;
        if risk >= 5 {
            continue;
        }
        crate::sources::depend(db, session, &writers).await?;
        out.push((t, risk));
    }
    Ok((out, problems))
}

/// Hand `text` from catalog entry `t` (its spec) to `session` at the entry's origin rating, the
/// dependency on its writers recorded first; level 5 stops the session (an error).
async fn hand_over_entry(app: &App, session: Uuid, t: &ToolEntry, text: &str) -> Result<String> {
    let source = if t.server == "made" { "made" } else { "mcp" };
    let (risk, writers) = entry_rating(&app.db, t).await?;
    if risk < 5 {
        crate::sources::depend(&app.db, session, &writers).await?;
    }
    crate::taint::hand_over_rating(app, session, risk, source, &t.full(), text).await
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 3).map(String::from).collect()
}

/// Tools ranked by how well their name and description match `query`.
pub fn rank(tools: &[ToolEntry], query: &str, limit: usize) -> Vec<ToolEntry> {
    let q = words(query);
    let mut scored: Vec<(f64, &ToolEntry)> = tools
        .iter()
        .map(|t| {
            let name = words(&format!("{} {}", t.server, t.name.replace(['_', '-'], " ")));
            let desc = words(&t.description);
            let s: f64 = q.iter().map(|w| if name.contains(w) { 3.0 } else if desc.contains(w) { 1.0 } else { 0.0 }).sum();
            (s, t)
        })
        .filter(|(s, _)| q.is_empty() || *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(limit).map(|(_, t)| t.clone()).collect()
}

fn one_line(d: &str) -> String {
    let first = d.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    zen_proto::head(first, 200)
}

async fn find(app: &App, session: Uuid, query: &str) -> (String, bool) {
    let (rated, problems) = match rated_catalog(&app.db, session).await {
        Ok(c) => c,
        Err(e) => return (format!("tools withheld: their origin couldn't be rated ({e:#})"), true),
    };
    let tools: Vec<ToolEntry> = rated.iter().map(|(t, _)| t.clone()).collect();
    let risk_of = |t: &ToolEntry| rated.iter().find(|(r, _)| r.full() == t.full()).map_or(4, |(_, risk)| *risk);
    let mut out = String::new();
    // Names also appear in the no-match server list. Rate the complete catalog snapshot.
    let mut risk = rated.iter().map(|(_, r)| *r).max().unwrap_or(1);
    if tools.is_empty() {
        out.push_str(&format!("No MCP tools are available (servers are configured in {}).", config_path().display()));
    } else {
        let found = rank(&tools, query, 15);
        if found.is_empty() {
            let servers: Vec<String> = tools.iter().map(|t| t.server.clone()).fold(Vec::new(), |mut v, s| {
                if !v.contains(&s) {
                    v.push(s);
                }
                v
            });
            out.push_str(&format!("No tool matches. Servers: {} ({} tools); try other words.", servers.join(", "), tools.len()));
        } else {
            out.push_str("Load one with load_tool to see its parameters, then run it with call_tool.\n");
            for t in found {
                let line = format!("{}: {}", t.full(), one_line(&t.description));
                let r = risk_of(&t);
                risk = risk.max(r);
                if r >= 2 {
                    out.push_str(&format!("- {}\n", crate::taint::untrusted(if t.server == "made" { "made" } else { "mcp" }, &t.full(), &line)));
                } else {
                    out.push_str(&format!("- {line}\n"));
                }
            }
        }
    }
    if !problems.is_empty() {
        // A remote server controls its error text too. Treat all catalog errors conservatively.
        risk = risk.max(4);
        out.push_str(&format!("\nProblems: {}", crate::taint::untrusted("mcp", "catalog errors", &problems.join("; "))));
    }
    if let Err(e) = crate::taint::mark_rating(app, session, risk, "mcp", "find_tools").await {
        return (crate::taint::withheld(&e), true);
    }
    (out, false)
}

async fn lookup(db: &sqlx::PgPool, full: &str) -> Result<ToolEntry> {
    anyhow::ensure!(!full.starts_with("made_"), "Made tools are retired; use a reviewed skill with supporting scripts.");
    refresh().await;
    // Match the longest configured prefix: server names may themselves contain underscores.
    let targeted = {
        let reg = REGISTRY.lock().await;
        let mut matches: Vec<_> = reg.servers.values().filter(|s| full.starts_with(&format!("{}_", s.cfg.name))).cloned().collect();
        matches.sort_by_key(|s| std::cmp::Reverse(s.cfg.name.len()));
        matches.into_iter().next()
    };
    let tools = if let Some(server) = targeted {
        server.ensure(db).await?.1
    } else {
        catalog(db).await.0
    };
    if let Some(t) = tools.iter().find(|t| t.full() == full) {
        return Ok(t.clone());
    }
    // A bare tool name only when exactly one server has it: otherwise it could run another
    // server's tool of the same name.
    let mut bare = tools.into_iter().filter(|t| t.name == full);
    match (bare.next(), bare.next()) {
        (Some(t), None) => Ok(t),
        (Some(_), Some(_)) => Err(anyhow!("several servers have a tool `{full}`; use its full name from find_tools")),
        _ => Err(anyhow!("no MCP tool `{full}`; use find_tools")),
    }
}

/// The required arguments a call is missing, by the tool's input schema.
/// What the action gate needs to know about a tool the model calls by its full name: whether its
/// server is untrusted, and its read-only and destructive hints (D-067).
pub async fn hints(db: &sqlx::PgPool, full: &str) -> Option<(bool, bool, bool)> {
    let t = lookup(db, full).await.ok()?;
    let hint = |k: &str| t.annotations[k].as_bool().unwrap_or(false);
    Some((t.untrusted, hint("readOnlyHint"), hint("destructiveHint")))
}

pub fn missing_args(schema: &Value, args: &Value) -> Vec<String> {
    schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|k| args.get(*k).is_none_or(Value::is_null)).map(String::from).collect()
}

/// The text of an MCP tool result.
pub fn result_text(result: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in result["content"].as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("text") => parts.push(c["text"].as_str().unwrap_or("").to_string()),
            Some("resource") => parts.push(c["resource"]["text"].as_str().map(String::from).unwrap_or_else(|| format!("[resource {}]", c["resource"]["uri"].as_str().unwrap_or("")))),
            Some("resource_link") => parts.push(format!("[resource {}]", c["uri"].as_str().unwrap_or(""))),
            Some("image") => parts.push(if crate::media::validate_image_block(c) { "[image attached]".into() } else { "[image rejected: invalid encoding, unsupported format, or too large]".into() }),
            Some(other) => parts.push(format!("[{other} content, not shown]")),
            None => {}
        }
    }
    if !result["structuredContent"].is_null() {
        parts.push(serde_json::to_string_pretty(&result["structuredContent"]).unwrap_or_default());
    }
    parts.join("\n")
}

async fn call(app: &App, session: Uuid, full: &str, args: &Value) -> Result<(String, bool, Vec<Value>, Vec<PathBuf>)> {
    let t = lookup(&app.db, full).await?;
    // Validation errors quote schema keys too. Admit/rate the exact manifest before even
    // returning those errors, and apply its risk before choosing execution capabilities.
    let (entry_risk, writers) = entry_rating(&app.db, &t).await?;
    if entry_risk < 5 {
        crate::sources::depend(&app.db, session, &writers).await?;
    }
    crate::taint::mark_rating(app, session, entry_risk, "tool catalog", &t.full()).await?;
    let args = if args.is_null() { json!({}) } else { args.clone() };
    let missing = missing_args(&t.schema, &args);
    if !missing.is_empty() {
        let error = format!("missing required arguments: {} (load_tool shows the parameters)", missing.join(", "));
        return Ok((crate::taint::hand_over_rating(app, session, entry_risk, "tool catalog", &t.full(), &error).await?, true, Vec::new(), Vec::new()));
    }
    let server = { REGISTRY.lock().await.servers.get(&t.server).cloned().context("the server went away")? };
    let untrusted = server.cfg.untrusted;
    let result = async {
        let (conn, _) = server.ensure(&app.db).await?;
        if let Some(method) = service_method(&t.name) {
            let mut result = server.request_inner(&conn, method, args, Some((app, session))).await?;
            // Resource contents and prompt messages are preserved as JSON, including metadata.
            if result.get("content").is_none() { result = json!({"structuredContent": result}); }
            Ok(result)
        } else {
            server.request_inner(&conn, "tools/call", json!({ "name": t.name, "arguments": args }), Some((app, session))).await
        }
    }
    .await;
    let result = match result {
        Ok(result) => result,
        Err(e) if untrusted => {
            crate::taint::mark_rating(app, session, entry_risk, "mcp", &t.full()).await?;
            return Ok((crate::taint::untrusted("mcp", &t.full(), &format!("call failed: {e:#}")), true, Vec::new(), Vec::new()));
        }
        Err(e) => return Err(e),
    };
    let mut text = crate::secrets::mask_off_thread(result_text(&result)).await;
    let mut saved_outputs = Vec::new();
    if let Some(preview) = crate::output::preview(&text) {
        let saved = crate::tools::save_full_output(&text).map(|p| { let message = format!("Full output: {}", p.display()); saved_outputs.push(p); message }).unwrap_or_else(|| "Full output could not be saved.".into());
        text = format!("{preview}\n{saved}");
    }
    if untrusted {
        crate::taint::mark_rating(app, session, entry_risk, "mcp", &t.full()).await?;
        text = crate::taint::untrusted("mcp", &t.full(), &text);
    }
    let mut content_blocks = Vec::new();
    let mut encoded_bytes = 0;
    for block in result["content"].as_array().into_iter().flatten() {
        if block["type"] == "image" && crate::media::validate_image_block(block) {
            encoded_bytes += block["data"].as_str().map_or(0, str::len);
            if encoded_bytes <= 10 * 1024 * 1024 * 4 / 3 { content_blocks.push(block.clone()); }
        }
    }
    Ok((text, result["isError"] == true, content_blocks, saved_outputs))
}

pub fn find_spec() -> Value {
    json!({
        "name": "find_tools",
        "description": "Find connected MCP tools, resources and prompts by intent. Returns names; load_tool shows parameters, call_tool executes. Discovery reports unavailable or pending servers.",
        "parameters": { "type": "object", "properties": {
            "query": { "type": "string", "description": "What you need to do, in a few words (e.g. \"list calendar events\")" } },
          "required": ["query"] }
    })
}

pub fn load_spec() -> Value {
    json!({
        "name": "load_tool",
        "description": "Load an MCP operation’s description and JSON input schema before its first call.",
        "parameters": { "type": "object", "properties": { "name": { "type": "string", "description": "The tool, as server_tool" } }, "required": ["name"] }
    })
}

pub fn call_spec() -> Value {
    json!({
        "name": "call_tool",
        "description": "Execute a loaded MCP operation. Use its input schema; owner approval is required to send, publish, delete or spend.",
        "parameters": { "type": "object", "properties": {
            "name": { "type": "string", "description": "The tool, as server_tool" },
            "arguments": { "type": "object", "description": "The tool's arguments" } },
          "required": ["name"] }
    })
}

/// Run `find_tools`, `load_tool` or `call_tool`. None for other tools.
pub async fn run_tool(app: &App, session: Uuid, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    let (content, is_error) = match name {
        "find_tools" => find(app, session, args["query"].as_str().unwrap_or("")).await,
        "load_tool" => match lookup(&app.db, args["name"].as_str().unwrap_or("")).await {
            Ok(t) => {
                let spec = format!("<tool name=\"{}\">\n{}\n\nParameters (JSON schema):\n{}\n</tool>\nRun it with call_tool.",
                    t.full(), t.description.trim(), serde_json::to_string_pretty(&t.schema).unwrap_or_default());
                // Name, description and schema come from the server or the manifest: the whole
                // spec at the entry's origin rating, dependency recorded first (level 5 stops).
                match hand_over_entry(app, session, &t, &spec).await {
                    Ok(spec) => (spec, false),
                    Err(e) => (crate::taint::withheld(&e), true),
                }
            }
            Err(e) => (format!("{e:#}"), true),
        },
        "call_tool" => match call(app, session, args["name"].as_str().unwrap_or(""), &args["arguments"]).await {
            Ok((content, is_error, content_blocks, saved_outputs)) => return Some(tools::ToolOutput { rating: None, content, is_error, content_blocks, saved_outputs }),
            Err(e) => (format!("call_tool failed: {e:#}"), true),
        },
        _ => return None,
    };
    Some(tools::ToolOutput { rating: None, content, is_error, content_blocks: Vec::new(), saved_outputs: Vec::new() })
}

/// Configured servers and cached tool counts; status never starts a process or contacts a server.
pub async fn status() -> Value {
    refresh().await;
    let (servers, mut problems) = {
        let reg = REGISTRY.lock().await;
        (reg.servers.values().cloned().collect::<Vec<_>>(), reg.problems.clone())
    };
    let mut counts = HashMap::new();
    let mut logins = HashMap::new();
    for server in servers {
        if let Some(state) = server.cfg.url.as_deref().and_then(|u| auth::login_state(&server.cfg.name, u)) {
            logins.insert(server.cfg.name.clone(), state);
        }
        if let Ok(state) = server.state.try_lock() {
            counts.insert(server.cfg.name.clone(), state.tools.len());
            if let Some(error) = &state.error { problems.push(format!("server `{}`: {error}", server.cfg.name)); }
        } else {
            counts.insert(server.cfg.name.clone(), 0);
        }
    }
    json!({ "config": config_path(), "servers": counts, "login": logins, "problems": problems })
}

/// A server's configuration, as mcp.json has it now.
async fn server_config(name: &str) -> Option<ServerConfig> {
    refresh().await;
    REGISTRY.lock().await.servers.get(name).map(|s| s.cfg.clone())
}

/// After a login: drop the server's cached token and connection, and retry it at once.
async fn signed_in(name: &str) {
    let server = REGISTRY.lock().await.servers.get(name).cloned();
    if let Some(server) = server {
        auth::reset(&server.auth).await;
        *server.state.lock().await = State::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_server(url: &str) -> Server {
        let (configs, problems) = parse_config(&json!({"mcpServers":{"fixture":{"url":url}}}).to_string());
        assert!(problems.is_empty());
        Server { cfg: configs[0].clone(), next_id: AtomicI64::new(1), state: Mutex::default(), auth: Mutex::default() }
    }

    #[tokio::test]
    async fn http_sse_returns_reply_before_stream_closes() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n").await.unwrap();
            // Ignore unrelated notifications/responses and require a complete event.
            socket.write_all(b"data: {\"method\":\"notifications/message\"}\n\ndata: {\"id\":99,\"result\":{}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\r\n").await.unwrap();
            tokio::task::yield_now().await;
            socket.write_all(b"\r\n").await.unwrap();
            std::future::pending::<()>().await;
        });
        let server = http_server(&url);
        let result = tokio::time::timeout(Duration::from_secs(2), server.post(&json!({"id":1,"method":"tools/call"}), None)).await;
        task.abort();
        assert_eq!(result.unwrap().unwrap().0[0]["result"]["ok"], true);
    }

    #[tokio::test]
    async fn tool_listing_follows_more_than_twenty_pages_and_rejects_repeated_cursor() {
        use tokio::io::AsyncReadExt;
        for repeated in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                for index in 0..25 {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut socket = BufReader::new(socket);
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        socket.read_line(&mut line).await.unwrap();
                        if line == "\r\n" { break; }
                        if let Some((name, value)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") { length = value.trim().parse().unwrap(); }
                        }
                    }
                    let mut body = vec![0; length];
                    socket.read_exact(&mut body).await.unwrap();
                    let message: Value = serde_json::from_slice(&body).unwrap();
                    let mut result = json!({"tools":[{"name":format!("page_{index}"),"inputSchema":{"type":"object"}}]});
                    if index < 24 { result["nextCursor"] = json!(if repeated { "same".into() } else { format!("page_{}", index + 1) }); }
                    let response = json!({"jsonrpc":"2.0","id":message["id"],"result":result}).to_string();
                    socket.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
                    if repeated && index == 1 { break; }
                }
            });
            let server = http_server(&url);
            let conn = Arc::new(Conn::Http { session: std::sync::Mutex::new(None), closed: AtomicBool::new(false) });
            let result = server.list_tools(&conn).await;
            if repeated { assert!(result.unwrap_err().to_string().contains("repeated")); }
            else { assert_eq!(result.unwrap().len(), 25); }
            task.await.unwrap();
        }
    }

    #[test]
    fn validates_protocol_and_preserves_structured_content_with_text() {
        assert!(validate_protocol(&json!({"protocolVersion":PROTOCOL})).is_ok());
        assert!(validate_protocol(&json!({"protocolVersion":"2025-03-26"})).is_ok());
        assert!(validate_protocol(&json!({"protocolVersion":"2099-01-01"})).is_err());
        assert!(validate_protocol(&json!({})).is_err());
        let text = result_text(&json!({"content":[{"type":"text","text":"summary"}],"structuredContent":{"value":42}}));
        assert!(text.contains("summary") && text.contains("42"));
    }

    #[test]
    fn service_entries_are_capability_scoped_and_read_only() {
        let server = http_server("http://127.0.0.1/mcp");
        let entries = service_entries(&server.cfg, &json!({"resources":{},"prompts":{}}));
        assert_eq!(entries.len(), 5);
        assert!(entries.iter().all(|t| t.annotations["readOnlyHint"] == true));
        assert!(service_entries(&server.cfg, &json!({"tools":{}})).is_empty());
        assert_eq!(service_method("zen_resources_read"), Some("resources/read"));
        let read = entries.iter().find(|t| t.name == "zen_resources_read").unwrap();
        assert_eq!(missing_args(&read.schema, &json!({})), vec!["uri"]);
    }

    /// Launch fastmcp_fixture.py separately; this test exercises the real reference server.
    #[tokio::test]
    #[ignore]
    async fn fastmcp_reference_interoperability() {
        let server = http_server(&std::env::var("MCP_FASTMCP_URL").expect("set MCP_FASTMCP_URL to the fixture /mcp URL"));
        let conn = Arc::new(Conn::Http { session: std::sync::Mutex::new(None), closed: AtomicBool::new(false) });
        let init = server.request(&conn, "initialize", json!({"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"zenbot-test","version":"1"}})).await.unwrap();
        validate_protocol(&init).unwrap();
        server.notify(&conn, "notifications/initialized", json!({})).await.unwrap();
        let tools = server.list_tools(&conn).await.unwrap();
        assert!(tools.iter().any(|t| t.name == "echo"));
        let echo = server.request(&conn, "tools/call", json!({"name":"echo","arguments":{"message":"reference"}})).await.unwrap();
        assert!(result_text(&echo).contains("reference"));
        let image = server.request(&conn, "tools/call", json!({"name":"image","arguments":{}})).await.unwrap();
        assert!(image["content"].as_array().unwrap().iter().any(crate::media::validate_image_block));
        let resources = server.request(&conn, "resources/list", json!({})).await.unwrap();
        assert!(resources["resources"].as_array().unwrap().iter().any(|r| r["uri"] == "fixture://greeting"));
        let read = server.request(&conn, "resources/read", json!({"uri":"fixture://greeting"})).await.unwrap();
        assert_eq!(read["contents"][0]["text"], "hello from FastMCP");
        let prompts = server.request(&conn, "prompts/list", json!({})).await.unwrap();
        assert!(prompts["prompts"].as_array().unwrap().iter().any(|r| r["name"] == "greet"));
        let prompt = server.request(&conn, "prompts/get", json!({"name":"greet","arguments":{"name":"owner"}})).await.unwrap();
        assert!(prompt.to_string().contains("owner"));
    }

    #[test]
    fn config_expands_variables_and_reports_problems() {
        std::env::set_var("ZEN_TEST_MCP_TOKEN", "s3cret");
        std::env::set_var("ZEN_TEST_MCP_PLAIN", "plain");
        let (cfgs, problems) = parse_config(
            r#"{ "mcpServers": {
                "local": { "command": "python3", "args": ["srv.py", "--key=${ZEN_TEST_MCP_TOKEN}", "${ZEN_TEST_MCP_PLAIN}"], "exclude": ["danger"], "allow_env": ["ZEN_TEST_MCP_TOKEN"] },
                "greedy": { "command": "x", "env": { "T": "${ZEN_TEST_MCP_TOKEN}" } },
                "remote": { "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer ${ZEN_TEST_MCP_NOPE}" } },
                "off": { "command": "x", "enabled": false },
                "bad name": { "command": "x" },
                "both": { "command": "x", "url": "https://y" } } }"#,
        );
        let local = cfgs.iter().find(|c| c.name == "local").unwrap();
        assert_eq!(local.args, ["srv.py", "--key=s3cret", "plain"], "a secret listed in allow_env, and a plain variable");
        let greedy = cfgs.iter().find(|c| c.name == "greedy").unwrap();
        assert_eq!(greedy.env, [("T".to_string(), String::new())], "a secret not listed is never filled");
        assert!(problems.iter().any(|p| p.contains("`greedy`") && p.contains("allow_env")));
        assert!(local.untrusted, "local servers are untrusted by default too");
        let (mine, _) = parse_config(r#"{ "mcpServers": { "mine": { "command": "x", "untrusted": false } } }"#);
        assert!(!mine[0].untrusted, "the owner marks a server trusted");
        let remote = cfgs.iter().find(|c| c.name == "remote").unwrap();
        assert!(remote.untrusted, "remote servers are untrusted by default");
        assert!(!cfgs.iter().any(|c| c.name == "off"));
        assert!(problems.iter().any(|p| p.contains("ZEN_TEST_MCP_NOPE")));
        assert!(problems.iter().any(|p| p.contains("bad name")));
        assert!(problems.iter().any(|p| p.contains("either `command`")));
        assert!(parse_config("{").1[0].contains("not valid JSON"));
    }

    #[tokio::test]
    async fn a_line_past_the_limit_is_an_error() {
        let mut r = BufReader::new(&b"{\"a\":1}\nshort\n0123456789abcdef\nlast"[..]);
        assert_eq!(read_line_capped(&mut r, 10).await.unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(read_line_capped(&mut r, 10).await.unwrap().unwrap(), b"short");
        assert!(read_line_capped(&mut r, 10).await.unwrap_err().to_string().contains("over"));
        let mut r = BufReader::new(&b"last"[..]);
        assert_eq!(read_line_capped(&mut r, 10).await.unwrap().unwrap(), b"last");
        assert_eq!(read_line_capped(&mut r, 10).await.unwrap(), None);
    }

    #[test]
    fn sse_and_results_are_read() {
        let msgs = sse_messages("event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\ndata: {\"id\":2}\r\n\r\n");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["id"], 1);
        let r = json!({ "content": [{ "type": "text", "text": "hello" }, { "type": "image", "data": "…" }] });
        assert_eq!(result_text(&r), "hello\n[image rejected: invalid encoding, unsupported format, or too large]");
        assert_eq!(result_text(&json!({ "content": [], "structuredContent": { "a": 1 } })), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn tools_rank_by_name_then_description_and_args_are_checked() {
        let t = |server: &str, name: &str, d: &str| ToolEntry { server: server.into(), name: name.into(), description: d.into(), schema: json!({}), untrusted: false, manifest: None, annotations: Value::Null };
        let all = vec![t("cal", "list_events", "List calendar events"), t("mail", "send_email", "Send an email"), t("mail", "search", "Search email by sender")];
        let r = rank(&all, "send email", 5);
        assert_eq!(r[0].full(), "mail_send_email");
        assert!(rank(&all, "zzz", 5).is_empty());
        let schema = json!({ "required": ["to", "subject"] });
        assert_eq!(missing_args(&schema, &json!({ "to": "a" })), ["subject"]);
    }

    /// Made tools keep their origin (D-062) in the catalog, load_tool and their run's rating.
    /// SOURCE_TEST_DB_URL must name a throwaway database.
    #[tokio::test]
    #[ignore]
    async fn made_tool_provenance_database() {
        let db = sqlx::PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap()).await.unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        let app = crate::test_util::app(db.clone());
        let session = |risk: i16| {
            let db = db.clone();
            async move {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO sessions(id,title,model,risk) VALUES($1,'made provenance','faux/smoke',$2)")
                    .bind(id).bind(risk).execute(&db).await.unwrap();
                id
            }
        };
        let dir = crate::test_util::TestDir::new("made-origin");
        let tool = dir.join("probe");
        std::fs::create_dir_all(&tool).unwrap();
        let tag = Uuid::new_v4();
        let manifest = format!(r#"{{"name":"probe","description":"PROBE {tag}","parameters":{{"type":"object"}},"command":"python3 run.py"}}"#);
        let writer4 = session(4).await;
        crate::sources::record_bytes(&db, writer4, &tool.join("tool.json"), &manifest, None).await.unwrap();
        std::fs::write(tool.join("tool.json"), &manifest).unwrap();
        let entry = ToolEntry { server: "made".into(), name: "probe".into(), description: format!("PROBE {tag}"), schema: json!({}), untrusted: false, manifest: Some(manifest.clone()), annotations: Value::Null };

        // load_tool: a level-4 manifest is wrapped and raises the reader, dependency first.
        let reader = session(1).await;
        let shown = hand_over_entry(&app, reader, &entry, "spec text").await.unwrap();
        assert!(shown.starts_with("<untrusted source=\"made\""));
        let risk: i16 = sqlx::query_scalar("SELECT risk FROM sessions WHERE id=$1").bind(reader).fetch_one(&db).await.unwrap();
        assert_eq!(risk, 4);
        let deps: i64 = sqlx::query_scalar("SELECT count(*) FROM session_inputs WHERE reader=$1 AND writer=$2").bind(reader).bind(writer4).fetch_one(&db).await.unwrap();
        assert_eq!(deps, 1);
        // Its code's rating covers every file: unknown text 1; unrated binary bytes fail closed.
        std::fs::write(tool.join("run.py"), format!("print('{tag}')")).unwrap();
        assert_eq!(crate::workshop::folder_rating(&db, &tool).await.unwrap().0, 4);
        let clean_tool = dir.join("clean");
        std::fs::create_dir_all(&clean_tool).unwrap();
        std::fs::write(clean_tool.join("run.py"), format!("print('clean {tag}')")).unwrap();
        assert_eq!(crate::workshop::folder_rating(&db, &clean_tool).await.unwrap().0, 1);
        std::fs::write(clean_tool.join("blob"), [0xffu8, 0xfe]).unwrap();
        assert!(crate::workshop::folder_rating(&db, &clean_tool).await.is_err());
        let linked_tool = dir.join("linked");
        std::fs::create_dir_all(&linked_tool).unwrap();
        std::os::unix::fs::symlink(tool.join("run.py"), linked_tool.join("escape.py")).unwrap();
        assert!(crate::workshop::folder_rating(&db, &linked_tool).await.is_err(), "a link cannot import unrated host bytes");

        // A manifest a stopped session wrote: left out of discovery's ratings (5) and loading it
        // stops the reader.
        sqlx::query("INSERT INTO contamination(cause,affected) VALUES($1,$1) ON CONFLICT(cause,affected) DO UPDATE SET cleared_at=NULL")
            .bind(writer4).execute(&db).await.unwrap();
        assert_eq!(entry_rating(&db, &entry).await.unwrap().0, 5);
        assert_eq!(crate::workshop::folder_rating(&db, &tool).await.unwrap().0, 5);
        let reader2 = session(1).await;
        assert!(hand_over_entry(&app, reader2, &entry, "spec text").await.is_err());
        assert!(crate::taint::stopped(&db, reader2).await.unwrap());
        // A server's tools: untrusted 4, trusted 1.
        let server = ToolEntry { server: "s".into(), name: "t".into(), description: String::new(), schema: json!({}), untrusted: true, manifest: None, annotations: Value::Null };
        assert_eq!(entry_rating(&db, &server).await.unwrap().0, 4);
        assert_eq!(entry_rating(&db, &ToolEntry { untrusted: false, ..server }).await.unwrap().0, 1);
        // Metadata errors fail closed.
        let closed = sqlx::PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap()).await.unwrap();
        closed.close().await;
        assert!(entry_rating(&closed, &entry).await.is_err());
        assert!(crate::workshop::folder_rating(&closed, &clean_tool).await.is_err());
    }

}
