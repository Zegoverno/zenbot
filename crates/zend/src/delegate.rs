//! Delegation.
//!
//! `delegate` hands a subtask to a subagent: a child session (kind `subagent`) with a fresh context
//! and the same instructions, memory and tools, except that it can't delegate further or ask the
//! owner. One call can hand out several tasks (`tasks`), which the kernel runs at the same time.
//! The parent gets each subagent's final answer.
//!
//! A task may name a `role` (per task, or for all): a profile from the caller's
//! `agents/<name>/roles.toml` (roles.rs) giving the subagent a model, an effort, a narrower tool list
//! (applied at each of its turns, never wider than it gets anyway) and skills, whose SKILL.md is
//! attached to its brief (its first prompt, not its instructions). The child session records the
//! role (`sessions.role`). An unknown role, or a broken roles file, fails the call. The tool's
//! description lists the roles as they were when the session's tools were first fixed, so a later
//! change to the file doesn't move the prompt prefix; a call always checks the current file.
//!
//! A subagent's model: the task's `model`, else its role's, else (handed to another agent) that
//! agent's, else the roles file's `default_model` (when a worker serves it), else the parent's.
//!
//! zenbot can hand a task to another agent (`agent`, per task or for all): the child is that
//! agent's session, with its instructions, tools, limits and vault, in its workspace (`agent.toml`
//! `workspace`, else the kernel's; a task's `dir` is taken relative to it). Only a zenbot session
//! may name another agent; a sandboxed agent's `dir` must lie inside its mounts.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{tools, AppState};

pub const SUBAGENT: &str = "subagent";
const SUBAGENT_NOTE: &str = "You are a subagent: the main agent gave you one task, below. Do it, then answer with what it needs \
to carry on: the result, how you checked it, and anything it must know or decide. You can't ask the owner or delegate further; \
take sensible defaults and say what you assumed. Be concise.";

/// The instructions a subagent gets: the session's own, plus how to work as a subagent.
pub fn system_for(base: &str) -> String {
    format!("{base}\n\n{SUBAGENT_NOTE}")
}

/// The last thing a session's agent said.
pub(crate) async fn final_answer(db: &sqlx::SqlitePool, session: Uuid) -> String {
    let rows = crate::tape::load(db, session, &["message"]).await.unwrap_or_default();
    rows.iter()
        .rev()
        .filter(|b| b.payload["role"] == "assistant")
        .map(|b| zen_proto::text_of(&b.payload["content"]))
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default()
}

/// The agent a task names, when it's another than the caller's: None for the caller's own. Only
/// zenbot may name another; an unknown name is an error listing the agents.
pub(crate) fn named_agent(caller: &crate::agents::Agent, named: Option<&str>) -> Result<Option<crate::agents::Agent>> {
    let Some(name) = named.map(str::trim).filter(|n| !n.is_empty() && *n != caller.name) else { return Ok(None) };
    anyhow::ensure!(caller.name == crate::agents::DEFAULT, "only zenbot can hand a task to another agent; leave out `agent`");
    let names: Vec<String> = crate::agents::list().into_iter().map(|(n, _)| n).collect();
    anyhow::ensure!(names.iter().any(|n| n == name), "no agent `{name}`; the agents are: {}", names.join(", "));
    crate::agents::load(name).map(Some)
}

/// A sandboxed agent can only work in a folder its sandbox shows it: refused up front otherwise.
fn dir_in_reach(sandbox: Option<&crate::sandbox::Sandbox>, dir: &std::path::Path) -> Result<()> {
    match sandbox {
        Some(s) => s.check(dir, false).map_err(|e| anyhow::anyhow!("{} can't work in {}: {e}; leave out `dir` or name one inside its mounts", s.agent, dir.display())),
        None => Ok(()),
    }
}

