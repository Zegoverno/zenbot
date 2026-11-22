//! The learning loop. When the owner archives one of their sessions with an outcome (done, partial,
//! not-done), the kernel queues a review: a fresh `review` session, as the archived session's
//! agent, on the review model (`ZEN_REVIEW_MODEL`), that reads the archived session (`history`)
//! and judges how the work went and how much the owner had to intervene. It may change only two
//! things: proposals for model changes (never direct roles.toml writes), and notes (IDENTITY.md, USER.md for
//! zenbot, memory through `remember`, a skill through `save_skill`). AGENTS.md and SOUL.md are the
//! owner's: a review only suggests changes to them. It ends with `submit_review`, stored in
//! `session_reviews`; a review that changed something or suggests something is reported.
//!
//! The facts the review starts from (duration, turns, interventions, tools, cost, models, skills,
//! verifications, roles) are counted here by the kernel, never by the model. Reviews run one at a
//! time in the background and never hold up the archive.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{tools, AppState};

/// The kind of a session that reviews an archived one.
pub const REVIEW: &str = "review";

/// The tools a review session may use.
pub const TOOLS: &[&str] = &["history", "read", "search", "find_skills", "load_skill", "save_skill", "remember", "edit", "write", "submit_review"];

const PROMPT: &str = include_str!("../defaults/review.md");

/// Reviews run one at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Queue a review of `session`, just archived with an outcome, when it is an owner's session and
/// reviews are on. Returns at once.
pub(crate) async fn queue(app: &AppState, session: Uuid) {
    if !crate::config::get().review {
        return;
    }
    let kind: Option<Option<String>> = sqlx::query_scalar("SELECT kind FROM sessions WHERE id = $1").bind(session).fetch_optional(&app.db).await.ok().flatten();
    let Some(kind) = kind else { return };
    if !crate::agent::owner_driven(kind.as_deref()) {
        return;
    }
    let (app, work) = (app.clone(), app.background_work());
    tokio::spawn(async move {
        let _work = work;
        let _one = ONE_AT_A_TIME.lock().await;
        if let Err(e) = run(&app, session).await {
            tracing::warn!("review of session {session}: {e:#}");
        }
    });
}

/// Run the review of `session` and report what it changed.
async fn run(app: &AppState, session: Uuid) -> Result<()> {
    let facts = facts(&app.db, session).await?;
    let agent = facts["agent"].as_str().unwrap_or("zenbot").to_string();
    let past = past_reviews(&app.db, &agent).await?;
    let prompt = render(&facts, &past);
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, title, model, parent, kind, workspace, agent, title_source, model_source)
         SELECT $1, 'review: ' || title, $2, id, $3, workspace, agent, 'owner', 'owner' FROM sessions WHERE id = $4",
    )
    .bind(id)
    .bind(&crate::config::get().review_model)
    .bind(REVIEW)
    .bind(session)
    .execute(&app.db)
    .await?;
    crate::turns::run_and_wait(app, id, REVIEW, &prompt).await?;
    sqlx::query("UPDATE sessions SET archived = true WHERE id = $1").bind(id).execute(&app.db).await?;
    let row = sqlx::query("SELECT changes, suggestions FROM session_reviews WHERE review_session_id = $1 ORDER BY id DESC LIMIT 1").bind(id).fetch_optional(&app.db).await?;
    if let Some(r) = row {
        let changes: Value = serde_json::from_str(&r.get::<String, _>("changes")).unwrap_or(json!([]));
        let suggestions: Value = serde_json::from_str(&r.get::<String, _>("suggestions")).unwrap_or(json!([]));
        if let Some(text) = report(&facts, &changes, &suggestions) {
            // Persist in the jobs feed: the board and Matrix already consume completed runs.
            deliver(&app.db, id, &text).await?;
            crate::tape::append(&app.db, id, "message", &json!({ "role": "assistant", "content": [{ "type": "text", "text": text }] })).await?;
            tracing::info!("review of {session}: {text}");
        }
    }
    Ok(())
}

