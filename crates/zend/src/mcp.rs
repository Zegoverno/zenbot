//! MCP client: tools from the owner's MCP servers, found and loaded on demand (DESIGN.md, "Skills
//! and tools that improve themselves"; Phase 2).
//!
//! Servers are configured in `~/.zenbot/mcp.json`, the `mcpServers` shape Claude Code and FastMCP
//! use: `command`/`args`/`env` for a local server (stdio), or `url`/`headers` for a remote one
//! (streamable HTTP). `${VAR}` in any string is filled from the kernel's environment, so secrets
//! stay out of the file and out of the model's sight. Per server: `enabled`, `timeout_s`,
//! `include` / `exclude` (tool names), and `untrusted` (wrap its output as untrusted and taint the
//! session; default true for remote servers).
//!
//! The model's tool list never changes mid-session (that would break the prompt cache on every
//! engine; the Phase 0 spike), so MCP tools aren't added to it. Instead the model gets three fixed
//! tools: `find_tools` (names and one-line descriptions), `load_tool` (a tool's full definition, as
//! a tool result) and `call_tool` (run it). Every call goes through the kernel: recorded like any
//! tool call, with a timeout, output capped and secrets masked.
//!
//! The client is written here, against the MCP spec (2025-06-18), instead of using the `rmcp` crate:
//! three methods are needed (initialize, tools/list, tools/call), and rmcp would add a second
//! reqwest and a C crypto build.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
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

/// `${VAR}` filled from the environment (an unset variable becomes empty, and is reported).
pub fn expand(s: &str, missing: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find('}') {
            Some(j) => {
                let var = &after[..j];
                match std::env::var(var) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => missing.push(var.to_string()),
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
        let strs = |k: &str, missing: &mut Vec<String>| -> Vec<String> { s[k].as_array().into_iter().flatten().filter_map(Value::as_str).map(|x| expand(x, missing)).collect() };
        let map = |k: &str, missing: &mut Vec<String>| -> Vec<(String, String)> {
            s[k].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), expand(v.as_str().unwrap_or(""), missing))).collect()
        };
        let command = s["command"].as_str().map(|c| expand(c, &mut missing));
        let url = s["url"].as_str().map(|u| expand(u, &mut missing));
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
            untrusted: s["untrusted"].as_bool().unwrap_or(url.is_some()),
            command,
            url,
        };
        if !missing.is_empty() {
            problems.push(format!("server `{name}`: environment variables not set: {}", missing.join(", ")));
        }
        out.push(cfg);
    }
    (out, problems)
}

// ---------- connections ----------

