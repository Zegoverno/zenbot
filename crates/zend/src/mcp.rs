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
//! its output as untrusted and taint the session; default true for every server, local or remote,
//! until the owner sets `"untrusted": false`; D-052). The agent can't change the file with `edit` or
//! `write` (agent.rs, `Protected::OwnerOnly`): only the owner does.
//!
//! The model's tool list never changes mid-session (that would break the prompt cache on every
//! engine; the Phase 0 spike), so MCP tools aren't added to it. Instead the model gets three fixed
//! tools: `find_tools` (names and one-line descriptions), `load_tool` (a tool's full definition, as
//! a tool result) and `call_tool` (run it). Every call goes through the kernel: recorded like any
//! tool call, with a timeout, output capped and secrets masked.
//!
//! Calls run at the same time, to one server and to several: a server's lock is held only to
//! connect, and its tools are looked up from its last listing (listed again when a stdio server
//! says they changed). A stdio server gets each request written under a stdin lock and a reader
//! task hands each answer to its call by id; it runs in its own process group, killed whole when
//! it's dropped. A message from a server (a stdio line, an HTTP body) over 8 MB fails the call.
//!
//! The client is written here, against the MCP spec (2025-06-18), instead of using the `rmcp` crate:
//! three methods are needed (initialize, tools/list, tools/call), and rmcp would add a second
//! reqwest and a C crypto build.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

use crate::{tools, App};