/// Event-only system job: never scheduled, one completed run per nonempty report.
async fn deliver(db: &SqlitePool, session: Uuid, text: &str) -> Result<()> {
    let mut tx = crate::storage::begin(db).await?;
    sqlx::query("INSERT INTO jobs (name, kind, action, schedule, tz, enabled, created_by)
        SELECT 'session-review', 'system', 'session-review', 'event', 'UTC', 0, 'kernel'
        WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE kind = 'system' AND action = 'session-review')")
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO job_runs (job_id, trigger, status, session_id, ended_at, output)
        SELECT id, 'archive', 'ok', $1, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), $2 FROM jobs
        WHERE kind = 'system' AND action = 'session-review'
        AND NOT EXISTS (SELECT 1 FROM job_runs WHERE session_id = $1)")
        .bind(session).bind(text).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// What the kernel knows about how `session` went.
pub(crate) async fn facts(db: &SqlitePool, session: Uuid) -> Result<Value> {
    let s = sqlx::query("SELECT title, agent, model, outcome, outcome_note, rating, created_at, ended_at, CAST((julianday(COALESCE(ended_at, updated_at)) - julianday(created_at)) * 86400 AS INTEGER) AS secs FROM sessions WHERE id = $1")
        .bind(session)
        .fetch_one(db)
        .await?;
    let agent: Option<String> = s.get("agent");
    let agent = agent.unwrap_or_else(|| "zenbot".into());
    let t = sqlx::query(
        "SELECT COUNT(*) AS turns, COALESCE(SUM(tool_calls), 0) AS tool_calls, COALESCE(SUM(tool_errors), 0) AS tool_errors,
                COALESCE(SUM(cost_usd), 0.0) AS cost, COALESCE(SUM(input_tokens), 0) AS input_tokens, COALESCE(SUM(output_tokens), 0) AS output_tokens,
                COALESCE(SUM(outcome IN ('aborted', 'interrupted', 'stopped')), 0) AS aborts
         FROM turns WHERE session_id = $1",
    )
    .bind(session)
    .fetch_one(db)
    .await?;
    // Kernel prompts have an explicit marker, even in an owner-driven session.
    let owner_messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tape_events WHERE session_id = $1 AND kind = 'message' AND json_extract(payload, '$.role') = 'user' AND COALESCE(json_extract(payload, '$.kernel'), 0) = 0")
        .bind(session).fetch_one(db).await?;
    let steers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tape_events WHERE session_id = $1 AND kind = 'message' AND json_extract(payload, '$.steer') = 1")
        .bind(session).fetch_one(db).await?;
    let models: Vec<String> = sqlx::query_scalar("SELECT DISTINCT model FROM turns WHERE session_id = $1 ORDER BY model").bind(session).fetch_all(db).await?;
    let children = sqlx::query("SELECT kind, role, model, COUNT(*) AS n FROM sessions WHERE parent = $1 GROUP BY kind, role, model ORDER BY kind, role, model").bind(session).fetch_all(db).await?;
    let children: Vec<Value> = children
        .iter()
        .map(|r| json!({ "kind": r.get::<Option<String>, _>("kind"), "role": r.get::<Option<String>, _>("role"), "model": r.get::<String, _>("model"), "count": r.get::<i64, _>("n") }))
        .collect();
    let mut skills: Vec<String> = sqlx::query_scalar("SELECT DISTINCT name FROM brain_skill_uses WHERE session_id = $1").bind(session).fetch_all(db).await?;
    let loaded: Vec<String> = sqlx::query_scalar("SELECT DISTINCT json_extract(args, '$.name') FROM tool_calls WHERE session_id = $1 AND name = 'load_skill' AND json_extract(args, '$.name') IS NOT NULL")
        .bind(session)
        .fetch_all(db)
        .await?;
    for l in loaded {
        if !skills.contains(&l) {
            skills.push(l);
        }
    }
    skills.sort();
    let verifications: Vec<String> = sqlx::query_scalar("SELECT payload FROM tape_events WHERE session_id = $1 AND kind = 'verification' ORDER BY id").bind(session).fetch_all(db).await?;
    let verifications: Vec<Value> = verifications
        .iter()
        .filter_map(|p| serde_json::from_str::<Value>(p).ok())
        .map(|v| {
            let results: Vec<&str> = v["results"].as_array().into_iter().flatten().filter_map(|r| r["result"].as_str()).collect();
            let count = |k: &str| results.iter().filter(|r| **r == k).count();
            json!({ "pass": count("pass"), "fail": count("fail"), "uncertain": count("uncertain"), "verifier": v["verifier"] })
        })
        .collect();
    let roles: Value = match crate::roles::load(&agent) {
        Ok(r) => json!({ "default_model": r.default_model, "roles": r.roles.iter().map(|(k, v)| (k.clone(), json!({ "model": v.model, "description": v.description }))).collect::<serde_json::Map<_, _>>() }),
        Err(e) => json!({ "error": format!("{e:#}") }),
    };
    Ok(json!({
        "session": session.to_string(),
        "title": s.get::<String, _>("title"),
        "agent": agent,
        "model": s.get::<String, _>("model"),
        "outcome": s.get::<Option<String>, _>("outcome"),
        "note": s.get::<Option<String>, _>("outcome_note"),
        "rating": s.get::<Option<i64>, _>("rating"),
        "duration_secs": s.get::<Option<i64>, _>("secs"),
        "turns": t.get::<i64, _>("turns"),
        "owner_messages": owner_messages,
        "steers": steers,
        "aborts": t.get::<i64, _>("aborts"),
        "tool_calls": t.get::<i64, _>("tool_calls"),
        "tool_errors": t.get::<i64, _>("tool_errors"),
        "cost_usd": (t.get::<f64, _>("cost") * 10000.0).round() / 10000.0,
        "input_tokens": t.get::<i64, _>("input_tokens"),
        "output_tokens": t.get::<i64, _>("output_tokens"),
        "models": models,
        "children": children,
        "skills": skills,
        "verifications": verifications,
        "roles": roles,
    }))
}