struct Stdio_ {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

enum Conn {
    Stdio(Box<Stdio_>),
    Http { session: Option<String> },
}

struct Server {
    cfg: ServerConfig,
    conn: Option<Conn>,
    next_id: i64,
    tools: Vec<ToolEntry>,
    error: Option<String>,
}

/// The servers, keyed by name, with the config they were started from and when it was read.
struct Registry {
    servers: HashMap<String, Mutex<Server>>,
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
    let names: Vec<String> = cfgs.iter().map(|c| c.name.clone()).collect();
    reg.servers.retain(|n, _| names.contains(n));
    for cfg in cfgs {
        let same = match reg.servers.get(&cfg.name) {
            Some(s) => s.lock().await.cfg == cfg,
            None => false,
        };
        if !same {
            reg.servers.insert(cfg.name.clone(), Mutex::new(Server { cfg, conn: None, next_id: 1, tools: Vec::new(), error: None }));
        }
    }
    reg.loaded = mtime;
    reg.problems = problems;
}

impl Server {
    async fn connect(&mut self) -> Result<()> {
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
                    .kill_on_drop(true)
                    .spawn()
                    .with_context(|| format!("starting `{cmd}`"))?;
                let stdin = child.stdin.take().context("no stdin")?;
                let stdout = BufReader::new(child.stdout.take().context("no stdout")?);
                Conn::Stdio(Box::new(Stdio_ { _child: child, stdin, stdout }))
            }
            _ => Conn::Http { session: None },
        };
        self.conn = Some(conn);
        let init = self
            .request("initialize", json!({ "protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": { "name": "zenbot", "version": env!("CARGO_PKG_VERSION") } }))
            .await?;
        anyhow::ensure!(init["protocolVersion"].is_string(), "the server's initialize answer has no protocolVersion");
        self.notify("notifications/initialized", json!({})).await?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = self.request("tools/list", params).await?;
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
                });
            }
            cursor = page["nextCursor"].as_str().map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        self.tools = tools;
        self.error = None;
        Ok(())
    }

    async fn ensure(&mut self) -> Result<()> {
        if self.conn.is_none() {
            if let Err(e) = self.connect().await {
                self.conn = None;
                self.error = Some(format!("{e:#}"));
                return Err(e);
            }
        }
        Ok(())
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        match self.conn.as_mut().context("not connected")? {
            Conn::Stdio(io) => {
                io.stdin.write_all(format!("{msg}\n").as_bytes()).await?;
                io.stdin.flush().await?;
            }
            Conn::Http { session } => {
                let session = session.clone();
                let _ = self.post(&msg, session.as_deref()).await?;
            }
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let timeout = self.cfg.timeout;
        let res = tokio::time::timeout(timeout, async {
            match self.conn.as_mut().context("not connected")? {
                Conn::Stdio(io) => {
                    io.stdin.write_all(format!("{msg}\n").as_bytes()).await?;
                    io.stdin.flush().await?;
                    loop {
                        let mut line = String::new();
                        if io.stdout.read_line(&mut line).await? == 0 {
                            bail!("the server exited");
                        }
                        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
                        if v["id"] == json!(id) && v.get("method").is_none() {
                            return answer(v);
                        }
                        // A request from the server (sampling, roots, …): not supported.
                        if v.get("method").is_some() && v.get("id").is_some() {
                            let reply = json!({ "jsonrpc": "2.0", "id": v["id"], "error": { "code": -32601, "message": "not supported by zenbot" } });
                            io.stdin.write_all(format!("{reply}\n").as_bytes()).await?;
                        }
                    }
                }
                Conn::Http { session } => {
                    let session = session.clone();
                    let (body, new_session) = self.post(&msg, session.as_deref()).await?;
                    if let (Some(s), Some(Conn::Http { session })) = (new_session, self.conn.as_mut()) {
                        *session = Some(s);
                    }
                    let found = body.into_iter().find(|v| v["id"] == json!(id)).context("the server's answer had no response for the request")?;
                    answer(found)
                }
            }
        })
        .await;
        match res {
            Ok(r) => r,
            Err(_) => {
                // A stdio server that didn't answer may answer late; start it again next time.
                self.conn = None;
                bail!("no answer within {}s", timeout.as_secs())
            }
        }
    }

    /// POST a message (streamable HTTP): the answer is JSON or an event stream of JSON messages.
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
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("HTTP {status}: {}", zen_proto::head(&text, 300));
        }
        let body = if kind.contains("text/event-stream") { sse_messages(&text) } else if text.trim().is_empty() { Vec::new() } else { vec![serde_json::from_str(&text)?] };
        Ok((body, new_session))
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

/// Every server's tools (connecting the ones not yet connected), and what went wrong.
async fn catalog() -> (Vec<ToolEntry>, Vec<String>) {
    refresh().await;
    let reg = REGISTRY.lock().await;
    let mut tools = Vec::new();
    let mut problems = reg.problems.clone();
    let mut names: Vec<&String> = reg.servers.keys().collect();
    names.sort();
    for name in names {
        let mut s = reg.servers[name].lock().await;
        if let Err(e) = s.ensure().await {
            problems.push(format!("server `{name}`: {e:#}"));
            continue;
        }
        tools.extend(s.tools.iter().cloned());
    }
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

async fn find(query: &str) -> (String, bool) {
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
            for t in found {
                out.push_str(&format!("- {}: {}\n", t.full(), one_line(&t.description)));
            }
        }
    }
    if !problems.is_empty() {
        out.push_str(&format!("\nProblems: {}", problems.join("; ")));
    }
    (out, false)
}