const PROTOCOL: &str = "2025-06-18";
const MAX_OUTPUT: usize = 50 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct ServerConfig {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub url: Option<String>,
    pub headers: Vec<(String, String)>,
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
        if s["enabled"] == false || s["disabled"] == true {
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
        let cfg = ServerConfig {
            name: name.clone(),
            args: strs("args", &mut missing),
            env: map("env", &mut missing),
            headers: map("headers", &mut missing),
            timeout: Duration::from_secs(s["timeout_s"].as_u64().unwrap_or(60).clamp(1, 600)),
            include: s["include"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect(),
            exclude: s["exclude"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect(),
            untrusted: s["untrusted"].as_bool().unwrap_or(true),
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
            // A request from the server (sampling, roots, …): not supported.
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

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().expect("http client"));

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
    reg.servers = cfgs.into_iter().map(|cfg| (cfg.name.clone(), Arc::new(Server { cfg, next_id: AtomicI64::new(1), state: Mutex::default() }))).collect();
    reg.loaded = mtime;
    reg.problems = problems;
}

impl Server {
    /// The live connection and the tools, connecting (or listing the tools again) when needed.
    async fn ensure(&self) -> Result<(Arc<Conn>, Vec<ToolEntry>)> {
        let mut st = self.state.lock().await;
        if let Some(conn) = st.conn.clone().filter(|c| c.usable()) {
            if !conn.stale() {
                return Ok((conn, st.tools.clone()));
            }
            if let Conn::Stdio(io) = &*conn {
                io.stale.store(false, Ordering::SeqCst);
            }
            match self.list_tools(&conn).await {
                Ok(tools) => st.tools = tools,
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
        match self.connect().await {
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

    async fn connect(&self) -> Result<(Arc<Conn>, Vec<ToolEntry>)> {
        let conn = match (&self.cfg.command, &self.cfg.url) {
            (Some(cmd), _) => {
                // A clean environment: the kernel's own (its token, API keys) never reaches a
                // server; only the basics and what the config gives it.
                let keep = ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TZ", "TMPDIR"];
                let mut child = Command::new(cmd)
                    .args(&self.cfg.args)
                    .env_clear()
                    .envs(keep.iter().filter_map(|k| std::env::var(k).ok().map(|v| (*k, v))))
                    .envs(self.cfg.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
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
            .request(&conn, "initialize", json!({ "protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": { "name": "zenbot", "version": env!("CARGO_PKG_VERSION") } }))
            .await?;
        anyhow::ensure!(init["protocolVersion"].is_string(), "the server's initialize answer has no protocolVersion");
        self.notify(&conn, "notifications/initialized", json!({})).await?;
        let tools = self.list_tools(&conn).await?;
        Ok((conn, tools))
    }

    async fn list_tools(&self, conn: &Arc<Conn>) -> Result<Vec<ToolEntry>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = self.request(conn, "tools/list", params).await?;
            for t in page["tools"].as_array().into_iter().flatten() {
                let name = t["name"].as_str().unwrap_or("").to_string();
                if name.is_empty() || (!self.cfg.include.is_empty() && !self.cfg.include.contains(&name)) || self.cfg.exclude.contains(&name) {
                    continue;
                }
                tools.push(ToolEntry {
                    server: self.cfg.name.clone(),
                    name,
                    description: t["description"].as_str().unwrap_or("").to_string(),
                    schema: t["inputSchema"].clone(),
                    untrusted: self.cfg.untrusted,
                });
            }
            cursor = page["nextCursor"].as_str().map(String::from);
            if cursor.is_none() {
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
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let timeout = self.cfg.timeout;
        let res = tokio::time::timeout(timeout, async {
            match &**conn {
                Conn::Stdio(io) => {
                    let (tx, rx) = oneshot::channel();
                    io.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
                    // Unanswered (a timeout, a dropped call): forget the waiter.
                    let _forget = Forget(io, id);
                    anyhow::ensure!(io.usable(), "the server exited");
                    io.write(&msg).await?;
                    rx.await.map_err(|_| anyhow!("the server exited"))?.map_err(|e| anyhow!(e))
                }
                Conn::Http { session, .. } => {
                    let current = session.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let (body, new_session) = self.post(&msg, current.as_deref()).await?;
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
        let url = self.cfg.url.as_deref().context("no url")?;
        let mut req = HTTP
            .post(url)
            .timeout(self.cfg.timeout)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL)
            .json(msg);
        for (k, v) in &self.cfg.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(s) = session {
            req = req.header("Mcp-Session-Id", s);
        }
        let resp = req.send().await.context("contacting the server")?;
        let status = resp.status();
        let new_session = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
        let kind = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let mut body: Vec<u8> = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading the server's answer")?;
            anyhow::ensure!(body.len() + chunk.len() <= MAX_MESSAGE, "the server's answer is over {} MB", MAX_MESSAGE / 1024 / 1024);
            body.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&body);
        if !status.is_success() {
            bail!("HTTP {status}: {}", zen_proto::head(&text, 300));
        }
        let body = if kind.contains("text/event-stream") { sse_messages(&text) } else if text.trim().is_empty() { Vec::new() } else { vec![serde_json::from_str(&text)?] };
        Ok((body, new_session))
    }
}

/// Removes a call's waiter when the call ends, answered or not.
struct Forget<'a>(&'a StdioConn, i64);

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
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
async fn catalog() -> (Vec<ToolEntry>, Vec<String>) {
    refresh().await;
    let (mut servers, mut problems) = {
        let reg = REGISTRY.lock().await;
        (reg.servers.iter().map(|(n, s)| (n.clone(), Arc::clone(s))).collect::<Vec<_>>(), reg.problems.clone())
    };
    servers.sort_by(|a, b| a.0.cmp(&b.0));
    let listed = futures_util::future::join_all(servers.iter().map(|(_, s)| s.ensure())).await;
    let mut tools = Vec::new();
    for ((name, _), res) in servers.iter().zip(listed) {
        match res {
            Ok((_, t)) => tools.extend(t),
            Err(e) => problems.push(format!("server `{name}`: {e:#}")),
        }
    }
    // The agent's own tools (workshop.rs), found and called the same way.
    tools.extend(crate::workshop::made_entries());
    (tools, problems)
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
    let (tools, problems) = catalog().await;
    let mut out = String::new();
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
            let has_untrusted = found.iter().any(|t| t.untrusted);
            for t in found {
                let line = format!("{}: {}", t.full(), one_line(&t.description));
                if t.untrusted {
                    out.push_str(&format!("- {}\n", crate::taint::untrusted("mcp", &t.full(), &line)));
                } else {
                    out.push_str(&format!("- {line}\n"));
                }
            }
            if has_untrusted {
                if let Err(e) = crate::taint::taint(app, session, "mcp", "find_tools").await {
                    return (crate::taint::withheld(&e), true);
                }
            }
        }
    }
    if !problems.is_empty() {
        // A remote server controls its error text too. Treat all catalog errors conservatively.
        if let Err(e) = crate::taint::taint(app, session, "mcp", "catalog errors").await {
            return (crate::taint::withheld(&e), true);
        }
        out.push_str(&format!("\nProblems: {}", crate::taint::untrusted("mcp", "catalog errors", &problems.join("; "))));
    }
    (out, false)
}

async fn lookup(full: &str) -> Result<ToolEntry> {
    let (tools, _) = catalog().await;
    if let Some(t) = tools.iter().find(|t| t.full() == full) {
        return Ok(t.clone());
    }
    // A bare tool name only when exactly one server has it: otherwise it could run another
    // server's tool (or a made tool) of the same name.
    let mut bare = tools.into_iter().filter(|t| t.name == full);
    match (bare.next(), bare.next()) {
        (Some(t), None) => Ok(t),
        (Some(_), Some(_)) => Err(anyhow!("several servers have a tool `{full}`; use its full name from find_tools")),
        _ => Err(anyhow!("no MCP tool `{full}`; use find_tools")),
    }
}

/// The required arguments a call is missing, by the tool's input schema.
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
            Some(other) => parts.push(format!("[{other} content, not shown]")),
            None => {}
        }
    }
    if parts.is_empty() && !result["structuredContent"].is_null() {
        parts.push(serde_json::to_string_pretty(&result["structuredContent"]).unwrap_or_default());
    }
    parts.join("\n")
}

async fn call(app: &App, session: Uuid, full: &str, args: &Value, force_sandbox: bool) -> Result<(String, bool)> {
    let t = lookup(full).await?;
    let args = if args.is_null() { json!({}) } else { args.clone() };
    let missing = missing_args(&t.schema, &args);
    if !missing.is_empty() {
        let error = format!("missing required arguments: {} (load_tool shows the parameters)", missing.join(", "));
        if t.untrusted {
            crate::taint::taint(app, session, "mcp", &t.full()).await?;
            return Ok((crate::taint::untrusted("mcp", &t.full(), &error), true));
        }
        anyhow::bail!("{error}");
    }
    if t.server == "made" {
        let (text, is_error, networked) = crate::workshop::run_made(&app.db, &t.name, &args, force_sandbox).await?;
        // An approved tool can reach the network: what it returns may be web content.
        if networked {
            crate::taint::taint(app, session, "made", &t.full()).await?;
            return Ok((crate::taint::untrusted("made", &t.full(), &text), is_error));
        }
        return Ok((text, is_error));
    }
    let server = { REGISTRY.lock().await.servers.get(&t.server).cloned().context("the server went away")? };
    let untrusted = server.cfg.untrusted;
    let result = async {
        let (conn, _) = server.ensure().await?;
        server.request(&conn, "tools/call", json!({ "name": t.name, "arguments": args })).await
    }
    .await;
    let result = match result {
        Ok(result) => result,
        Err(e) if untrusted => {
            crate::taint::taint(app, session, "mcp", &t.full()).await?;
            return Ok((crate::taint::untrusted("mcp", &t.full(), &format!("call failed: {e:#}")), true));
        }
        Err(e) => return Err(e),
    };
    let mut text = crate::secrets::mask_off_thread(result_text(&result)).await;
    if text.len() > MAX_OUTPUT {
        let saved = crate::tools::save_full_output(&text).map(|p| format!("the full output is in {}", p.display())).unwrap_or_else(|| "the rest was dropped".into());
        text = format!("{}\n[... cut at 50 KB; {saved} ...]", &text[..text.floor_char_boundary(MAX_OUTPUT)]);
    }
    if untrusted {
        crate::taint::taint(app, session, "mcp", &t.full()).await?;
        text = crate::taint::untrusted("mcp", &t.full(), &text);
    }
    Ok((text, result["isError"] == true))
}

pub fn find_spec() -> Value {
    json!({
        "name": "find_tools",
        "description": "Find tools from the owner's connected services (MCP servers: mail, calendar, documents, …) and tools you made (made_…) by what you need; returns names and one-liners. Then load_tool shows a tool's parameters and call_tool runs it.",
        "parameters": { "type": "object", "properties": {
            "query": { "type": "string", "description": "What you need to do, in a few words (e.g. \"list calendar events\")" } },
          "required": ["query"] }
    })
}

pub fn load_spec() -> Value {
    json!({
        "name": "load_tool",
        "description": "Show a tool's full description and parameters (its JSON schema), by the name find_tools gave. Load a tool \
before its first call_tool in a session.",
        "parameters": { "type": "object", "properties": { "name": { "type": "string", "description": "The tool, as server_tool" } }, "required": ["name"] }
    })
}

pub fn call_spec() -> Value {
    json!({
        "name": "call_tool",
        "description": "Run a tool from a connected service (found with find_tools, loaded with load_tool) with arguments that \
match its parameters. Ask the owner before calls that send, publish, delete or spend.",
        "parameters": { "type": "object", "properties": {
            "name": { "type": "string", "description": "The tool, as server_tool" },
            "arguments": { "type": "object", "description": "The tool's arguments" } },
          "required": ["name"] }
    })
}

/// Run `find_tools`, `load_tool` or `call_tool`. None for other tools.
pub async fn run_tool(app: &App, session: Uuid, name: &str, args: &Value, force_sandbox: bool) -> Option<tools::ToolOutput> {
    let (content, is_error) = match name {
        "find_tools" => find(app, session, args["query"].as_str().unwrap_or("")).await,
        "load_tool" => match lookup(args["name"].as_str().unwrap_or("")).await {
            Ok(t) => {
                let spec = format!("<tool name=\"{}\">\n{}\n\nParameters (JSON schema):\n{}\n</tool>\nRun it with call_tool.",
                    t.full(), t.description.trim(), serde_json::to_string_pretty(&t.schema).unwrap_or_default());
                // Name, description and schema all come from the server: wrap the entire spec.
                match if t.untrusted { crate::taint::taint(app, session, "mcp", &t.full()).await } else { Ok(()) } {
                    Ok(()) if t.untrusted => (crate::taint::untrusted("mcp", &t.full(), &spec), false),
                    Ok(()) => (spec, false),
                    Err(e) => (crate::taint::withheld(&e), true),
                }
            }
            Err(e) => (format!("{e:#}"), true),
        },
        "call_tool" => match call(app, session, args["name"].as_str().unwrap_or(""), &args["arguments"], force_sandbox).await {
            Ok(r) => r,
            Err(e) => (format!("call_tool failed: {e:#}"), true),
        },
        _ => return None,
    };
    Some(tools::ToolOutput { rating: None, content, is_error })
}

/// Servers and their tool counts (`GET /api/mcp`). Connects (starts) every configured server.
pub async fn status() -> Value {
    let (tools, problems) = catalog().await;
    let mut by: HashMap<String, usize> = HashMap::new();
    for t in &tools {
        *by.entry(t.server.clone()).or_default() += 1;
    }
    json!({ "config": config_path(), "servers": by, "problems": problems })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(result_text(&r), "hello\n[image content, not shown]");
        assert_eq!(result_text(&json!({ "content": [], "structuredContent": { "a": 1 } })), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn tools_rank_by_name_then_description_and_args_are_checked() {
        let t = |server: &str, name: &str, d: &str| ToolEntry { server: server.into(), name: name.into(), description: d.into(), schema: json!({}), untrusted: false };
        let all = vec![t("cal", "list_events", "List calendar events"), t("mail", "send_email", "Send an email"), t("mail", "search", "Search email by sender")];
        let r = rank(&all, "send email", 5);
        assert_eq!(r[0].full(), "mail_send_email");
        assert!(rank(&all, "zzz", 5).is_empty());
        let schema = json!({ "required": ["to", "subject"] });
        assert_eq!(missing_args(&schema, &json!({ "to": "a" })), ["subject"]);
    }
}