async fn delegate_all(app: &AppState, session: Uuid, workspace: &std::path::Path, who: &crate::agents::Agent, args: &Value) -> Result<(String, bool)> {
    let mut jobs: Vec<Value> = args["tasks"].as_array().cloned().unwrap_or_default();
    if let Some(t) = args["task"].as_str() {
        jobs.insert(0, json!({ "task": t, "model": args["model"], "dir": args["dir"] }));
    }
    // A top-level `agent` or `role` is every task's that doesn't name one.
    let jobs: Vec<Value> = jobs
        .into_iter()
        .map(|j| if j.is_string() { json!({ "task": j }) } else { j })
        .map(|mut j| {
            for key in ["agent", "role"] {
                if j[key].as_str().is_none() && args[key].is_string() {
                    j[key] = args[key].clone();
                }
            }
            j
        })
        .collect();
    anyhow::ensure!(!jobs.is_empty(), "delegate needs a `task` (or `tasks`): the goal, what to read first, and when it's done");
    anyhow::ensure!(jobs.len() <= 8, "at most 8 tasks at a time");
    // The caller's roles: a broken file fails the call (it also holds the default model).
    let roles = crate::roles::load(&who.name).context("delegate needs a valid roles file; fix it")?;
    // One model refresh for the whole call, not one per task.
    crate::collect_models(app).await;
    let served: Vec<String> = app.routes.lock().await.keys().cloned().collect();
    let runs = jobs.iter().map(|j| delegate(app, session, workspace, who, j, &served, &roles));
    let results = futures_util::future::join_all(runs).await;
    let mut out = Vec::new();
    let mut any_error = false;
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok((text, err)) => {
                any_error |= err;
                out.push(if jobs.len() > 1 { format!("## Task {}\n{text}", i + 1) } else { text });
            }
            Err(e) => {
                any_error = true;
                out.push(format!("## Task {}\nfailed: {e:#}", i + 1));
            }
        }
    }
    Ok((out.join("\n\n"), any_error))
}

async fn delegate(app: &AppState, session: Uuid, workspace: &std::path::Path, who: &crate::agents::Agent, args: &Value, served: &[String], roles: &crate::roles::Roles) -> Result<(String, bool)> {
    let task = args["task"].as_str().map(str::trim).filter(|t| !t.is_empty()).context("each task needs its text: the goal, what to read first, and when it's done")?;
    let row = sqlx::query("SELECT model, kind FROM sessions WHERE id = $1").bind(session).fetch_one(&app.db).await?;
    anyhow::ensure!(row.get::<Option<String>, _>("kind").as_deref() != Some(SUBAGENT), "a subagent can't delegate further");
    let agent = named_agent(who, args["agent"].as_str())?;
    let role_name = args["role"].as_str().map(str::trim).filter(|r| !r.is_empty());
    let role = role_name.map(|r| roles.get(r)).transpose()?;
    let parent: String = row.get("model");
    // The roles file's default is a soft one: when no worker serves it, the parent's model.
    let default = roles.default_model.as_deref().filter(|m| served.iter().any(|s| s == m));
    let task_model = args["model"].as_str().map(str::trim).filter(|m| !m.is_empty());
    let model = crate::roles::resolve_model(task_model, role, agent.as_ref().and_then(|a| a.model.as_deref()), default, &parent);
    if model != parent {
        anyhow::ensure!(served.contains(&model), "no worker serves `{model}`");
    }
    // A role's effort, when the model takes it (else the model's default).
    let effort = match role.and_then(|r| r.effort.as_deref()) {
        Some(e) if crate::workers::efforts(&crate::workers::model_info(app, &model).await.unwrap_or(Value::Null)).contains(&e) => Some(e),
        _ => None,
    };
    // The role's skills go with the brief, from the skills the subagent's agent sees.
    let brief = match role {
        Some(r) if !r.skills.is_empty() => with_skills(task, &r.skills, &agent.as_ref().unwrap_or(who).skill_roots())?,
        _ => task.to_string(),
    };
    // Another agent works in its own workspace (else the kernel's), never the caller's.
    let base = match &agent {
        Some(a) => a.workspace_dir().unwrap_or_else(|| app.workspace.clone()),
        None => workspace.to_path_buf(),
    };
    let dir = args["dir"].as_str().map(|d| tools::resolve(&base, d)).unwrap_or(base);
    if let Some(a) = &agent {
        dir_in_reach(a.sandbox().as_ref(), &dir)?;
    }
    let label = match (&agent, role_name) {
        (Some(a), Some(r)) => format!(" ({}, {r})", a.name),
        (Some(a), None) => format!(" ({})", a.name),
        (None, Some(r)) => format!(" ({r})"),
        (None, None) => String::new(),
    };
    app.emit(session, json!({ "type": "status", "text": format!("delegating to a subagent{label} on {model}") })).await;
    let started = std::time::Instant::now();
    let child = tokio::time::timeout(Duration::from_secs(1900), crate::run_child(app, session, SUBAGENT, &brief, &dir, crate::turns::Child { model: Some(&model), agent: agent.as_ref().map(|a| a.name.as_str()), role: role_name, effort }))
        .await
        .context("the subagent took too long")??;
    let answer = final_answer(&app.db, child).await;
    let cost: f64 = sqlx::query_scalar("SELECT COALESCE(SUM(cost_usd), 0.0) FROM turns WHERE session_id = $1").bind(child).fetch_one(&app.db).await.unwrap_or(0.0);
    let error: Option<String> = sqlx::query_scalar("SELECT error FROM turns WHERE session_id = $1 ORDER BY started_at DESC LIMIT 1").bind(child).fetch_optional(&app.db).await?.flatten();
    let head = format!(
        "Subagent {}{label} on {model} finished in {}s, ${cost:.3}{}:\n",
        &child.to_string()[..8],
        started.elapsed().as_secs(),
        error.as_deref().map(|e| format!(", with an error: {e}")).unwrap_or_default()
    );
    let body = if answer.trim().is_empty() { "(no answer)".to_string() } else { answer };
    Ok((format!("{head}{body}"), error.is_some()))
}

