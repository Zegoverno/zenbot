//! Delegation.
//!
//! `delegate` hands a subtask to a subagent: a child session (kind `subagent`) with a fresh context
//! and the same instructions, memory and tools, except that it can't delegate further or ask the
//! owner. One call can hand out several tasks (`tasks`), which the kernel runs at the same time.
//! The parent gets each subagent's final answer. A subagent runs on the model the call names, else
//! on its parent's.
//!
//! zenbot can hand a task to another agent (`agent`, per task or for all): the child is that
//! agent's session, with its instructions, tools, limits and vault, in its workspace (`agent.toml`
//! `workspace`, else the kernel's; a task's `dir` is taken relative to it), on the task's model,
//! else the agent's, else the parent's. Only a zenbot session may name another agent; a sandboxed
//! agent's `dir` must lie inside its mounts.

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
    // A top-level `agent` is every task's that doesn't name one.
    let jobs: Vec<Value> = jobs
        .into_iter()
        .map(|j| if j.is_string() { json!({ "task": j }) } else { j })
        .map(|mut j| {
            if j["agent"].as_str().is_none() && args["agent"].is_string() {
                j["agent"] = args["agent"].clone();
            }
            j
        })
        .collect();
    anyhow::ensure!(!jobs.is_empty(), "delegate needs a `task` (or `tasks`): the goal, what to read first, and when it's done");
    anyhow::ensure!(jobs.len() <= 8, "at most 8 tasks at a time");
    // One model refresh for the whole call, not one per task.
    crate::collect_models(app).await;
    let served: Vec<String> = app.routes.lock().await.keys().cloned().collect();
    let runs = jobs.iter().map(|j| delegate(app, session, workspace, who, j, &served));
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

async fn delegate(app: &AppState, session: Uuid, workspace: &std::path::Path, who: &crate::agents::Agent, args: &Value, served: &[String]) -> Result<(String, bool)> {
    let task = args["task"].as_str().map(str::trim).filter(|t| !t.is_empty()).context("each task needs its text: the goal, what to read first, and when it's done")?;
    let row = sqlx::query("SELECT model, kind FROM sessions WHERE id = $1").bind(session).fetch_one(&app.db).await?;
    anyhow::ensure!(row.get::<Option<String>, _>("kind").as_deref() != Some(SUBAGENT), "a subagent can't delegate further");
    let agent = named_agent(who, args["agent"].as_str())?;
    let model = match args["model"].as_str().map(str::trim).filter(|m| !m.is_empty()).or(agent.as_ref().and_then(|a| a.model.as_deref())) {
        Some(m) => {
            anyhow::ensure!(served.iter().any(|s| s == m), "no worker serves `{m}`");
            m.to_string()
        }
        None => row.get("model"),
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
    let label = agent.as_ref().map(|a| format!(" ({})", a.name)).unwrap_or_default();
    app.emit(session, json!({ "type": "status", "text": format!("delegating to a subagent{label} on {model}") })).await;
    let started = std::time::Instant::now();
    let child = tokio::time::timeout(Duration::from_secs(1900), crate::run_child(app, session, SUBAGENT, task, &dir, Some(&model), agent.as_ref().map(|a| a.name.as_str())))
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

pub fn spec() -> Value {
    json!({
        "name": "delegate",
        "description": "Give a well-defined subtask to a subagent: a fresh session with your instructions, memory and tools, \
without ask or delegate. Use it to keep your context clean or, with several `tasks`, to work in parallel. Brief each task: goal, \
what to read first, what to avoid, what to report. Returns each final answer; a subagent runs on your model unless you name one. Name an `agent` to hand the task to that agent (zenbot only): it works as itself, with its own tools, vault and workspace.",
        "parameters": { "type": "object", "properties": {
            "task": { "type": "string", "description": "The subtask, self-contained: the subagent sees nothing of this conversation" },
            "tasks": { "type": "array", "description": "Several subtasks to run at the same time, each {task, model?, dir?, agent?}", "items": { "type": "object", "properties": {
                "task": { "type": "string" }, "model": { "type": "string" }, "dir": { "type": "string" }, "agent": { "type": "string" } }, "required": ["task"] } },
            "agent": { "type": "string", "description": "Optional: the agent to hand the task(s) to (default: you)" },
            "model": { "type": "string", "description": "Optional: a model id (default: yours)" },
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
