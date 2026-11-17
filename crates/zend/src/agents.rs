//! Agents: who a session is. zenbot is the default; the owner can add others, each a folder under
//! the zen home:
//!
//! ```text
//! agents/<name>/SOUL.md       who it is (the owner's file)
//! agents/<name>/IDENTITY.md   how it works (the agent keeps it)
//! agents/<name>/agent.toml    what it may use (optional; the owner's file)
//! agents/<name>/roles.toml    its roles for subagents (the agent keeps it; roles.rs)
//! agents/<name>/skills/       its own skills (optional), on top of the shared ones
//! ```
//!
//! Each agent has a vault: zenbot's is the global one (`global/`); another agent's is private,
//! `global/agents/<name>/` (`Agent::vault`), with its own `AGENTS.md`, `MEMORY.md` (its short-term
//! memory), `wiki/`, and records under `memory/` and `entities/`. `remember`, `brain put` and the
//! sleep write to the agent's own vault. `global` in `agent.toml` says how much of the global
//! vault another agent sees: none (default), read or write (`brain put` still writes to its own);
//! zenbot sees every vault. The environment (`AGENTS.md`) and the owner (`USER.md`) are shared.
//! Sessions are private: `history` and `search` reach only the agent's own (zenbot's reach all; the
//! owner's clients still show every session). zenbot can hand a task to another agent (`delegate`
//! with `agent`); no other agent can. An agent may have its own MCP servers (`agents/<name>/mcp.json`),
//! which only it sees; of the global ones (`~/.zenbot/mcp.json`, run as the owner) it gets only those
//! its `mcp` setting names (none when missing; zenbot: all). `agent.toml` can only narrow what zenbot has, never widen it: a
//! tool list, MCP servers and skills are filters over zenbot's own. The kernel enforces them (the
//! tool list a session is offered, `dispatch.rs` refusing anything outside it, MCP and skill
//! lookups) and no agent can change its own settings (`agent.toml`, `mcp.json`) with `write` or `edit`.
//! Every agent but zenbot is sandboxed (`sandbox.rs`, `Agent::mounts`): its `bash` runs in
//! bubblewrap, on a network of its own (pasta: internet, not the host's loopback), and its file
//! tools refuse other paths, so even with `bash` it reaches only its
//! workspace (by default `agents/<name>/work`), vault and folder, the system, the shared
//! instructions, the global vault as `global` allows, and the `read`/`write` paths of `agent.toml`.
//! Its folder is read-only to it but for `IDENTITY.md`, `roles.toml` and `work/`: the kernel trusts the rest
//! (settings, soul, skills, which `save_skill` writes for it), so the agent can't create a missing
//! `mcp.json` or `agent.toml` either; `workspace`/`write` paths inside the folder must be under
//! `work/`. Its own MCP servers run in its sandbox too (`mcp.rs`).
//!
//! The settings are read when a turn starts; a bad `agent.toml` fails the turn loudly rather than
//! running with tools the owner meant to take away.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// The default agent: every session from before agents existed is its.
pub const DEFAULT: &str = "zenbot";

/// Every tool an agent's `tools` or `deny` may name (zenbot's whole list, in `agent::specs` order).
pub const TOOLS: &[&str] = &[
    "bash", "read", "write", "edit", "history", "search", "ask", "remember", "brain", "web_search", "web_fetch", "find_skills", "load_skill",
    "save_skill", "find_tools", "load_tool", "call_tool", "verify", "delegate", "schedule", "judge", "work",
];

/// What `agent.toml` holds. Every key is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    /// One line on what the agent is for (shown when picking one).
    description: Option<String>,
    /// The tools it may use (from `TOOLS`); every tool when missing.
    tools: Option<Vec<String>>,
    /// Tools taken away from the list above.
    #[serde(default)]
    deny: Vec<String>,
    /// The global MCP servers (names in `mcp.json`) it may use; none when empty. When missing,
    /// every one for zenbot, none for any other agent (opt-in). Its own `mcp.json` always counts.
    mcp: Option<Vec<String>>,
    /// Whether it sees the shared skills besides its own (default true).
    shared_skills: Option<bool>,
    /// The model its new sessions start on (`<engine>/<model>`); the kernel's default when missing.
    model: Option<String>,
    /// Where its sessions' tools run (`~` allowed); the kernel's workspace when missing.
    workspace: Option<String>,
    /// The parts of the instructions besides its soul (`compile::PARTS`); all of them when missing.
    context: Option<Vec<String>>,
    /// How much of the global vault it sees: `none` (default), `read` or `write`.
    global: Option<String>,
    /// Paths it may read besides its own (`~` allowed; agents other than zenbot are sandboxed).
    #[serde(default)]
    read: Vec<String>,
    /// Paths it may write besides its own.
    #[serde(default)]
    write: Vec<String>,
}

/// How much of the global vault an agent sees besides its own (zenbot: all of it, every vault).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Global {
    None,
    Read,
    Write,
}

impl Global {
    pub fn as_str(self) -> &'static str {
        match self {
            Global::None => "none",
            Global::Read => "read",
            Global::Write => "write",
        }
    }
}