/// A brief with the SKILL.md of each named skill after it; an unknown skill is an error.
fn with_skills(task: &str, names: &[String], roots: &[std::path::PathBuf]) -> Result<String> {
    let skills = crate::skills::scan_roots(roots);
    let mut out = task.to_string();
    for name in names {
        let skill = crate::skills::lookup(&skills, name).map_err(|e| anyhow::anyhow!("the role's skill: {e}"))?;
        let text = crate::skills::load(skill, None).map_err(|e| anyhow::anyhow!("the role's skill {name}: {e}"))?;
        out.push_str(&format!("\n\n<skill name=\"{}/{}\">\nYour role uses this skill; follow it.\n\n{}\n</skill>", skill.domain, skill.name, text.trim_end()));
    }
    Ok(out)
}

/// A tool list whose `delegate` lists `agent`'s roles. The listing the session's tools already
/// had is kept (the prefix stays fixed when roles.toml changes); a new session, or a new
/// description of the tool, lists the file as it is now.
pub fn with_roles(mut tools: Value, agent: &str, current: Option<&Value>) -> Value {
    let base = spec()["description"].as_str().unwrap_or_default().to_string();
    let of = |list: &Value| list.as_array().and_then(|a| a.iter().find(|t| t["name"] == "delegate")).and_then(|t| t["description"].as_str()).map(String::from);
    let kept = current.and_then(of).filter(|d| d.starts_with(&base));
    let description = kept.unwrap_or_else(|| described(&base, crate::roles::load(agent)));
    if let Some(t) = tools.as_array_mut().and_then(|a| a.iter_mut().find(|t| t["name"] == "delegate")) {
        t["description"] = json!(description);
    }
    tools
}

fn described(base: &str, roles: Result<crate::roles::Roles>) -> String {
    match roles {
        Ok(r) if r.roles.is_empty() => base.to_string(),
        Ok(r) => format!("{base}\n\nRoles (name, model: what it's for):\n{}", r.listing()),
        Err(e) => format!("{base}\n\nRoles: your roles file is broken ({e:#}); fix it before delegating."),
    }
}

pub fn spec() -> Value {
    json!({
        "name": "delegate",
        "description": "Give a well-defined subtask to a subagent: a fresh session with your instructions, memory and tools, \
without ask or delegate. Use it to keep your context clean or, with several `tasks`, to work in parallel. Brief each task: goal, \
what to read first, what to avoid, what to report. Returns each final answer. A `role` gives the subagent a profile: model, tools, skills. Name an `agent` to hand the task to that agent (zenbot only): it works as itself, with its own tools, vault and workspace.",
        "parameters": { "type": "object", "properties": {
            "task": { "type": "string", "description": "The subtask, self-contained: the subagent sees nothing of this conversation" },
            "tasks": { "type": "array", "description": "Several subtasks to run at the same time, each {task, role?, model?, dir?, agent?}", "items": { "type": "object", "properties": {
                "task": { "type": "string" }, "role": { "type": "string" }, "model": { "type": "string" }, "dir": { "type": "string" }, "agent": { "type": "string" } }, "required": ["task"] } },
            "role": { "type": "string", "description": "Optional: one of your roles, below" },
            "agent": { "type": "string", "description": "Optional: the agent to hand the task(s) to (default: you)" },
            "model": { "type": "string", "description": "Optional: a model id (default: the role's)" },
            "dir": { "type": "string", "description": "Optional: the directory to work in (default: your workspace)" } },
          "required": [] }
    })
}