/// The agent's latest reviews (at most 15), newest first.
async fn past_reviews(db: &SqlitePool, agent: &str) -> Result<Vec<Value>> {
    let rows = sqlx::query("SELECT kind_of_work, outcome, intervention, model_verdict, created_at FROM session_reviews WHERE agent = $1 ORDER BY id DESC LIMIT 15")
        .bind(agent)
        .fetch_all(db)
        .await?;
    Ok(rows
        .iter()
        .map(|r| json!({ "kind_of_work": r.get::<String, _>("kind_of_work"), "outcome": r.get::<String, _>("outcome"), "intervention": r.get::<String, _>("intervention"), "model_verdict": r.get::<String, _>("model_verdict"), "at": r.get::<String, _>("created_at") }))
        .collect())
}

/// The review's prompt: the template with the facts and the past reviews.
fn render(facts: &Value, past: &[Value]) -> String {
    let past = if past.is_empty() { "(none yet)".to_string() } else { past.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n") };
    let template = crate::agents::dir(facts["agent"].as_str().unwrap_or("zenbot")).join("review.md");
    let template = std::fs::read_to_string(template).unwrap_or_else(|_| PROMPT.to_string());
    template
        .replace("{session}", facts["session"].as_str().unwrap_or(""))
        .replace("{facts}", &serde_json::to_string_pretty(facts).unwrap_or_default())
        .replace("{past}", &past)
}

/// What the owner is told: each change with its file and why, the suggestions, and how to revert.
/// None when the review changed nothing and suggests nothing (it stays silent).
pub fn report(facts: &Value, changes: &Value, suggestions: &Value) -> Option<String> {
    let changes = changes.as_array().cloned().unwrap_or_default();
    let suggestions = suggestions.as_array().cloned().unwrap_or_default();
    if changes.is_empty() && suggestions.is_empty() {
        return None;
    }
    let mut text = format!("Review of \"{}\" ({}):\n", facts["title"].as_str().unwrap_or(""), facts["outcome"].as_str().unwrap_or(""));
    let line = |c: &Value| {
        let s = |k: &str| c[k].as_str().map(str::to_string).unwrap_or_else(|| if c.is_string() { c.as_str().unwrap_or("").to_string() } else { String::new() });
        format!("- {}: {} ({})\n", s("file"), s("what"), s("why"))
    };
    for c in &changes {
        text.push_str(&line(c));
    }
    if !suggestions.is_empty() {
        text.push_str("Suggestions for you:\n");
        for c in &suggestions {
            text.push_str(&line(c));
        }
    }
    if !changes.is_empty() {
        text.push_str("To revert: prompt files have a backup in ~/.zenbot/backups/prompt-files/; skills are commits in their folder (git revert); memory with `remember` remove.\n");
    }
    Some(text)
}

pub fn submit_spec() -> Value {
    json!({ "name": "submit_review", "description": "Record your review of the archived session, once, then end your turn.",
        "parameters": { "type": "object", "properties": {
            "kind_of_work": { "type": "string", "description": "A short label for the kind of work; reuse an earlier label when it fits" },
            "intervention": { "type": "string", "enum": ["none", "low", "medium", "high"] },
            "model_verdict": { "type": "string", "enum": ["right", "too-weak", "more-than-needed"] },
            "summary": { "type": "string", "description": "How the work went, in a few sentences" },
            "evidence": { "type": "string", "description": "The message numbers and facts behind it" },
            "changes": { "type": "array", "items": { "type": "object", "properties": { "file": { "type": "string" }, "what": { "type": "string" }, "why": { "type": "string" } } }, "description": "What you changed, one entry per change (empty when nothing)" },
            "suggestions": { "type": "array", "items": { "type": "object", "properties": { "file": { "type": "string" }, "what": { "type": "string" }, "why": { "type": "string" } } }, "description": "Changes for the owner to make (AGENTS.md, SOUL.md, anything you may not change)" } },
          "required": ["kind_of_work", "intervention", "model_verdict", "summary"] } })
}

/// The `submit_review` tool: one row in `session_reviews`.
pub async fn submit(app: &AppState, review: Uuid, args: &Value) -> Result<String> {
    let s = |k: &str| args[k].as_str().map(str::trim).unwrap_or("").to_string();
    let (kind, intervention, verdict, summary) = (s("kind_of_work"), s("intervention"), s("model_verdict"), s("summary"));
    anyhow::ensure!(!kind.is_empty() && !summary.is_empty(), "give `kind_of_work` and `summary`");
    anyhow::ensure!(matches!(intervention.as_str(), "none" | "low" | "medium" | "high"), "`intervention` is none, low, medium or high");
    anyhow::ensure!(matches!(verdict.as_str(), "right" | "too-weak" | "more-than-needed"), "`model_verdict` is right, too-weak or more-than-needed");
    let list = |k: &str| if args[k].is_array() { args[k].to_string() } else { "[]".into() };
    sqlx::query(
        "INSERT INTO session_reviews (session_id, review_session_id, agent, outcome, kind_of_work, intervention, model_verdict, summary, evidence, changes, suggestions)
         SELECT parent, id, COALESCE((SELECT agent FROM sessions p WHERE p.id = r.parent), 'zenbot'), (SELECT outcome FROM sessions p WHERE p.id = r.parent), $2, $3, $4, $5, $6, $7, $8
         FROM sessions r WHERE id = $1 AND kind = 'review'",
    )
    .bind(review)
    .bind(&kind)
    .bind(&intervention)
    .bind(&verdict)
    .bind(&summary)
    .bind(s("evidence"))
    .bind(list("changes"))
    .bind(list("suggestions"))
    .execute(&app.db)
    .await?;
    Ok("Review recorded. End your turn now.".into())
}

/// Why a review session may not write this file, if it may not: it may change only the agent's
/// IDENTITY.md, USER.md (zenbot only) and its own skill files. Model changes are proposals.
fn has_symlink(path: &Path) -> bool {
    path.ancestors().any(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
}

pub fn write_refusal(home: &Path, agent: &str, skill_roots: &[PathBuf], workspace: &Path, name: &str, args: &Value) -> Option<String> {
    if name == "save_skill" {
        let root = skill_roots.first()?;
        let dir = root.join(args["domain"].as_str().unwrap_or("")).join(args["name"].as_str().unwrap_or(""));
        let mut paths = vec![dir.join("SKILL.md")];
        if let Some(files) = args["files"].as_object() {
            paths.extend(files.keys().map(|p| dir.join(p)));
        }
        if paths.iter().any(|p| has_symlink(p) || !crate::agent::real(p).starts_with(root)) {
            return Some("A review may save skills only inside its own skill home (no escaping symlinks).".into());
        }
        return None;
    }
    if !matches!(name, "edit" | "write") {
        return None;
    }
    let path = tools::resolve(workspace, args["path"].as_str().unwrap_or(""));
    if has_symlink(&path) {
        return Some("Review writes must not traverse symlinks; propose the change instead.".into());
    }
    let target = crate::agent::real(&path);
    let dir = home.join("agents").join(agent);
    let mut allowed = vec![dir.join("IDENTITY.md")];
    if agent == "zenbot" {
        allowed.push(home.join("USER.md"));
    }
    if allowed.contains(&target) || skill_roots.iter().any(|r| target.starts_with(r)) {
        return None;
    }
    Some(format!(
        "A review may change only {}'s IDENTITY.md{}, skills (prefer save_skill); roles.toml changes must be suggestions backed by at least two reviews; memory through `remember`. Put anything else (AGENTS.md, SOUL.md, …) in `suggestions`.",
        agent,
        if agent == "zenbot" { ", USER.md" } else { "" }
    ))
}

#[derive(serde::Deserialize)]
pub struct ListQuery {
    agent: Option<String>,
    limit: Option<i64>,
}

/// GET /api/reviews: the latest reviews, newest first, optionally of one agent.
pub async fn list(axum::extract::State(app): axum::extract::State<AppState>, axum::extract::Query(q): axum::extract::Query<ListQuery>) -> Result<axum::Json<Value>, crate::api::ApiError> {
    let rows = sqlx::query(
        "SELECT id, session_id, review_session_id, agent, outcome, kind_of_work, intervention, model_verdict, summary, evidence, changes, suggestions, created_at
         FROM session_reviews WHERE $1 IS NULL OR agent = $1 ORDER BY id DESC LIMIT $2",
    )
    .bind(&q.agent)
    .bind(q.limit.unwrap_or(50).clamp(1, 500))
    .fetch_all(&app.db)
    .await
    .map_err(anyhow::Error::from)?;
    let json_of = |t: String| serde_json::from_str::<Value>(&t).unwrap_or(json!([]));
    Ok(axum::Json(Value::Array(
        rows.into_iter()
            .map(|r| json!({
                "id": r.get::<i64, _>("id"), "session_id": r.get::<Uuid, _>("session_id"), "review_session_id": r.get::<Option<Uuid>, _>("review_session_id"),
                "agent": r.get::<String, _>("agent"), "outcome": r.get::<String, _>("outcome"), "kind_of_work": r.get::<String, _>("kind_of_work"),
                "intervention": r.get::<String, _>("intervention"), "model_verdict": r.get::<String, _>("model_verdict"), "summary": r.get::<String, _>("summary"),
                "evidence": r.get::<String, _>("evidence"), "changes": json_of(r.get("changes")), "suggestions": json_of(r.get("suggestions")), "created_at": r.get::<String, _>("created_at"),
            }))
            .collect(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_review_writes_only_its_agents_notes_roles_and_skills() {
        let dir = crate::test_util::TestDir::new("review-guard");
        let home = dir.to_path_buf();
        std::fs::create_dir_all(home.join("agents/zenbot")).unwrap();
        std::fs::create_dir_all(home.join("agents/garry")).unwrap();
        std::fs::create_dir_all(home.join("global/skills")).unwrap();
        let skills = vec![home.join("global/skills")];
        let w = |agent: &str, p: PathBuf| write_refusal(&home, agent, &skills, &home, "write", &json!({ "path": p.display().to_string() }));
        assert!(w("zenbot", home.join("agents/zenbot/IDENTITY.md")).is_none());
        assert!(w("zenbot", home.join("USER.md")).is_none());
        assert!(w("zenbot", home.join("agents/zenbot/roles.toml")).is_some());
        assert!(w("zenbot", home.join("global/skills/work/x/SKILL.md")).is_none());
        assert!(w("zenbot", home.join("agents/zenbot/SOUL.md")).is_some());
        assert!(w("zenbot", home.join("AGENTS.md")).is_some());
        assert!(w("zenbot", home.join("agents/zenbot/../garry/IDENTITY.md")).is_some());
        assert!(w("garry", home.join("USER.md")).is_some(), "USER.md is zenbot's");
        assert!(w("zenbot", PathBuf::from("/etc/passwd")).is_some());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(home.join("agents/zenbot/SOUL.md"), home.join("agents/zenbot/IDENTITY.md")).unwrap();
            assert!(w("zenbot", home.join("agents/zenbot/IDENTITY.md")).is_some(), "dangling links are refused too");
            std::fs::write(home.join("agents/zenbot/SOUL.md"), "owner").unwrap();
            assert!(w("zenbot", home.join("agents/zenbot/IDENTITY.md")).is_some());
            std::os::unix::fs::symlink(home.join("agents"), home.join("global/skills/escape")).unwrap();
            assert!(w("zenbot", home.join("global/skills/escape/zenbot/SOUL.md")).is_some());
            assert!(write_refusal(&home, "zenbot", &skills, &home, "save_skill", &json!({"domain":"escape", "name":"zenbot"})).is_some());
        }
        assert!(write_refusal(&home, "zenbot", &skills, &home, "read", &json!({ "path": "/etc/passwd" })).is_none());
    }

    #[test]
    fn the_report_lists_changes_and_stays_silent_without_any() {
        let f = json!({ "title": "t", "outcome": "done" });
        assert!(report(&f, &json!([]), &json!([])).is_none());
        let r = report(&f, &json!([{ "file": "IDENTITY.md", "what": "a note", "why": "corrected twice" }]), &json!([])).unwrap();
        assert!(r.contains("IDENTITY.md: a note (corrected twice)") && r.contains("backups/prompt-files"));
    }

    #[tokio::test]
    async fn facts_are_counted_by_the_kernel() {
        let (_db_dir, db) = crate::test_util::database().await;
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, title, model, outcome, outcome_note, archived) VALUES ($1, 'job', 'faux/smoke', 'done', 'ok', true)").bind(id).execute(&db).await.unwrap();
        let child = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, model, parent, kind, role) VALUES ($1, 'claude/haiku', $2, 'subagent', 'scout')").bind(child).bind(id).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO brain_skill_uses (session_id, name, digest) VALUES ($1, 'work/verify', 'x')").bind(id).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO tool_calls (session_id, call_id, name, args, is_error, duration_ms, output_bytes) VALUES ($1, 'c', 'load_skill', '{\"name\":\"writing/plain\"}', 0, 1, 1)").bind(id).execute(&db).await.unwrap();
        crate::tape::append(&db, id, "message", &json!({"role":"user", "kernel":true, "content":"system"})).await.unwrap();
        let queued: i64 = sqlx::query_scalar("INSERT INTO pending_prompts(session_id,text,steer) VALUES ($1,'fix',1) RETURNING id").bind(id).fetch_one(&db).await.unwrap();
        crate::tape::append_owner_queued(&db, id, &json!({"role":"user", "content":"fix"}), Some(queued)).await.unwrap();
        let f = facts(&db, id).await.unwrap();
        assert_eq!(f["owner_messages"], 1);
        assert_eq!(f["steers"], 1);
        deliver(&db, id, "A report").await.unwrap();
        deliver(&db, id, "A report").await.unwrap();
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM job_runs WHERE session_id=$1").bind(id).fetch_one(&db).await.unwrap(), 1);
        let runs = crate::jobs::runs(&db, None, 10).await.unwrap();
        assert_eq!(runs[0]["output"], "A report");
        assert_eq!(runs[0]["status"], "ok");
        assert_eq!(f["outcome"], "done");
        assert_eq!(f["note"], "ok");
        assert_eq!(f["skills"], json!(["work/verify", "writing/plain"]));
        assert_eq!(f["children"][0]["role"], "scout");
        assert_eq!(f["children"][0]["model"], "claude/haiku");
        assert_eq!(f["turns"], 0);
    }
}