/// An agent and what it may use.
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub name: String,
    pub description: String,
    /// The tools it may use (None: every tool).
    tools: Option<BTreeSet<String>>,
    deny: BTreeSet<String>,
    /// The global MCP servers it may use (None: every one, only zenbot's default).
    pub mcp: Option<Vec<String>>,
    pub shared_skills: bool,
    pub model: Option<String>,
    pub workspace: Option<PathBuf>,
    /// The parts of its instructions, in `compile::PARTS` order (the soul always in).
    pub parts: Vec<&'static str>,
    /// Its `global` setting (zenbot's is always write: `global_level`).
    global: Global,
    /// Extra paths of its sandbox (`read`, `write` in agent.toml).
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
}

impl Agent {
    /// Whether this agent may use the tool `name`. `submit_verdict` belongs to the verifier's
    /// role, not to an agent, and is left to `agent::refusal`.
    pub fn allows(&self, name: &str) -> bool {
        name == "submit_verdict" || (self.tools.as_ref().is_none_or(|t| t.contains(name)) && !self.deny.contains(name))
    }

    /// Why this agent may not call `name`, if it may not.
    pub fn refusal(&self, name: &str) -> Option<String> {
        (!self.allows(name)).then(|| format!("`{name}` isn't one of {}'s tools (agents/{}/agent.toml)", self.name, self.name))
    }

    /// A tool list (`agent::specs`) with only the tools this agent may use, in the same order. A
    /// sandboxed agent's `bash` says where it runs (the list is fixed per session, as its agent is).
    pub fn filter(&self, specs: Value) -> Value {
        match specs {
            Value::Array(all) => Value::Array(all.into_iter().filter(|t| self.allows(t["name"].as_str().unwrap_or(""))).map(|t| self.sandboxed_spec(t)).collect()),
            other => other,
        }
    }

    fn sandboxed_spec(&self, mut t: Value) -> Value {
        if self.name != DEFAULT && t["name"] == "bash" {
            if let Some(d) = t["description"].as_str() {
                let global = match self.global_level() { Global::None => "", Global::Read => ", the shared vault read-only", Global::Write => ", the shared vault" };
                let here = format!("Runs in a sandbox that sees only your workspace, vault and agent folder (read-only but for IDENTITY.md, roles.toml and work/; plus system and tool folders{global}, and paths your agent.toml adds); the rest of the home is hidden.");
                t["description"] = Value::String(d.replace("Runs as the owner on the host.", &here));
            }
        }
        t
    }

    /// Whether this agent may use the MCP server `server`.
    pub fn allows_server(&self, server: &str) -> bool {
        self.mcp.as_ref().is_none_or(|s| s.iter().any(|x| x == server))
    }

    /// The agent's own folder.
    pub fn dir(&self) -> PathBuf {
        dir(&self.name)
    }

    /// Its own skills folder (`agents/<name>/skills`); zenbot's skills are the shared ones.
    pub fn own_skills(&self) -> Option<PathBuf> {
        (self.name != DEFAULT).then(|| self.dir().join("skills"))
    }