/// Run `delegate`. None for other tools.
pub async fn run_tool(app: &AppState, session: Uuid, workspace: &std::path::Path, who: &crate::agents::Agent, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    if name != "delegate" {
        return None;
    }
    Some(match delegate_all(app, session, workspace, who, args).await {
        Ok((content, is_error)) => tools::ToolOutput::text(content, is_error),
        Err(e) => tools::ToolOutput::error(format!("delegate failed: {e:#}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_zenbot_names_another_agent_and_an_unknown_one_is_listed() {
        let zenbot = crate::agents::tests_agent("zenbot", "none");
        let garry = crate::agents::tests_agent("garry", "none");
        assert!(named_agent(&zenbot, None).unwrap().is_none());
        assert!(named_agent(&zenbot, Some("zenbot")).unwrap().is_none());
        assert!(named_agent(&garry, Some("garry")).unwrap().is_none());
        let e = named_agent(&garry, Some("zenbot")).unwrap_err().to_string();
        assert!(e.contains("only zenbot"), "{e}");
        let e = named_agent(&zenbot, Some("no-such-agent")).unwrap_err().to_string();
        assert!(e.contains("no agent `no-such-agent`") && e.contains("zenbot"), "{e}");
    }

    #[test]
    fn the_roles_listed_stay_as_the_session_first_had_them() {
        let roles = crate::roles::parse(crate::roles::DEFAULT).unwrap();
        let base = spec()["description"].as_str().unwrap().to_string();
        let listed = described(&base, Ok(roles));
        assert!(listed.starts_with(&base) && listed.contains("- researcher (claude/claude-haiku-5-5): Find"), "{listed}");
        assert_eq!(described(&base, Ok(crate::roles::Roles::default())), base, "no roles, no list");
        assert!(described(&base, Err(anyhow::anyhow!("bad toml"))).contains("broken (bad toml)"));
        // An earlier listing is kept; one from an older description of the tool isn't.
        let tools = json!([{ "name": "read" }, spec()]);
        let earlier = json!([{ "name": "delegate", "description": format!("{base}\n\nRoles: the earlier list") }]);
        let kept = with_roles(tools.clone(), "zenbot", Some(&earlier));
        assert_eq!(kept[1]["description"], earlier[0]["description"]);
        let stale = json!([{ "name": "delegate", "description": "an old description" }]);
        assert!(with_roles(tools, "zenbot", Some(&stale))[1]["description"].as_str().unwrap().starts_with(&base));
    }

    #[test]
    fn a_role_brief_carries_its_skills() {
        let root = crate::test_util::TestDir::new("delegate-skills");
        std::fs::create_dir_all(root.join("work/verify")).unwrap();
        std::fs::write(root.join("work/verify/SKILL.md"), "---\nname: verify\ndescription: Check work.\n---\n\nRun the checks.\n").unwrap();
        let brief = with_skills("review it", &["work/verify".into()], &[root.to_path_buf()]).unwrap();
        assert!(brief.starts_with("review it\n\n<skill name=\"work/verify\">") && brief.contains("Run the checks.") && brief.ends_with("</skill>"), "{brief}");
        let e = with_skills("x", &["nope".into()], &[root.to_path_buf()]).unwrap_err().to_string();
        assert!(e.contains("no skill `nope`"), "{e}");
    }

    #[test]
    fn another_agent_works_only_inside_its_sandbox() {
        use crate::sandbox::{Access, Mount, Sandbox};
        let home = crate::test_util::TestDir::new("delegate-dir");
        let work = home.join("agents/garry/work");
        std::fs::create_dir_all(&work).unwrap();
        let garry = Sandbox::new("garry", vec![Mount::new(&work, Access::Write)]);
        let token = home.join("token");
        assert!(dir_in_reach(None, &token).is_ok(), "zenbot isn't sandboxed");
        assert!(dir_in_reach(Some(&garry), &work.join("sub")).is_ok());
        let e = dir_in_reach(Some(&garry), &token).unwrap_err().to_string();
        assert!(e.contains("garry can't work in") && e.contains("outside garry's sandbox"), "{e}");
    }
}