async fn lookup(full: &str) -> Result<ToolEntry> {
    let (tools, problems) = catalog().await;
    tools.into_iter().find(|t| t.full() == full || t.name == full).ok_or_else(|| {
        let extra = if problems.is_empty() { String::new() } else { format!(" ({})", problems.join("; ")) };
        anyhow!("no MCP tool `{full}`; use find_tools{extra}")
    })
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

async fn call(app: &App, session: Uuid, full: &str, args: &Value) -> Result<(String, bool)> {
    let t = lookup(full).await?;
    let args = if args.is_null() { json!({}) } else { args.clone() };
    let missing = missing_args(&t.schema, &args);
    anyhow::ensure!(missing.is_empty(), "missing required arguments: {} (load_tool shows the parameters)", missing.join(", "));
    let (result, untrusted) = {
        let reg = REGISTRY.lock().await;
        let mut s = reg.servers.get(&t.server).context("the server went away")?.lock().await;
        s.ensure().await?;
        let untrusted = s.cfg.untrusted;
        (s.request("tools/call", json!({ "name": t.name, "arguments": args })).await?, untrusted)
    };
    let mut text = crate::secrets::mask_off_thread(result_text(&result)).await;
    if text.len() > MAX_OUTPUT {
        let path = crate::outputs_dir().unwrap_or_else(std::env::temp_dir).join(format!("mcp-{}.txt", Uuid::new_v4()));
        let _ = std::fs::write(&path, &text);
        text = format!("{}\n[... cut at 50 KB; the full output is in {} ...]", &text[..text.floor_char_boundary(MAX_OUTPUT)], path.display());
    }
    if untrusted {
        let first = sqlx::query("UPDATE sessions SET tainted_at = now() WHERE id = $1 AND tainted_at IS NULL").bind(session).execute(&app.db).await.map(|r| r.rows_affected() == 1).unwrap_or(false);
        if first {
            let _ = crate::tape::append(&app.db, session, "taint", &json!({ "source": "mcp", "about": t.full() })).await;
        }
        text = crate::web::untrusted("mcp", &t.full(), &text);
    }
    Ok((text, result["isError"] == true))
}

pub fn find_spec() -> Value {
    json!({
        "name": "find_tools",
        "description": "Search the extra tools from the owner's connected services (MCP servers: email, calendars, documents, \
finance, issue trackers, …). Returns tool names with one-line descriptions; then load_tool shows one's parameters and \
call_tool runs it. Use it when a job needs a service your built-in tools can't reach. Searching is free; don't guess tool \
names.",
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
pub async fn run_tool(app: &App, session: Uuid, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    let (content, is_error) = match name {
        "find_tools" => find(args["query"].as_str().unwrap_or("")).await,
        "load_tool" => match lookup(args["name"].as_str().unwrap_or("")).await {
            Ok(t) => (
                format!("<tool name=\"{}\">\n{}\n\nParameters (JSON schema):\n{}\n</tool>\nRun it with call_tool.", t.full(), t.description.trim(), serde_json::to_string_pretty(&t.schema).unwrap_or_default()),
                false,
            ),
            Err(e) => (format!("{e:#}"), true),
        },
        "call_tool" => match call(app, session, args["name"].as_str().unwrap_or(""), &args["arguments"]).await {
            Ok(r) => r,
            Err(e) => (format!("call_tool failed: {e:#}"), true),
        },
        _ => return None,
    };
    Some(tools::ToolOutput { content, is_error })
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
        let (cfgs, problems) = parse_config(
            r#"{ "mcpServers": {
                "local": { "command": "python3", "args": ["srv.py", "--key=${ZEN_TEST_MCP_TOKEN}"], "exclude": ["danger"] },
                "remote": { "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer ${ZEN_TEST_MCP_NOPE}" } },
                "off": { "command": "x", "enabled": false },
                "bad name": { "command": "x" },
                "both": { "command": "x", "url": "https://y" } } }"#,
        );
        let local = cfgs.iter().find(|c| c.name == "local").unwrap();
        assert_eq!(local.args, ["srv.py", "--key=s3cret"]);
        assert!(!local.untrusted, "local servers are trusted by default");
        let remote = cfgs.iter().find(|c| c.name == "remote").unwrap();
        assert!(remote.untrusted, "remote servers are untrusted by default");
        assert!(!cfgs.iter().any(|c| c.name == "off"));
        assert!(problems.iter().any(|p| p.contains("ZEN_TEST_MCP_NOPE")));
        assert!(problems.iter().any(|p| p.contains("bad name")));
        assert!(problems.iter().any(|p| p.contains("either `command`")));
        assert!(parse_config("{").1[0].contains("not valid JSON"));
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
        let t = |server: &str, name: &str, d: &str| ToolEntry { server: server.into(), name: name.into(), description: d.into(), schema: json!({}) };
        let all = vec![t("cal", "list_events", "List calendar events"), t("mail", "send_email", "Send an email"), t("mail", "search", "Search email by sender")];
        let r = rank(&all, "send email", 5);
        assert_eq!(r[0].full(), "mail_send_email");
        assert!(rank(&all, "zzz", 5).is_empty());
        let schema = json!({ "required": ["to", "subject"] });
        assert_eq!(missing_args(&schema, &json!({ "to": "a" })), ["subject"]);
    }
}
