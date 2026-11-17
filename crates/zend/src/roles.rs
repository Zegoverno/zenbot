//! Roles: profiles a subagent can be given, not agents. A role is a kind of work (its
//! description), a model, an optional effort, an optional tool list that narrows what the subagent
//! would get anyway (never widens it), and skills whose SKILL.md is attached to the subagent's
//! brief. `delegate` names one per task (`role`); the child session records it (`sessions.role`).
//!
//! Each agent keeps its roles in `agents/<name>/roles.toml`: the agent's file, not the owner's
//! (`agent.toml` is the owner's limits), so it can tune role models as it learns. It is written
//! from `defaults/agent/roles.toml` when missing (zenbot's by `defaults.rs`, another agent's by
//! `agents new` and before its sandbox is built, which binds it writable like `IDENTITY.md`). A
//! missing file means no roles; a broken one fails the delegate call that needs it.
//!
//! A subagent's model: the task's `model`, else the role's, else (handed to another agent) that
//! agent's, else `default_model`, else the parent's (`resolve_model`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;

pub const FILE: &str = "roles.toml";
pub const DEFAULT: &str = include_str!("../defaults/agent/roles.toml");

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Roles {
    /// The model of a subagent with no model and no role.
    pub default_model: Option<String>,
    #[serde(default)]
    pub roles: BTreeMap<String, Role>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Role {
    #[serde(default)]
    pub description: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The tools it may use, within what the subagent gets anyway (None: all of those).
    pub tools: Option<Vec<String>>,
    /// Skills (`domain/name` or `name`) whose SKILL.md goes with its brief.
    #[serde(default)]
    pub skills: Vec<String>,
}

/// An agent's roles file.
pub fn path(agent: &str) -> PathBuf {
    crate::agents::dir(agent).join(FILE)
}

/// Write the default roles file in an agent's folder when it has none. Never overwrites.
pub fn ensure(dir: &Path) {
    let p = dir.join(FILE);
    // Not `exists()`: a dangling link there would have the kernel create the file it names.
    if std::fs::symlink_metadata(&p).is_err() {
        if let Err(e) = std::fs::write(&p, DEFAULT) {
            tracing::warn!("creating {}: {e}", p.display());
        }
    }
}

/// The agent `agent`'s roles: none without a file; a broken file is an error naming it.
pub fn load(agent: &str) -> Result<Roles> {
    load_from(&path(agent))
}

pub fn load_from(p: &Path) -> Result<Roles> {
    match std::fs::read_to_string(p) {
        Ok(text) => parse(&text).map_err(|e| anyhow!("{}: {e:#}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Roles::default()),
        Err(e) => Err(anyhow!("{}: {e}", p.display())),
    }
}

pub fn parse(text: &str) -> Result<Roles> {
    let roles: Roles = toml::from_str(text).map_err(|e| anyhow!("{}", e.to_string().trim()))?;
    let model_ok = |m: &str| -> Result<()> {
        if !m.contains('/') {
            bail!("model `{m}` must be `<engine>/<model>`");
        }
        Ok(())
    };
    if let Some(m) = &roles.default_model {
        model_ok(m)?;
    }
    for (name, r) in &roles.roles {
        if !crate::agents::valid_name(name) {
            bail!("role `{name}`: names are lowercase letters, digits and hyphens");
        }
        if let Some(m) = &r.model {
            model_ok(m).map_err(|e| anyhow!("role `{name}`: {e}"))?;
        }
        let unknown: Vec<&str> = r.tools.iter().flatten().map(String::as_str).filter(|t| !crate::agents::TOOLS.contains(t)).collect();
        if !unknown.is_empty() {
            bail!("role `{name}`: unknown tool(s) {}; tools are: {}", unknown.join(", "), crate::agents::TOOLS.join(", "));
        }
    }
    Ok(roles)
}

impl Roles {
    /// The role `name`; an unknown one is an error listing the roles.
    pub fn get(&self, name: &str) -> Result<&Role> {
        self.roles.get(name).ok_or_else(|| {
            let names: Vec<&str> = self.roles.keys().map(String::as_str).collect();
            if names.is_empty() {
                anyhow!("no role `{name}`: there are no roles (agents/<name>/roles.toml); leave out `role`")
            } else {
                anyhow!("no role `{name}`; the roles are: {}", names.join(", "))
            }
        })
    }

    /// One line per role for the delegate tool's description: name, model and what it's for.
    pub fn listing(&self) -> String {
        self.roles
            .iter()
            .map(|(n, r)| format!("- {n} ({}): {}", r.model.as_deref().unwrap_or("default model"), r.description.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A subagent's model: the task's, else the role's, else the other agent's (handed to one), else
/// `default` (the roles file's `default_model`), else the parent's.
pub fn resolve_model(task: Option<&str>, role: Option<&Role>, agent: Option<&str>, default: Option<&str>, parent: &str) -> String {
    task.or(role.and_then(|r| r.model.as_deref())).or(agent).or(default).unwrap_or(parent).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_roles_parse() {
        let r = parse(DEFAULT).unwrap();
        assert_eq!(r.default_model.as_deref(), Some("claude/claude-sonnet-5-5"));
        assert_eq!(r.roles.keys().map(String::as_str).collect::<Vec<_>>(), ["coder", "researcher", "reviewer"]);
        let researcher = r.get("researcher").unwrap();
        assert_eq!(researcher.model.as_deref(), Some("claude/claude-haiku-5-5"));
        assert!(researcher.tools.as_ref().unwrap().iter().all(|t| !matches!(t.as_str(), "write" | "edit" | "bash")));
        assert_eq!(r.get("reviewer").unwrap().skills, ["work/verify"]);
        assert!(r.get("coder").unwrap().tools.is_none(), "the coder gets every tool");
        assert!(r.listing().contains("- researcher (claude/claude-haiku-5-5): Find"));
    }

    #[test]
    fn bad_files_are_refused_with_the_reason() {
        let e = parse("[roles.x]\nmodel = \"haiku\"").unwrap_err().to_string();
        assert!(e.contains("role `x`") && e.contains("<engine>/<model>"), "{e}");
        let e = parse("[roles.x]\ntools = [\"read\", \"teleport\"]").unwrap_err().to_string();
        assert!(e.contains("unknown tool(s) teleport"), "{e}");
        let e = parse("[roles.x]\nmodle = \"a/b\"").unwrap_err().to_string();
        assert!(e.contains("modle"), "{e}");
        let e = parse("[roles.Bad]\n").unwrap_err().to_string();
        assert!(e.contains("role `Bad`"), "{e}");
        assert_eq!(parse("").unwrap(), Roles::default());
    }

    #[test]
    fn an_unknown_role_lists_the_roles() {
        let r = parse(DEFAULT).unwrap();
        let e = r.get("poet").unwrap_err().to_string();
        assert!(e.contains("no role `poet`") && e.contains("coder, researcher, reviewer"), "{e}");
        let e = Roles::default().get("poet").unwrap_err().to_string();
        assert!(e.contains("there are no roles"), "{e}");
    }

    #[test]
    fn model_resolution_order() {
        let roles = parse("default_model = \"d/default\"\n[roles.r]\nmodel = \"r/role\"\n[roles.plain]\n").unwrap();
        let (r, plain, d) = (roles.get("r").ok(), roles.get("plain").ok(), roles.default_model.as_deref());
        assert_eq!(resolve_model(Some("t/task"), r, Some("a/agent"), d, "p/parent"), "t/task");
        assert_eq!(resolve_model(None, r, Some("a/agent"), d, "p/parent"), "r/role");
        assert_eq!(resolve_model(None, plain, Some("a/agent"), d, "p/parent"), "a/agent");
        assert_eq!(resolve_model(None, plain, None, d, "p/parent"), "d/default");
        assert_eq!(resolve_model(None, None, None, None, "p/parent"), "p/parent");
    }

    #[test]
    fn ensure_writes_once_and_a_missing_file_is_no_roles() {
        let dir = crate::test_util::TestDir::new("roles");
        assert_eq!(load_from(&dir.join(FILE)).unwrap(), Roles::default());
        ensure(&dir);
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), DEFAULT);
        std::fs::write(dir.join(FILE), "default_model = \"x/y\"").unwrap();
        ensure(&dir);
        assert_eq!(load_from(&dir.join(FILE)).unwrap().default_model.as_deref(), Some("x/y"));
        std::fs::write(dir.join(FILE), "nope = 1").unwrap();
        let e = load_from(&dir.join(FILE)).unwrap_err().to_string();
        assert!(e.contains("roles.toml") && e.contains("nope"), "{e}");
    }
}