    /// The skill folders it sees, its own first (an own skill wins over a shared one of the same
    /// name).
    pub fn skill_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = self.own_skills().into_iter().collect();
        if self.shared_skills {
            roots.push(crate::skills::root());
        }
        roots
    }

    /// Where `save_skill` writes: its own folder, or the shared one for zenbot.
    pub fn skill_home(&self) -> PathBuf {
        self.own_skills().unwrap_or_else(crate::skills::root)
    }

    /// Its vault: the global one for zenbot, `global/agents/<name>/` for another agent.
    pub fn vault(&self) -> PathBuf {
        vault(&self.name)
    }

    /// How much of the global vault it sees: zenbot always all of it.
    pub fn global_level(&self) -> Global {
        if self.name == DEFAULT { Global::Write } else { self.global }
    }

    /// Where its sessions run: its `workspace` setting; else zenbot's is the kernel's (None) and
    /// another agent's its own `agents/<name>/work` (created here), never the owner's home.
    pub fn workspace_dir(&self) -> Option<PathBuf> {
        self.workspace_dir_in(&crate::zen_home())
    }

    fn workspace_dir_in(&self, home: &Path) -> Option<PathBuf> {
        if self.workspace.is_some() || self.name == DEFAULT {
            return self.workspace.clone();
        }
        let dir = dir_in(home, &self.name).join("work");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("creating {}: {e}", dir.display());
        }
        Some(dir)
    }

    /// What its sandbox shows it (`sandbox.rs`): the one list both its `bash` and its file tools
    /// obey. Empty for zenbot, which isn't sandboxed.
    pub fn mounts(&self) -> Vec<crate::sandbox::Mount> {
        let user = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
        self.mounts_in(&user, &crate::zen_home())
    }

    /// Its sandbox; None for zenbot.
    pub fn sandbox(&self) -> Option<crate::sandbox::Sandbox> {
        (self.name != DEFAULT).then(|| crate::sandbox::Sandbox::new(&self.name, self.mounts()))
    }

    fn mounts_in(&self, user_home: &Path, home: &Path) -> Vec<crate::sandbox::Mount> {
        if self.name == DEFAULT {
            return Vec::new();
        }
        let (global, dir) = (home.join("global"), dir_in(home, &self.name));
        let vault = vault_in(&global, &self.name);
        let workspace = self.workspace_dir_in(home).unwrap_or_else(|| dir.join("work"));
        // Its folder is read-only in the sandbox but for IDENTITY.md, bound back writable: it must
        // exist to be bound (an agent whose file went missing gets the template again).
        let identity = dir.join("IDENTITY.md");
        if !identity.exists() {
            if let Err(e) = std::fs::write(&identity, include_str!("../defaults/agent/IDENTITY.md").replace("{{name}}", &title(&self.name))) {
                tracing::warn!("creating {}: {e}", identity.display());
            }
        }
        // Its roles.toml too, which it keeps the same way (roles.rs).
        crate::roles::ensure(&dir);
        let l = crate::sandbox::Layout {
            user_home, zen: home, global: &global, dir: &dir, vault: &vault, workspace: &workspace,
            global_level: self.global_level(), read: &self.read, write: &self.write,
        };
        crate::sandbox::mounts(&l)
    }

    /// Why its `workspace`, `read` or `write` paths can't be given to it, if one can't (anything
    /// of the zen home but its own folder and vault).
    fn refuse_paths(&self, home: &Path) -> Option<String> {
        if self.name == DEFAULT {
            return None;
        }
        let (dir, vault) = (dir_in(home, &self.name), vault_in(&home.join("global"), &self.name));
        if let Some(e) = self.workspace.iter().chain(&self.read).chain(&self.write).find_map(|p| crate::sandbox::refuse_path(p, home, &dir, &vault)) {
            return Some(e);
        }
        // What it writes in its own folder: only under work/ (the rest holds what the kernel trusts).
        let work = crate::sandbox::real_path(&dir.join("work"));
        let dir = crate::sandbox::real_path(&dir);
        self.workspace.iter().chain(&self.write).find_map(|p| {
            let real = crate::sandbox::real_path(p);
            (real.starts_with(&dir) && !real.starts_with(&work))
                .then(|| format!("{} would let {} change its own settings: in its folder it may write only under {}", p.display(), self.name, work.display()))
        })
    }

    /// Write its vault's skeleton when it has none (an agent from before vaults); cheap when it has.
    pub fn ensure_vault(&self) -> Result<()> {
        if self.name != DEFAULT {
            write_vault(&self.vault(), &self.name)?;
        }
        Ok(())
    }

    /// What clients show of it.
    pub fn summary(&self) -> Value {
        let tools: Vec<&str> = TOOLS.iter().copied().filter(|t| self.allows(t)).collect();
        json!({
            "name": self.name, "description": self.description,
            "tools": tools, "all_tools": self.tools.is_none() && self.deny.is_empty(), "mcp": self.mcp, "shared_skills": self.shared_skills,
            "model": self.model, "workspace": self.workspace.as_ref().map(|w| w.display().to_string()),
            "context": self.parts, "global": self.global_level().as_str(),
            "read": self.read.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(), "write": self.write.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        })
    }
}

/// Whether `name` is a valid agent name: a lowercase letter, then up to 31 lowercase letters,
/// digits and hyphens (a folder name and a word that reads well in `/new garry`).
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.ends_with('-')
}

/// An agent's folder under the zen home.
pub fn dir(name: &str) -> PathBuf {
    dir_in(&crate::zen_home(), name)
}

fn dir_in(home: &Path, name: &str) -> PathBuf {
    home.join("agents").join(name)
}

/// An agent's vault: the global one for zenbot, `global/agents/<name>/` for another.
pub fn vault(name: &str) -> PathBuf {
    vault_in(&crate::layout::global_dir(), name)
}

fn vault_in(global: &Path, name: &str) -> PathBuf {
    if name == DEFAULT { global.to_path_buf() } else { global.join("agents").join(name) }
}

/// Every agent's vault but zenbot's, as (name, folder): the folders under `global/agents/`, whether
/// or not the agent still exists (a vault on disk is indexed until it is moved away).
pub fn vaults() -> Vec<(String, PathBuf)> {
    vaults_in(&crate::layout::global_dir())
}

fn vaults_in(global: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(global.join("agents"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from).map(|n| (n, e.path())))
        .filter(|(n, p)| n != DEFAULT && valid_name(n) && p.is_dir())
        .collect();
    out.sort();
    out
}

/// The agent whose vault holds `path` (zenbot for anything else).
pub fn owner_of(path: &Path) -> String {
    owner_in(&crate::layout::global_dir(), path)
}

fn owner_in(global: &Path, path: &Path) -> String {
    path.strip_prefix(global.join("agents"))
        .ok()
        .and_then(|rest| rest.components().next())
        .and_then(|c| c.as_os_str().to_str())
        .filter(|n| valid_name(n))
        .unwrap_or(DEFAULT)
        .to_string()
}

/// The agent of the session `session` (zenbot when it has none).
pub async fn of_session(db: &sqlx::SqlitePool, session: uuid::Uuid) -> Result<String, sqlx::Error> {
    Ok(sqlx::query_scalar("SELECT agent FROM sessions WHERE id = $1").bind(session).fetch_optional(db).await?.unwrap_or_else(|| DEFAULT.into()))
}

/// Write a vault's skeleton where it's missing: its rules (`AGENTS.md`) and an empty `wiki/`.
/// Never overwrites; idempotent.
fn write_vault(vault: &Path, name: &str) -> Result<()> {
    let rules = vault.join("AGENTS.md");
    if rules.is_file() && vault.join("wiki").is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(vault.join("wiki"))?;
    // Not `exists()`: a dangling link there would have the kernel create the file it names.
    if std::fs::symlink_metadata(&rules).is_err() {
        std::fs::write(&rules, include_str!("../defaults/agent/VAULT.md").replace("{{name}}", &title(name)))?;
    }
    Ok(())
}

/// `garry` → `Garry`.
fn title(name: &str) -> String {
    let mut c = name.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// An agent with the `global` setting `global`, for other modules' tests.
#[cfg(test)]
pub(crate) fn tests_agent(name: &str, global: &str) -> Agent {
    from_settings(name, toml::from_str(&format!("global = \"{global}\"")).unwrap()).unwrap()
}

/// An agent with the settings `settings`, for other modules' tests.
#[cfg(test)]
pub(crate) fn tests_agent_with(name: &str, settings: &str) -> Agent {
    from_settings(name, toml::from_str(settings).unwrap()).unwrap()
}

/// An agent's soul, relative to the zen home.
pub fn soul_rel(name: &str) -> String {
    format!("agents/{name}/SOUL.md")
}

/// An agent's identity file, relative to the zen home.
pub fn identity_rel(name: &str) -> String {
    format!("agents/{name}/IDENTITY.md")
}

/// The agent `name`, with its settings. An agent other than zenbot must have its folder and soul;
/// a bad `agent.toml` is an error, never ignored.
pub fn load(name: &str) -> Result<Agent> {
    load_in(&crate::zen_home(), name)
}

fn load_in(home: &Path, name: &str) -> Result<Agent> {
    if !valid_name(name) {
        bail!("`{name}` isn't a valid agent name (lowercase letters, digits and hyphens)");
    }
    let dir = dir_in(home, name);
    if name != DEFAULT && !dir.join("SOUL.md").is_file() {
        bail!("no agent `{name}` (no {})", dir.join("SOUL.md").display());
    }
    let path = dir.join("agent.toml");
    // One self-contained message (file and cause), however a client prints errors.
    let bad = |e: &dyn std::fmt::Display| anyhow!("agent `{name}`: {}: {e}", path.display());
    let settings: Settings = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|e| bad(&e.to_string().trim()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(e) => return Err(bad(&e)),
    };
    let agent = from_settings(name, settings).map_err(|e| bad(&format!("{e:#}")))?;
    match agent.refuse_paths(home) {
        Some(why) => Err(bad(&why)),
        None => Ok(agent),
    }
}

fn from_settings(name: &str, s: Settings) -> Result<Agent> {
    let check = |list: &[String]| -> Result<()> {
        let unknown: Vec<&str> = list.iter().map(String::as_str).filter(|t| !TOOLS.contains(t)).collect();
        if !unknown.is_empty() {
            bail!("unknown tool(s) {}; tools are: {}", unknown.join(", "), TOOLS.join(", "));
        }
        Ok(())
    };
    if let Some(t) = &s.tools {
        check(t)?;
    }
    check(&s.deny)?;
    let parts = match &s.context {
        None => crate::compile::PARTS.to_vec(),
        Some(want) => {
            let unknown: Vec<&str> = want.iter().map(String::as_str).filter(|p| !crate::compile::PARTS.contains(p)).collect();
            if !unknown.is_empty() {
                bail!("unknown context part(s) {}; parts are: {}", unknown.join(", "), crate::compile::PARTS.join(", "));
            }
            crate::compile::PARTS.iter().copied().filter(|p| *p == "soul" || want.iter().any(|w| w == p)).collect()
        }
    };
    if let Some(m) = &s.model {
        if !m.contains('/') {
            bail!("model `{m}` must be `<engine>/<model>`");
        }
    }
    let workspace = s.workspace.as_deref().map(expand_home);
    if let Some(w) = &workspace {
        if !w.is_dir() {
            bail!("workspace {} isn't a folder", w.display());
        }
    }
    let global = match s.global.as_deref() {
        None | Some("none") => Global::None,
        Some("read") => Global::Read,
        Some("write") => Global::Write,
        Some(other) => bail!("global `{other}` must be none, read or write"),
    };
    Ok(Agent {
        name: name.into(),
        description: s.description.unwrap_or_default().trim().to_string(),
        tools: s.tools.map(|t| t.into_iter().collect()),
        deny: s.deny.into_iter().collect(),
        // Another agent gets the global servers (run as the owner) only when agent.toml names them.
        mcp: if name == DEFAULT { s.mcp } else { Some(s.mcp.unwrap_or_default()) },
        shared_skills: s.shared_skills.unwrap_or(true),
        model: s.model,
        workspace,
        parts,
        global,
        read: s.read.iter().map(|p| expand_home(p)).collect(),
        write: s.write.iter().map(|p| expand_home(p)).collect(),
    })
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/").or(if p == "~" { Some("") } else { None }) {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

/// Every agent with a folder and a soul (zenbot always first), each loaded or with what's wrong
/// with it.
pub fn list() -> Vec<(String, Result<Agent>)> {
    list_in(&crate::zen_home())
}

fn list_in(home: &Path) -> Vec<(String, Result<Agent>)> {
    let mut names: Vec<String> = std::fs::read_dir(home.join("agents"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n != DEFAULT && valid_name(n) && dir_in(home, n).join("SOUL.md").is_file())
        .collect();
    names.sort();
    names.insert(0, DEFAULT.into());
    names.into_iter().map(|n| (n.clone(), load_in(home, &n))).collect()
}

/// Create the agent `name`: its folder with a starting soul, identity and settings the owner then
/// edits. Refuses a name that exists.
pub fn create(name: &str, description: &str) -> Result<Agent> {
    create_in(&crate::zen_home(), name, description)
}

fn create_in(home: &Path, name: &str, description: &str) -> Result<Agent> {
    if !valid_name(name) {
        bail!("`{name}` isn't a valid agent name (a lowercase letter, then lowercase letters, digits and hyphens; at most 32)");
    }
    let dir = dir_in(home, name);
    if dir.exists() {
        bail!("agent `{name}` already exists ({})", dir.display());
    }
    let description = description.trim().replace('\n', " ");
    let title = title(name);
    let what = if description.is_empty() { "a new agent whose job the owner will describe here".to_string() } else { description.clone() };
    // Mid-sentence in the soul's first line ("…agents: runs my ideas…"), as written below it.
    let what_lower = {
        let mut c = what.chars();
        c.next().map(|f| f.to_lowercase().collect::<String>() + c.as_str()).unwrap_or_default()
    };
    let soul = include_str!("../defaults/agent/SOUL.md").replace("{{name}}", &title).replace("{{what_lower}}", &what_lower).replace("{{what}}", &what);
    let identity = include_str!("../defaults/agent/IDENTITY.md").replace("{{name}}", &title);
    let settings = include_str!("../defaults/agent/agent.toml").replace("{{description}}", &description.replace('"', "'"));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("SOUL.md"), soul)?;
    std::fs::write(dir.join("IDENTITY.md"), identity)?;
    std::fs::write(dir.join("agent.toml"), settings)?;
    std::fs::write(dir.join(crate::roles::FILE), crate::roles::DEFAULT)?;
    write_vault(&vault_in(&home.join("global"), name), name)?;
    load_in(home, name)
}

/// Whether the agent `name` exists: zenbot always, another one with its folder and soul (even if
/// its settings are broken, so the owner can open and fix it).
fn exists_in(home: &Path, name: &str) -> bool {
    valid_name(name) && (name == DEFAULT || dir_in(home, name).join("SOUL.md").is_file())
}

/// The agent `name`'s files as they are on disk (an empty string for a missing one), what it may
/// use (or what's wrong with its settings), and its folder: what the owner's apps show to edit.
pub fn files(name: &str) -> Result<Value> {
    files_in(&crate::zen_home(), name)
}

fn files_in(home: &Path, name: &str) -> Result<Value> {
    if !exists_in(home, name) {
        bail!("no agent `{name}`");
    }
    let dir = dir_in(home, name);
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
    let mut out = match load_in(home, name) {
        Ok(a) => a.summary(),
        Err(e) => json!({ "name": name, "error": format!("{e:#}") }),
    };
    out["soul"] = json!(read("SOUL.md"));
    out["identity"] = json!(read("IDENTITY.md"));
    out["settings"] = json!(read("agent.toml"));
    out["dir"] = json!(dir.display().to_string());
    Ok(out)
}

/// The owner's change to an agent's files (each one given is replaced whole). Settings are checked
/// before anything is written, so a bad `agent.toml` is refused, never saved; every file that
/// changes is backed up first (`backups/prompt-files`, as an agent's own edits are).
pub fn update(name: &str, soul: Option<&str>, identity: Option<&str>, settings: Option<&str>) -> Result<()> {
    update_in(&crate::zen_home(), name, soul, identity, settings)
}

fn update_in(home: &Path, name: &str, soul: Option<&str>, identity: Option<&str>, settings: Option<&str>) -> Result<()> {
    if !exists_in(home, name) {
        bail!("no agent `{name}`");
    }
    if let Some(text) = settings {
        let parsed: Settings = toml::from_str(text).map_err(|e| anyhow!("agent.toml: {}", e.to_string().trim()))?;
        from_settings(name, parsed).map_err(|e| anyhow!("agent.toml: {e:#}"))?;
    }
    if soul.is_some_and(|s| s.trim().is_empty()) {
        bail!("SOUL.md can't be empty: it says who the agent is");
    }
    let dir = dir_in(home, name);
    std::fs::create_dir_all(&dir)?;
    for (file, text) in [("SOUL.md", soul), ("IDENTITY.md", identity), ("agent.toml", settings)] {
        let Some(text) = text else { continue };
        let path = dir.join(file);
        if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
            continue;
        }
        if path.exists() {
            crate::memory::backup_file(home, &path)?;
        }
        let tmp = dir.join(format!(".{file}.tmp"));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(())
}

/// Remove the agent `name` (never zenbot): its folder moves to `backups/agents/<name>-<time>`, and
/// its vault to `vault/` inside it, so nothing is lost. Its sessions stay on record; a new turn in one fails, saying the agent is gone.
pub fn remove(name: &str) -> Result<PathBuf> {
    remove_in(&crate::zen_home(), name)
}

fn remove_in(home: &Path, name: &str) -> Result<PathBuf> {
    if name == DEFAULT {
        bail!("zenbot is the default agent and can't be removed");
    }
    if !exists_in(home, name) {
        bail!("no agent `{name}`");
    }
    let to = home.join("backups/agents").join(format!("{name}-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")));
    std::fs::create_dir_all(to.parent().unwrap_or(home))?;
    std::fs::rename(dir_in(home, name), &to)?;
    let vault = vault_in(&home.join("global"), name);
    if vault.exists() {
        std::fs::rename(&vault, to.join("vault"))?;
    }
    Ok(to)
}

/// Whether `path` (already resolved) is an agent's settings file (`agent.toml`, or its own
/// `mcp.json`): the agent never changes these with `write` or `edit`, or it could give itself back
/// what the owner took away, or servers that run code.
pub fn is_settings_file(home: &Path, path: &Path) -> bool {
    let agents = crate::agent::real(&home.join("agents"));
    path.file_name().is_some_and(|f| f == "agent.toml" || f == "mcp.json") && path.parent().and_then(Path::parent).is_some_and(|p| p == agents)
}

/// Every agent's soul and identity, relative to the zen home: the prompt files an edit backs up.
pub fn prompt_files(home: &Path) -> Vec<String> {
    let mut out = vec![soul_rel(DEFAULT), identity_rel(DEFAULT)];
    for e in std::fs::read_dir(home.join("agents")).into_iter().flatten().flatten() {
        let Some(n) = e.file_name().to_str().map(String::from) else { continue };
        if n != DEFAULT && valid_name(&n) {
            out.push(soul_rel(&n));
            out.push(identity_rel(&n));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(text: &str) -> Result<Agent> {
        from_settings("garry", toml::from_str(text)?)
    }

    #[test]
    fn names() {
        for ok in ["garry", "zen-fit", "a1"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "Garry", "1abc", "a b", "../x", "a/b", "x-", &"a".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn settings_narrow_and_fail_loudly() {
        let a = settings("tools = [\"read\", \"search\", \"web_fetch\", \"bash\"]\ndeny = [\"bash\"]\nmcp = []\ncontext = [\"user\", \"memory\"]").unwrap();
        assert!(a.allows("read") && a.allows("web_fetch") && !a.allows("bash") && !a.allows("write"));
        assert!(a.allows("submit_verdict"), "a verifier's own tool is its role's, not the agent's");
        assert!(!a.allows_server("github"));
        assert_eq!(a.parts, ["soul", "user", "memory"]);
        assert!(a.refusal("write").unwrap().contains("agents/garry/agent.toml"));
        let filtered = a.filter(json!([{ "name": "bash" }, { "name": "read" }, { "name": "write" }, { "name": "search" }]));
        assert_eq!(filtered, json!([{ "name": "read" }, { "name": "search" }]));
        let bash = |a: &Agent| a.filter(crate::tools::specs()).as_array().unwrap().iter().find(|t| t["name"] == "bash").unwrap()["description"].as_str().unwrap().to_string();
        let (garry, zenbot) = (tests_agent("garry", "read"), tests_agent(DEFAULT, "none"));
        assert!(bash(&garry).contains("Runs in a sandbox") && bash(&garry).contains("shared vault read-only") && !bash(&garry).contains("as the owner"));
        assert!(bash(&zenbot).contains("Runs as the owner on the host."));
        // Missing keys: everything zenbot has, but the global MCP servers, which are opt-in.
        let all = settings("").unwrap();
        assert!(TOOLS.iter().all(|t| all.allows(t)) && all.parts == crate::compile::PARTS);
        assert!(!all.allows_server("any") && all.summary()["mcp"] == json!([]));
        let named = settings("mcp = [\"github\"]").unwrap();
        assert!(named.allows_server("github") && !named.allows_server("any"));
        let zenbot = from_settings(DEFAULT, toml::from_str("").unwrap()).unwrap();
        assert!(zenbot.allows_server("any") && zenbot.summary()["mcp"].is_null(), "zenbot keeps every server");
        // Mistakes are errors, not silently wider (or narrower) agents.
        assert!(settings("tools = [\"rm\"]").unwrap_err().to_string().contains("unknown tool"));
        assert!(settings("deny = [\"shell\"]").is_err());
        assert!(settings("tool = [\"read\"]").is_err(), "a misspelt key");
        assert!(settings("context = [\"soul\", \"mood\"]").is_err());
        assert!(settings("model = \"opus\"").is_err());
        assert!(settings("workspace = \"/no/such/folder\"").is_err());
    }

    #[test]
    fn create_load_list_and_settings_files() {
        let home = crate::test_util::TestDir::new("agents");
        let made = create_in(&home, "garry", "Runs ideas through YC's frameworks").unwrap();
        assert_eq!(made.description, "Runs ideas through YC's frameworks");
        let soul = std::fs::read_to_string(home.join("agents/garry/SOUL.md")).unwrap();
        assert!(soul.contains("You are Garry, one of the owner's agents: runs ideas through YC's frameworks.") && soul.contains("\nRuns ideas through YC's frameworks. Do that job"), "{soul}");
        assert!(create_in(&home, "garry", "").unwrap_err().to_string().contains("already exists"));
        assert!(create_in(&home, "Bad", "").is_err());
        assert!(load_in(&home, "nobody").unwrap_err().to_string().contains("no agent"));
        let listed: Vec<String> = list_in(&home).into_iter().map(|(n, a)| format!("{n}:{}", a.is_ok())).collect();
        assert_eq!(listed, ["zenbot:true", "garry:true"]);
        std::fs::write(home.join("agents/garry/agent.toml"), "tools = [\"nope\"]").unwrap();
        assert!(load_in(&home, "garry").is_err() && list_in(&home)[1].1.is_err());
        let real = crate::agent::real(&home.join("agents/garry/agent.toml"));
        assert!(is_settings_file(&home, &real));
        assert!(is_settings_file(&home, &crate::agent::real(&home.join("agents/garry/mcp.json"))));
        assert!(!is_settings_file(&home, &crate::agent::real(&home.join("agents/garry/SOUL.md"))));
        assert_eq!(prompt_files(&home), ["agents/zenbot/SOUL.md", "agents/zenbot/IDENTITY.md", "agents/garry/SOUL.md", "agents/garry/IDENTITY.md"]);
    }

    #[test]
    fn the_owner_reads_edits_and_removes_an_agent() {
        let home = crate::test_util::TestDir::new("agents-admin");
        create_in(&home, "garry", "YC office hours").unwrap();
        let f = files_in(&home, "garry").unwrap();
        assert!(f["soul"].as_str().unwrap().contains("You are Garry") && f["settings"].as_str().unwrap().contains("description") && f["error"].is_null());
        assert_eq!(files_in(&home, "zenbot").unwrap()["soul"], "", "zenbot always exists, files or not");
        // A bad agent.toml is refused before anything is written.
        let err = update_in(&home, "garry", Some("new soul"), None, Some("tools = [\"rm\"]")).unwrap_err().to_string();
        assert!(err.contains("unknown tool"), "{err}");
        assert!(std::fs::read_to_string(home.join("agents/garry/SOUL.md")).unwrap().contains("You are Garry"), "nothing written");
        assert!(update_in(&home, "garry", Some("  "), None, None).is_err(), "an empty soul");
        update_in(&home, "garry", Some("You are Garry Tan's office hours."), None, Some("tools = [\"read\"]\n")).unwrap();
        assert_eq!(load_in(&home, "garry").unwrap().summary()["tools"], json!(["read"]));
        let backups = std::fs::read_dir(home.join("backups/prompt-files")).unwrap().count();
        assert_eq!(backups, 2, "the old soul and settings are kept");
        // A broken agent can still be opened and fixed.
        std::fs::write(home.join("agents/garry/agent.toml"), "tools = 3").unwrap();
        assert!(files_in(&home, "garry").unwrap()["error"].is_string());
        update_in(&home, "garry", None, None, Some("")).unwrap();
        assert!(load_in(&home, "garry").is_ok());
        assert!(update_in(&home, "nobody", Some("x"), None, None).is_err());
        // Removing moves it aside; zenbot stays.
        assert!(remove_in(&home, "zenbot").is_err() && remove_in(&home, "nobody").is_err());
        let moved = remove_in(&home, "garry").unwrap();
        assert!(moved.join("SOUL.md").is_file() && !home.join("agents/garry").exists());
        assert!(load_in(&home, "garry").unwrap_err().to_string().contains("no agent"));
    }

    #[test]
    fn vaults_and_the_global_setting() {
        assert_eq!(settings("").unwrap().global_level(), Global::None, "private by default");
        assert_eq!(settings("global = \"read\"").unwrap().global_level(), Global::Read);
        assert_eq!(settings("global = \"write\"").unwrap().summary()["global"], "write");
        assert!(settings("global = \"all\"").unwrap_err().to_string().contains("none, read or write"));
        let zenbot = from_settings(DEFAULT, Settings::default()).unwrap();
        assert_eq!(zenbot.global_level(), Global::Write, "zenbot always sees the global vault");
        let g = Path::new("/z/global");
        assert_eq!(vault_in(g, DEFAULT), g);
        assert_eq!(vault_in(g, "garry"), Path::new("/z/global/agents/garry"));
        assert_eq!(owner_in(g, Path::new("/z/global/agents/garry/memory/x.md")), "garry");
        assert_eq!(owner_in(g, Path::new("/z/global/memory/x.md")), DEFAULT);
        assert_eq!(owner_in(g, Path::new("/elsewhere/agents/garry/x.md")), DEFAULT);
    }

    #[test]
    fn an_agent_gets_its_vault_and_takes_it_when_removed() {
        let home = crate::test_util::TestDir::new("agents-vault");
        create_in(&home, "garry", "YC office hours").unwrap();
        let vault = home.join("global/agents/garry");
        let rules = std::fs::read_to_string(vault.join("AGENTS.md")).unwrap();
        assert!(rules.contains("Garry's private vault") && vault.join("wiki").is_dir(), "{rules}");
        assert_eq!(vaults_in(&home.join("global")), [("garry".to_string(), vault.clone())]);
        // Idempotent, and never over the agent's own rules; a missing part is filled in.
        std::fs::write(vault.join("AGENTS.md"), "my rules").unwrap();
        std::fs::remove_dir(vault.join("wiki")).unwrap();
        write_vault(&vault, "garry").unwrap();
        assert_eq!(std::fs::read_to_string(vault.join("AGENTS.md")).unwrap(), "my rules");
        assert!(vault.join("wiki").is_dir());
        let moved = remove_in(&home, "garry").unwrap();
        assert!(moved.join("vault/AGENTS.md").is_file() && !vault.exists());
        assert!(vaults_in(&home.join("global")).is_empty());
    }

    #[test]
    fn another_agent_works_in_its_own_folder_and_reaches_only_its_mounts() {
        let home = crate::test_util::TestDir::new("agents-sandbox");
        let owner = crate::test_util::TestDir::new("agents-sandbox-owner");
        let user = owner.to_path_buf();
        std::fs::create_dir_all(user.join("repo")).unwrap();
        create_in(&home, "garry", "").unwrap();
        let garry = load_in(&home, "garry").unwrap();
        let work = garry.workspace_dir_in(&home).unwrap();
        assert!(work == home.join("agents/garry/work") && work.is_dir(), "its own workspace, never the owner's home");
        assert_eq!(from_settings(DEFAULT, Settings::default()).unwrap().workspace_dir_in(&home), None, "zenbot: the kernel's");
        assert!(from_settings(DEFAULT, Settings::default()).unwrap().mounts_in(&user, &home).is_empty(), "zenbot isn't sandboxed");
        let s = crate::sandbox::Sandbox::new("garry", garry.mounts_in(&user, &home));
        assert!(s.check(&work.join("x"), true).is_ok() && s.check(&home.join("global/agents/garry/x"), true).is_ok());
        assert!(s.check(&user.join("repo/x"), false).is_err() && s.check(&home.join("token"), false).is_err());
        // Extra paths: anything of the zen home but its own is refused when the agent loads.
        let toml = home.join("agents/garry/agent.toml");
        std::fs::write(&toml, format!("read = [\"{}\"]\nwrite = [\"{}\"]", user.join("repo").display(), work.display())).unwrap();
        let garry = load_in(&home, "garry").unwrap();
        let s = crate::sandbox::Sandbox::new("garry", garry.mounts_in(&user, &home));
        assert!(s.check(&user.join("repo/x"), false).is_ok() && s.check(&user.join("repo/x"), true).is_err());
        assert_eq!(garry.summary()["read"], json!([user.join("repo").display().to_string()]));
        for bad in [home.join("global").display().to_string(), home.display().to_string(), home.join("agents").display().to_string(), "relative".into()] {
            std::fs::write(&toml, format!("read = [\"{bad}\"]")).unwrap();
            assert!(load_in(&home, "garry").is_err(), "{bad}");
        }
        for bad in [format!("workspace = \"{}\"", home.join("agents/garry").display()), format!("write = [\"{}\"]", home.join("agents/garry/skills").display())] {
            std::fs::create_dir_all(home.join("agents/garry/skills")).unwrap();
            std::fs::write(&toml, &bad).unwrap();
            assert!(load_in(&home, "garry").unwrap_err().to_string().contains("own settings"), "{bad}");
        }
        // A missing IDENTITY.md is made again before it's bound writable.
        std::fs::write(&toml, "").unwrap();
        std::fs::remove_file(home.join("agents/garry/IDENTITY.md")).unwrap();
        let s = crate::sandbox::Sandbox::new("garry", load_in(&home, "garry").unwrap().mounts_in(&user, &home));
        assert!(home.join("agents/garry/IDENTITY.md").is_file() && s.check(&home.join("agents/garry/IDENTITY.md"), true).is_ok());
        assert!(s.check(&home.join("agents/garry/mcp.json"), true).is_err());
        std::fs::write(&toml, format!("workspace = \"{}\"", home.display())).unwrap();
        assert!(load_in(&home, "garry").unwrap_err().to_string().contains("zen home"));
    }

    #[test]
    fn the_tool_names_are_zenbots() {
        let names: Vec<String> = crate::agent::specs(None).as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        for n in &names {
            assert!(TOOLS.contains(&n.as_str()), "`{n}` is offered but agents can't name it");
        }
    }
}
