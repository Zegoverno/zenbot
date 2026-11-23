//! Scheduled jobs. One scheduler in the kernel, backed by the `jobs` and `job_runs` tables, runs
//! three kinds of job:
//!
//! - **system** jobs, the kernel's own maintenance, seeded at start (an install from before a job
//!   existed gets it at its next start): `sleep` (the nightly memory sleep, `memory::sleep`),
//!   `wiki` (index and lint brain records: `brain::tend`), `skills` (flag and archive unused
//!   skills: `workshop::tend`), `prune` (saved outputs bounded by age and size, prompt-file
//!   backups, database dumps, eval runs and engine sessions: `prune.rs`) and `engines`
//!   (`scripts/update-engines.sh`). Each records its own run; a run with nothing to report is
//!   `silent`. What `wiki`, `skills` and `prune` found in the last day reaches the next
//!   sessions with the sleep's morning note (`upkeep_note`). The owner can pause and run them;
//!   nobody can remove them.
//! - **agent** jobs: a prompt run in a fresh session (kind `job`) with the instructions the job
//!   picks (`context`: the soul always, plus any of identity, agents, user, memory, skills,
//!   project) and the skills it names loaded. The session can't ask, delegate or schedule; its final
//!   answer is the report, `[SILENT]` when there's nothing worth the owner's attention; however the
//!   run ends, its session is archived. A job with a `report_file` reports that file instead, as
//!   the run left it, when the run ends `ok`: inside its workspace, not a symlink, hardlink or
//!   secret file, written during the run, UTF-8, at most 60 KB (`report.rs`); anything else makes
//!   the run an error, never the answer posted instead.
//! - **script** jobs: a shell command run with no model, in a fresh `job` session; its masked
//!   output (or its `report_file`) is the report.
//!
//! How it runs (patterns from Hermes and OpenClaw, read in code; schedules in `schedule.rs`): the
//! loop sleeps until the earliest due job, at most a minute; due jobs are claimed in a short
//! immediate write transaction and their next run is set in the same transaction, so a crash
//! loses at most one run and never repeats one; the database allows one running run per job; runs
//! left running by a restart are marked `interrupted`. A run missed while the kernel was down runs once if it's within its grace (half
//! its period, 2 minutes to 2 hours), else it's recorded as missed. A failing agent job is spaced
//! out (never run more often than scheduled) and paused after 5 failures in a row.
//!
//! Who creates agent and script jobs: the owner (API, `zen jobs`) and the agent (`schedule`); a
//! job goes live when it's created. The next run of an agent job gets the last report as
//! `<last_report>`. ZEN_JOBS=0 turns the scheduler off (test and eval kernels).

mod prune;
mod report;
mod schedule;

use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use sqlx::{SqlitePool, Row};
use uuid::Uuid;

use crate::{tape, tools, AppState};
use report::{checked_report_file, file_report, real_path};
pub use schedule::parse_tz;
use schedule::{checked_schedule, grace, next_after, next_from, parse_schedule, period};

/// The kind of a session a scheduled agent job runs in.
pub const JOB: &str = "job";
/// Instructions every agent job gets unless it says otherwise (the soul is always in).
const DEFAULT_CONTEXT: [&str; 3] = ["soul", "user", "memory"];
/// Agent jobs paused after this many failed runs in a row.
const MAX_FAILURES: i32 = 5;
/// How long a failing agent job waits at least before its next run, by failures in a row.
const BACKOFF_SECS: [i64; 4] = [60, 300, 900, 3600];

/// The kernel's own jobs: (name, action, schedule in UTC, what it does). The nightly ones run ten
/// minutes apart after the sleep, so each has the machine to itself and its own record.
const SYSTEM: [(&str, &str, &str, &str); 5] = [
    ("sleep", "sleep", "0 3 * * *", "tidy short-term memory: promote, rank, archive (memory::sleep)"),
    ("wiki", "wiki", "10 3 * * *", "index and lint brain records (brain::tend)"),
    ("skills", "skills", "20 3 * * *", "flag skills unused for 30 days, archive them at 90 (workshop::tend)"),
    ("prune", "prune", "40 3 * * *", "remove saved tool outputs older than 30 days, prompt-file backups older than 90, database dumps but the newest 3 and eval runs older than 7 days (jobs::prune_files)"),
    ("engines", "engines", "0 4 * * *", "update the Claude Code and Codex CLIs, tested, with rollback (scripts/update-engines.sh)"),
];

/// Wakes the loop when jobs change, so a new job's first run isn't up to a minute late.
static WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

// ---------- jobs ----------

#[derive(Debug, Clone)]
pub struct Job {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub action: Option<String>,
    pub prompt: Option<String>,
    /// A script job's shell command: run with no model.
    pub command: Option<String>,
    pub context: Vec<String>,
    pub skills: Vec<String>,
    pub schedule: String,
    pub tz: String,
    pub model: Option<String>,
    pub workspace: Option<String>,
    /// The file whose text is the report of a run that ends `ok`, inside the workspace.
    pub report_file: Option<String>,
    /// The session a run's report is posted into, instead of only the jobs feed.
    pub post_to: Option<Uuid>,
    pub enabled: bool,
    pub paused_reason: Option<String>,
    pub created_by: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub failures: i32,
    /// The agent an agent job runs as (agents.rs): the one that scheduled it, or the owner's pick.
    pub agent: String,
    /// Explicit plugin provenance, not an execution permission or job kind.
    pub plugin: Option<String>,
}

const COLUMNS: &str = "id, name, kind, action, prompt, command, context, skills, schedule, tz, model, workspace, report_file, post_to, enabled, paused_reason, created_by, \
next_run_at, last_run_at, last_status, last_error, failures, agent, plugin";

fn job_of(r: &sqlx::sqlite::SqliteRow) -> Job {
    Job {
        id: r.get("id"),
        name: r.get("name"),
        kind: r.get("kind"),
        action: r.get("action"),
        prompt: r.get("prompt"),
        command: r.get("command"),
        context: r.get::<sqlx::types::Json<Vec<String>>,_>("context").0,
        skills: r.get::<sqlx::types::Json<Vec<String>>,_>("skills").0,
        schedule: r.get("schedule"),
        tz: r.get("tz"),
        model: r.get("model"),
        workspace: r.get("workspace"),
        report_file: r.get("report_file"),
        post_to: r.get("post_to"),
        enabled: r.get("enabled"),
        paused_reason: r.get("paused_reason"),
        created_by: r.get("created_by"),
        next_run_at: r.get("next_run_at"),
        last_run_at: r.get("last_run_at"),
        last_status: r.get("last_status"),
        last_error: r.get("last_error"),
        failures: r.get("failures"),
        agent: r.get("agent"),
        plugin: r.get("plugin"),
    }
}

pub fn job_json(j: &Job) -> Value {
    json!({
        "name": j.name, "kind": j.kind, "action": j.action, "prompt": j.prompt, "command": j.command, "context": j.context, "skills": j.skills,
        "schedule": j.schedule, "tz": j.tz, "model": j.model, "workspace": j.workspace, "report_file": j.report_file, "post_to": j.post_to, "enabled": j.enabled,
        "paused_reason": j.paused_reason, "created_by": j.created_by, "next_run_at": j.next_run_at, "last_run_at": j.last_run_at,
        "last_status": j.last_status, "last_error": j.last_error, "failures": j.failures, "agent": j.agent, "plugin": j.plugin,
    })
}

pub async fn get(db: &SqlitePool, name: &str) -> Result<Job> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM jobs WHERE name = $1 AND removed_at IS NULL"))
        .bind(name.trim())
        .fetch_optional(db)
        .await?
        .with_context(|| format!("no job `{}`; list them with `zen jobs`", name.trim()))?;
    Ok(job_of(&row))
}

pub async fn list(db: &SqlitePool) -> Result<Vec<Job>> {
    let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM jobs WHERE removed_at IS NULL ORDER BY kind DESC, name")).fetch_all(db).await?;
    Ok(rows.iter().map(job_of).collect())
}

/// Recent runs, newest first: of one job, or of all.
pub async fn runs(db: &SqlitePool, name: Option<&str>, limit: i64) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT r.id, j.name, r.trigger, r.status, r.session_id, r.started_at, r.ended_at, r.output, r.error, r.posted_to, r.pending_to, r.command, r.exit_code
         FROM job_runs r JOIN jobs j ON j.id = r.job_id
         WHERE ($1 IS NULL OR (j.name = $1 AND j.removed_at IS NULL)) ORDER BY r.started_at DESC, r.id DESC LIMIT $2",
    )
    .bind(name.map(str::trim))
    .bind(limit.clamp(1, 200))
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<i64, _>("id"), "job": r.get::<String, _>("name"), "trigger": r.get::<String, _>("trigger"),
                "status": r.get::<String, _>("status"), "session": r.get::<Option<Uuid>, _>("session_id"),
                "started_at": r.get::<DateTime<Utc>, _>("started_at"), "ended_at": r.get::<Option<DateTime<Utc>>, _>("ended_at"),
                "output": r.get::<Option<String>, _>("output"), "error": r.get::<Option<String>, _>("error"),
                "posted_to": r.get::<Option<Uuid>, _>("posted_to"),
                "pending_to": r.get::<Option<Uuid>, _>("pending_to"),
                "command": r.get::<Option<String>, _>("command"), "exit_code": r.get::<Option<i32>, _>("exit_code"),
            })
        })
        .collect())
}

/// What a new or changed agent job asks for (from the tool, the API or `zen jobs add`).
#[derive(Debug, Default, Clone)]
pub struct Spec {
    pub name: Option<String>,
    pub prompt: Option<String>,
    /// A script job's command instead of a prompt.
    pub command: Option<String>,
    pub schedule: Option<String>,
    pub tz: Option<String>,
    pub context: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub model: Option<String>,
    pub workspace: Option<String>,
    /// `Some("")` clears it.
    pub report_file: Option<String>,
    /// A session id (or 8+ character prefix), or `this` (the agent's session); `Some("")` clears it.
    pub post_to: Option<String>,
    /// The agent an owner's new job runs as (zenbot when missing). A job the agent schedules runs
    /// as that agent, always.
    pub agent: Option<String>,
    /// The plugin this routine came from; empty on update clears it.
    pub plugin: Option<String>,
}

fn strings(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Array(a) => Some(a.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()),
        Value::String(s) => Some(s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()),
        _ => None,
    }
}

impl Spec {
    pub fn from_json(v: &Value) -> Spec {
        let s = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        Spec {
            name: s("name"),
            prompt: s("prompt"),
            command: s("command"),
            schedule: s("schedule"),
            tz: s("tz"),
            context: strings(&v["context"]),
            skills: strings(&v["skills"]),
            model: s("model"),
            workspace: s("workspace").or_else(|| s("dir")),
            report_file: v["report_file"].as_str().map(|f| f.trim().to_string()),
            post_to: v["post_to"].as_str().map(|f| f.trim().to_string()),
            agent: s("agent"),
            plugin: v["plugin"].as_str().map(|p| p.trim().to_string()),
        }
    }
}

fn checked_plugin(plugin: Option<&str>) -> Result<Option<&str>> {
    let plugin = plugin.filter(|p| !p.is_empty());
    anyhow::ensure!(plugin.is_none_or(crate::plugins::valid_name), "plugin must be a plugin name (2–40 lowercase letters, digits or hyphens)");
    Ok(plugin)
}

fn valid_name(name: &str) -> Result<()> {
    anyhow::ensure!(crate::valid_name(name, 40), "a job name is lowercase letters, digits and single hyphens, up to 40 characters (e.g. `morning-brief`)");
    Ok(())
}

/// The context parts of a job, checked; the soul always first.
fn context_of(parts: Option<Vec<String>>) -> Result<Vec<String>> {
    let parts = parts.unwrap_or_else(|| DEFAULT_CONTEXT.iter().map(|s| s.to_string()).collect());
    let mut out = vec!["soul".to_string()];
    for p in parts {
        let p = p.to_lowercase();
        anyhow::ensure!(crate::compile::PARTS.contains(&p.as_str()), "unknown context part `{p}`: use {}", crate::compile::PARTS.join(", "));
        if !out.contains(&p) {
            out.push(p);
        }
    }
    Ok(out)
}

fn check_skills(skills: &[String], who: &crate::agents::Agent) -> Result<()> {
    let all = crate::skills::scan_roots(&who.skill_roots());
    for s in skills {
        crate::skills::lookup(&all, s).map_err(|e| anyhow::anyhow!(e))?;
    }
    Ok(())
}

/// Who is creating or changing a job, and whether it may go live.
#[derive(Clone, Copy)]
pub enum By {
    Owner,
    /// The agent, in this session; whether it may go live is decided by the gate.
    Agent(Uuid),
}

/// The session a job posts its reports into: one of the owner's own sessions (not a job's,
/// a subagent's or a verifier's), named by its id, an 8+ character prefix of it, or `this` (the
/// agent's session). Empty clears it (None).
async fn post_target(db: &SqlitePool, text: &str, by: &By) -> Result<Option<Uuid>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let prefix = match (text, by) {
        ("this" | "here", By::Agent(s)) => s.to_string(),
        ("this" | "here", By::Owner) => bail!("`post_to: this` names the agent's session; give the owner's session id instead"),
        _ => {
            anyhow::ensure!(
                text.len() >= 8 && text.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
                "`post_to` is a session id (or its first 8+ characters), or `this`"
            );
            text.to_ascii_lowercase()
        }
    };
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM sessions WHERE lower(hex(id)) LIKE replace($1,'-','') || '%' AND kind IS NULL LIMIT 2")
        .bind(&prefix)
        .fetch_all(db)
        .await?;
    match ids.as_slice() {
        [id] => Ok(Some(*id)),
        [] => bail!("no session of the owner's matches `{text}` (a job posts only into the owner's own sessions)"),
        _ => bail!("`{text}` matches more than one session; give more of the id"),
    }
}

/// The agent of the session behind `by` (None for the owner).
async fn agent_of(app: &AppState, by: &By) -> Result<Option<crate::agents::Agent>> {
    let By::Agent(s) = by else { return Ok(None) };
    let name: String = sqlx::query_scalar("SELECT agent FROM sessions WHERE id = $1").bind(s).fetch_optional(&app.db).await?.unwrap_or_else(|| crate::agents::DEFAULT.into());
    Ok(Some(crate::agents::load(&name)?))
}

/// Whether `by` may change, run, pause or remove `job`: the owner and zenbot any job, another
/// agent only its own (else it could run its prompt as a wider agent).
async fn ensure_own(app: &AppState, job: &Job, by: &By) -> Result<()> {
    if let Some(who) = agent_of(app, by).await? {
        anyhow::ensure!(who.name == crate::agents::DEFAULT || job.agent == who.name, "`{}` is {}'s job, not yours ({})", job.name, job.agent, who.name);
    }
    Ok(())
}

/// Whether `by` may give a job a shell command: a script job runs it with no model, so an agent
/// needs `bash` itself.
async fn ensure_shell(app: &AppState, by: &By) -> Result<()> {
    if let Some(who) = agent_of(app, by).await? {
        anyhow::ensure!(who.allows("bash"), "a script job runs a shell command, and `bash` isn't one of {}'s tools", who.name);
    }
    Ok(())
}

/// Create an agent job. Returns the job and, for the agent, why it's paused if it is.
pub async fn create(app: &AppState, spec: Spec, by: By) -> Result<(Job, Option<String>)> {
    let name = spec.name.clone().context("a job needs a `name`")?;
    valid_name(&name)?;
    let command = spec.command.clone();
    let kind = if command.is_some() { "script" } else { "agent" };
    checked_command(command.as_deref())?;
    if command.is_some() {
        ensure_shell(app, &by).await?;
    }
    let prompt = match &command {
        Some(_) => spec.prompt.clone().unwrap_or_default(),
        None => spec.prompt.clone().context("a job needs a `prompt` (the task, self-contained: it sees nothing of this conversation) or a `command`")?,
    };
    let tz_text = spec.tz.clone().unwrap_or_else(|| crate::config::get().tz.clone());
    let tz = parse_tz(&tz_text)?;
    let (schedule, next) = checked_schedule(spec.schedule.as_deref().context("a job needs a `schedule`")?, tz, Utc::now())?;
    let context = context_of(spec.context.clone())?;
    // An agent's job is its own: it can't schedule work as another agent (one with more tools).
    let agent_name = match &by {
        By::Owner => spec.agent.clone().unwrap_or_else(|| crate::agents::DEFAULT.into()),
        By::Agent(s) => {
            let own: String = sqlx::query_scalar("SELECT agent FROM sessions WHERE id = $1").bind(s).fetch_optional(&app.db).await?.unwrap_or_else(|| crate::agents::DEFAULT.into());
            anyhow::ensure!(spec.agent.as_deref().is_none_or(|a| a == own), "a job you schedule runs as you ({own})");
            own
        }
    };
    let who = crate::agents::load(&agent_name)?;
    let skills = spec.skills.clone().unwrap_or_default();
    check_skills(&skills, &who)?;
    let workspace = workspace_of(app, spec.workspace.as_deref());
    let report_file = report_file_in(&workspace, spec.report_file.as_deref())?;
    let post_to = match spec.post_to.as_deref() {
        Some(t) => post_target(&app.db, t, &by).await?,
        None => None,
    };
    let (created_by, session, model) = match &by {
        By::Owner => ("owner", None, spec.model.clone()),
        // The agent can't point unattended spending at another model: the default it is.
        By::Agent(s) => ("agent", Some(*s), None),
    };
    if let Some(m) = &model {
        if !app.routes.lock().await.contains_key(m) {
            // The workers' model lists may not have been read yet (a kernel that just started).
            crate::collect_models(app).await;
        }
        anyhow::ensure!(app.routes.lock().await.contains_key(m), "no worker serves model `{m}`");
    }
    let row = sqlx::query(&format!(
        "INSERT INTO jobs (name, kind, prompt, context, skills, schedule, tz, model, workspace, enabled, created_by, created_in, next_run_at, report_file, post_to, command, agent, plugin)
         VALUES ($1, $14, $2, $3, $4, $5, $6, $7, $8, true, $9, $10, $11, $12, $13, $15, $16, $17) RETURNING {COLUMNS}"
    ))
    .bind(&name)
    .bind(&prompt)
    .bind(sqlx::types::Json(&context))
    .bind(sqlx::types::Json(&skills))
    .bind(&schedule)
    .bind(tz.name())
    .bind(&model)
    .bind(&spec.workspace)
    .bind(created_by)
    .bind(session)
    .bind(next)
    .bind(&report_file)
    .bind(post_to)
    .bind(kind)
    .bind(&command)
    .bind(&who.name)
    .bind(checked_plugin(spec.plugin.as_deref())?)
    .fetch_one(&app.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => anyhow::anyhow!("a job named `{name}` already exists; change it with `update`"),
        _ => e.into(),
    })?;
    WAKE.notify_one();
    Ok((job_of(&row), None))
}

/// The longest command a script job takes.
const MAX_COMMAND: usize = 8 * 1024;

fn checked_command(command: Option<&str>) -> Result<()> {
    anyhow::ensure!(
        command.is_none_or(|c| c.len() <= MAX_COMMAND),
        "a script job's command is at most {MAX_COMMAND} bytes; put longer scripts in a file in the workspace"
    );
    Ok(())
}

/// A job's report file checked against the workspace it will have; none when empty.
fn report_file_in(workspace: &std::path::Path, file: Option<&str>) -> Result<Option<String>> {
    file.filter(|f| !f.is_empty()).map(|f| checked_report_file(workspace, f)).transpose()
}

/// Change an agent or script job.
pub async fn update(app: &AppState, name: &str, spec: Spec, by: By) -> Result<(Job, Option<String>)> {
    let job = get(&app.db, name).await?;
    anyhow::ensure!(job.kind != "system", "`{}` is a system job: only pause, resume and run apply", job.name);
    anyhow::ensure!(spec.command.is_none() || job.kind == "script", "`{}` runs a prompt; a script job is a new job with a `command`", job.name);
    ensure_own(app, &job, &by).await?;
    let plugin = checked_plugin(spec.plugin.as_deref().or(job.plugin.as_deref()))?;
    // Attribution alone must not recalculate the next run or reset failure backoff.
    if spec.plugin.is_some() && [spec.prompt.is_none(), spec.command.is_none(), spec.schedule.is_none(),
        spec.tz.is_none(), spec.context.is_none(), spec.skills.is_none(), spec.model.is_none(),
        spec.workspace.is_none(), spec.report_file.is_none(), spec.post_to.is_none(), spec.agent.is_none()].into_iter().all(|absent| absent) {
        let row = sqlx::query(&format!(
            "UPDATE jobs SET plugin = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1 RETURNING {COLUMNS}"
        )).bind(job.id).bind(plugin).fetch_one(&app.db).await?;
        return Ok((job_of(&row), job.paused_reason));
    }
    if spec.command.is_some() {
        ensure_shell(app, &by).await?;
    }
    let command = spec.command.clone().or(job.command.clone());
    checked_command(command.as_deref())?;
    let prompt = spec.prompt.clone().or(job.prompt.clone()).unwrap_or_default();
    let tz = parse_tz(spec.tz.as_deref().unwrap_or(&job.tz))?;
    let (schedule, next) = checked_schedule(spec.schedule.as_deref().unwrap_or(&job.schedule), tz, Utc::now())?;
    let context = match spec.context.clone() {
        Some(c) => context_of(Some(c))?,
        None => job.context.clone(),
    };
    let skills = spec.skills.clone().unwrap_or(job.skills.clone());
    anyhow::ensure!(spec.agent.as_deref().is_none_or(|a| a == job.agent), "a job's agent can't change; make a new job");
    check_skills(&skills, &crate::agents::load(&job.agent)?)?;
    // Checked again against the workspace it will have: a moved workspace can leave it outside.
    let workspace = workspace_of(app, spec.workspace.as_deref().or(job.workspace.as_deref()));
    let report_file = report_file_in(&workspace, spec.report_file.as_deref().or(job.report_file.as_deref()))?;
    let post_to = match spec.post_to.as_deref() {
        Some(t) => post_target(&app.db, t, &by).await?,
        None => job.post_to,
    };
    let model = match &by {
        By::Owner => spec.model.clone().or(job.model.clone()),
        By::Agent(_) => {
            anyhow::ensure!(spec.model.is_none(), "only the owner sets a job's model");
            job.model.clone()
        }
    };
    let paused = job.paused_reason.clone().filter(|_| !job.enabled);
    let row = sqlx::query(&format!(
        "UPDATE jobs SET prompt = $2, context = $3, skills = $4, schedule = $5, tz = $6, model = $7, workspace = COALESCE($8, workspace),
                next_run_at = $9, report_file = $10, post_to = $11, failures = 0, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), command = $12, plugin = CASE WHEN $13 THEN $14 ELSE plugin END
         WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(job.id)
    .bind(&prompt)
    .bind(sqlx::types::Json(&context))
    .bind(sqlx::types::Json(&skills))
    .bind(&schedule)
    .bind(tz.name())
    .bind(&model)
    .bind(&spec.workspace)
    .bind(next)
    .bind(&report_file)
    .bind(post_to)
    .bind(&command)
    .bind(spec.plugin.is_some())
    .bind(plugin)
    .fetch_one(&app.db)
    .await?;
    WAKE.notify_one();
    Ok((job_of(&row), paused))
}

/// Pause or resume a job. Only the owner pauses or resumes a system job.
pub async fn set_enabled(app: &AppState, name: &str, on: bool, by: By) -> Result<(Job, Option<String>)> {
    let job = get(&app.db, name).await?;
    if let By::Agent(_) = by {
        anyhow::ensure!(job.kind != "system", "only the owner pauses or resumes a system job");
    }
    ensure_own(app, &job, &by).await?;
    let reason = (!on).then_some(match by {
        By::Owner => "paused by the owner",
        By::Agent(_) => "paused by the agent",
    });
    // A job resumed after a while starts from now: its missed runs aren't made up.
    let next = if on {
        let tz = parse_tz(&job.tz)?;
        let (s, _) = parse_schedule(&job.schedule, tz, Utc::now())?;
        next_after(&s, tz, Utc::now())
    } else {
        job.next_run_at
    };
    let row = sqlx::query(&format!("UPDATE jobs SET enabled = $2, paused_reason = $3, next_run_at = $4, failures = CASE WHEN $2 THEN 0 ELSE failures END, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1 RETURNING {COLUMNS}"))
        .bind(job.id)
        .bind(on)
        .bind(reason)
        .bind(next)
        .fetch_one(&app.db)
        .await?;
    WAKE.notify_one();
    Ok((job_of(&row), None))
}

/// Remove an agent job (its runs stay).
pub async fn remove(db: &SqlitePool, name: &str) -> Result<()> {
    let job = get(db, name).await?;
    anyhow::ensure!(job.kind == "agent", "`{}` is a system job: pause it instead", job.name);
    sqlx::query("UPDATE jobs SET removed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), enabled = false, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1").bind(job.id).execute(db).await?;
    Ok(())
}

/// Run a job now, in the background. Returns the run's id. Refused when it's already running, or
/// for a paused agent job (paused by the owner, the agent or failures) unless the owner asks.
pub async fn run_now(app: &AppState, name: &str, by: By) -> Result<i64> {
    let job = get(&app.db, name).await?;
    ensure_own(app, &job, &by).await?;
    let trigger = match by {
        By::Owner => "owner",
        By::Agent(_) => {
            anyhow::ensure!(job.enabled || job.kind == "system", "`{}` is paused ({}); the owner can resume or run it", job.name, job.paused_reason.as_deref().unwrap_or("paused"));
            "agent"
        }
    };
    let run = start_run(&app.db, job.id, trigger).await?.with_context(|| format!("`{}` is already running", job.name))?;
    spawn_run(app, job, run);
    Ok(run)
}

// ---------- the scheduler ----------

/// Create the kernel's own jobs if they aren't there (an install from before a job existed gets it
/// here). One whose name an agent job already has isn't created: logged, so the owner can rename theirs.
pub async fn seed(db: &SqlitePool) -> Result<()> {
    for (name, action, schedule, _) in SYSTEM {
        let (s, canon) = parse_schedule(schedule, Tz::UTC, Utc::now())?;
        let next = next_after(&s, Tz::UTC, Utc::now());
        let made = sqlx::query(
            "INSERT INTO jobs (name, kind, action, schedule, tz, created_by, next_run_at)
             SELECT $1, 'system', $2, $3, 'UTC', 'kernel', $4
             WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE kind = 'system' AND action = $2 AND removed_at IS NULL)
             ON CONFLICT DO NOTHING",
        )
        .bind(name)
        .bind(action)
        .bind(&canon)
        .bind(next)
        .execute(db)
        .await?;
        if made.rows_affected() == 0 {
            let have: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = 'system' AND action = $1 AND removed_at IS NULL)").bind(action).fetch_one(db).await?;
            if !have {
                tracing::warn!("the system job `{name}` wasn't created: another job has that name (rename it and restart)");
            }
        }
    }
    Ok(())
}

/// Whether the scheduler runs in this kernel (ZEN_JOBS, on by default).
pub fn enabled() -> bool {
    crate::config::get().jobs
}

/// The scheduler: started once with the kernel.
pub async fn run_loop(app: AppState) {
    if let Err(e) = seed(&app.db).await {
        tracing::error!("seeding the system jobs: {e:#}");
    }
    if !enabled() {
        tracing::info!("scheduled jobs are off (ZEN_JOBS=0)");
        return;
    }
    // Runs this kernel didn't finish: the previous kernel stopped during them.
    let interrupted: Result<u64> = async {
        let mut tx=crate::storage::begin(&app.db).await?;
        sqlx::query("UPDATE sessions SET archived=true WHERE id IN (SELECT session_id FROM job_runs WHERE status='running')").execute(&mut *tx).await?;
        sqlx::query("UPDATE jobs SET last_status='interrupted',updated_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id IN (SELECT job_id FROM job_runs WHERE status='running')").execute(&mut *tx).await?;
        let count=sqlx::query("UPDATE job_runs SET status='interrupted',ended_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),error='the kernel stopped during the run' WHERE status='running'").execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(count)
    }.await;
    match interrupted {
        Ok(n) if n>0 => tracing::warn!("{n} job run(s) interrupted by a restart"),
        Ok(_) => {},
        Err(e) => tracing::error!("marking interrupted job runs: {e:#}"),
    }
    loop {
        if let Err(e) = tick(&app).await {
            tracing::error!("scheduler: {e:#}");
        }
        let wait = match sqlx::query_scalar::<_, Option<DateTime<Utc>>>("SELECT min(next_run_at) FROM jobs WHERE enabled AND removed_at IS NULL").fetch_one(&app.db).await {
            Ok(Some(t)) => (t - Utc::now()).to_std().unwrap_or(Duration::ZERO),
            _ => Duration::from_secs(60),
        };
        // At least once a minute (ZEN_JOBS_TICK, seconds; tests make it shorter), so a clock jump
        // or a change made straight in the database is seen.
        let most = Duration::from_secs_f64(crate::config::get().jobs_tick);
        let wait = wait.clamp(Duration::from_millis(200).min(most), most);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = WAKE.notified() => {}
        }
    }
}

/// Start the runs that are due.
async fn tick(app: &AppState) -> Result<()> {
    // Delivering reports never holds up the schedule: a failure is logged, a hang cut off.
    match tokio::time::timeout(DELIVERY_TIMEOUT, deliver_pending(app)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::error!("delivering pending job reports: {e:#}"),
        Err(_) => tracing::error!("delivering pending job reports took over {}s; the schedule goes on", DELIVERY_TIMEOUT.as_secs()),
    }
    let now = Utc::now();
    let mut tx = crate::storage::begin(&app.db).await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE enabled AND removed_at IS NULL AND next_run_at <= strftime('%Y-%m-%dT%H:%M:%f+00:00','now') ORDER BY next_run_at LIMIT 8"
    ))
    .fetch_all(&mut *tx)
    .await?;
    let mut start = Vec::new();
    for job in rows.iter().map(job_of) {
        let due = job.next_run_at.unwrap_or(now);
        let (sched, tz) = match parse_tz(&job.tz).and_then(|tz| parse_schedule(&job.schedule, tz, now).map(|(s, _)| (s, tz))) {
            Ok(x) => x,
            Err(e) => {
                // Never silently: the job stops with the reason shown.
                sqlx::query("UPDATE jobs SET enabled = false, paused_reason = $2, last_status = 'error', last_error = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1")
                    .bind(job.id)
                    .bind(format!("paused: its schedule can't be read ({e:#})"))
                    .execute(&mut *tx)
                    .await?;
                continue;
            }
        };
        let next = next_from(&sched, tz, due, now);
        let late = now - due;
        if late > grace(period(&sched, tz, due)) {
            sqlx::query("UPDATE jobs SET next_run_at = $2, last_status = 'missed', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1").bind(job.id).bind(next).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO job_runs (job_id, trigger, status, ended_at, error) VALUES ($1, 'schedule', 'missed', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), $2)")
                .bind(job.id)
                .bind(format!("due at {due}, {} minutes late (the kernel was down); skipped", late.num_minutes()))
                .execute(&mut *tx)
                .await?;
            continue;
        }
        sqlx::query("UPDATE jobs SET next_run_at = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1").bind(job.id).bind(next).execute(&mut *tx).await?;
        let trigger = if late > chrono::Duration::minutes(2) { "catchup" } else { "schedule" };
        // The database allows one running run per job: one still running (or started by the owner
        // just now) means this one is skipped, the next stays scheduled, and the tick goes on.
        if let Some(run) = start_run(&mut *tx, job.id, trigger).await? {
            start.push((job, run));
        }
    }
    tx.commit().await?;
    for (job, run) in start {
        spawn_run(app, job, run);
    }
    Ok(())
}

/// Archive a finished run's session (a system job's run has none): the report reaches the owner
/// through the jobs room and `zen jobs runs`, and the session stays in the board's Archived section.
async fn archive_session(db: &SqlitePool, run: i64) -> Result<()> {
    sqlx::query("UPDATE sessions SET archived = true WHERE id = (SELECT session_id FROM job_runs WHERE id = $1)").bind(run).execute(db).await?;
    Ok(())
}

/// A `running` row for a run of this job, unless one is running (None).
async fn start_run<'e>(db: impl sqlx::SqliteExecutor<'e>, job: i64, trigger: &str) -> Result<Option<i64>> {
    let run = sqlx::query_scalar("INSERT INTO job_runs (job_id, trigger, status) VALUES ($1, $2, 'running') ON CONFLICT DO NOTHING RETURNING id")
        .bind(job)
        .bind(trigger)
        .fetch_optional(db)
        .await?;
    Ok(run)
}

/// How a run ended.
struct Outcome {
    status: &'static str, // ok | silent | error
    output: Option<String>,
    error: Option<String>,
}

impl Outcome {
    fn ok(report: String) -> Outcome {
        Outcome { status: "ok", output: Some(report), error: None }
    }

    /// Nothing worth the owner's attention.
    fn silent() -> Outcome {
        Outcome { status: "silent", output: None, error: None }
    }

    /// A failed run, with what it printed or answered, if anything.
    fn error(error: impl Into<String>, output: Option<String>) -> Outcome {
        Outcome { status: "error", output, error: Some(error.into()) }
    }
}

fn spawn_run(app: &AppState, job: Job, run: i64) {
    let app = app.clone();
    tokio::spawn(async move {
        // An upgrade waits for a running job.
        let _busy = app.background_work();
        tracing::info!("job `{}` run {run} started", job.name);
        let outcome = match job.kind.as_str() {
            "system" => run_system(&app, &job).await,
            "script" => run_script(&app, &job, run).await,
            _ => run_agent(&app, &job, run).await,
        };
        let outcome = outcome.unwrap_or_else(|e| Outcome::error(format!("{e:#}"), None));
        match finish(&app.db, &job, run, &outcome).await {
            Ok(Some(_)) => {
                if let Err(e) = deliver_pending(&app).await {
                    tracing::error!("delivering pending job reports: {e:#}");
                }
                WAKE.notify_one();
            }
            Ok(None) => {}
            Err(e) => tracing::error!("recording job `{}` run {run}: {e:#}", job.name),
        }
        tracing::info!("job `{}` run {run}: {}", job.name, outcome.status);
    });
}

/// Record how a run ended; space out or pause a failing agent job. Its session, however it ended,
/// is archived.
///
/// Returns the session the report is to be posted into: the job's `post_to`, for an `ok`
/// run with a report. The intent is recorded (`pending_to`) together with the status, so the Matrix
/// bridge never posts a queued report in the jobs room too; `deliver_pending` records delivery.
async fn finish(db: &SqlitePool, job: &Job, run: i64, o: &Outcome) -> Result<Option<Uuid>> {
    archive_session(db, run).await?;
    let has_report = o.status == "ok" && o.output.as_deref().is_some_and(|t| !t.trim().is_empty());
    let target = job.post_to.filter(|_| has_report);
    sqlx::query("UPDATE job_runs SET status = $2, ended_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), output = $3, error = $4, pending_to = $5 WHERE id = $1")
        .bind(run)
        .bind(o.status)
        .bind(&o.output)
        .bind(&o.error)
        .bind(target)
        .execute(db)
        .await?;
    let failed = o.status == "error";
    let failures: i32 = sqlx::query_scalar(
        "UPDATE jobs SET last_run_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), last_status = $2, last_error = $3, failures = CASE WHEN $4 THEN failures + 1 ELSE 0 END, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
         WHERE id = $1 RETURNING failures",
    )
    .bind(job.id)
    .bind(o.status)
    .bind(&o.error)
    .bind(failed)
    .fetch_one(db)
    .await?;
    if !failed || job.kind != "agent" {
        return Ok(target);
    }
    if failures >= MAX_FAILURES {
        sqlx::query("UPDATE jobs SET enabled = false, paused_reason = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1")
            .bind(job.id)
            .bind(format!("paused after {failures} failed runs in a row; last: {}", zen_proto::head(o.error.as_deref().unwrap_or("?"), 300)))
            .execute(db)
            .await?;
    } else {
        // Never more often than scheduled: the next run waits at least the backoff.
        let wait = BACKOFF_SECS[(failures as usize - 1).min(BACKOFF_SECS.len() - 1)];
        sqlx::query("UPDATE jobs SET next_run_at = MAX(next_run_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now','+' || $2 || ' seconds')) WHERE id = $1 AND next_run_at IS NOT NULL")
            .bind(job.id)
            .bind(wait as f64)
            .execute(db)
            .await?;
    }
    Ok(target)
}

/// Deliver completed reports once the target session is idle. The intent lives in job_runs, so
/// a restart doesn't lose a queued report. The tape append and delivered marker share one
/// transaction: a retry sees either both or neither. Holding the turns lock through the append
/// prevents a turn starting between the idle check and the new message.
async fn deliver_pending(app: &AppState) -> Result<()> {
    let rows: Vec<(i64, Uuid)> = sqlx::query_as(
        "SELECT id, pending_to FROM job_runs WHERE pending_to IS NOT NULL ORDER BY id LIMIT 64",
    )
    .fetch_all(&app.db)
    .await?;
    for (run, target) in rows {
        // Each report on its own: one that can't be delivered is retried until its 2-hour window
        // ends, then given up (it stays in the jobs feed), and never stops the others.
        if let Err(e) = deliver_one(app, run, target).await {
            tracing::error!("delivering job run {run} into session {target}: {e:#}");
            let _ = sqlx::query("UPDATE job_runs SET pending_to = NULL, error = $2 WHERE id = $1 AND pending_to IS NOT NULL AND ended_at < strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-2 hours')")
                .bind(run)
                .bind(format!("session delivery failed: {}; report retained in the jobs feed", zen_proto::head(&format!("{e:#}"), 300)))
                .execute(&app.db)
                .await;
        }
    }
    Ok(())
}

/// The longest one pass of `deliver_pending` may hold up the scheduler's tick.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Post one run's report into its target session, unless the target is busy (then it waits for
/// the next pass) or the delivery is no longer allowed (then it's dropped with the reason).
async fn deliver_one(app: &AppState, run: i64, target: Uuid) -> Result<()> {
    let turns = app.turns.lock().await;
    let mut tx = crate::storage::begin(&app.db).await?;
    let row = sqlx::query(
        "SELECT r.output, j.name FROM job_runs r JOIN jobs j ON j.id = r.job_id
         WHERE r.id = $1 AND r.pending_to = $2 AND r.status = 'ok'",
    )
    .bind(run)
    .bind(target)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else { return Ok(()) };
    let eligible: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sessions WHERE id = $1 AND kind IS NULL)")
        .bind(target)
        .fetch_one(&mut *tx)
        .await?;
    let too_late: bool = sqlx::query_scalar("SELECT ended_at < strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-2 hours') FROM job_runs WHERE id = $1")
        .bind(run)
        .fetch_one(&mut *tx)
        .await?;
    if !eligible || too_late {
        sqlx::query("UPDATE job_runs SET pending_to = NULL, error = $2 WHERE id = $1")
            .bind(run)
            .bind("session delivery unavailable or expired; report retained in the jobs feed")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(());
    }
    if turns.contains_key(&target) {
        return Ok(());
    }
    let job: String = row.get("name");
    let report: String = row.get::<Option<String>, _>("output").unwrap_or_default();
    let message = json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": format!("⏰ *{job}*\n\n{}", report.trim()) }],
        "job": job, "job_run": run, "timestamp": Utc::now().timestamp_millis(),
    });
    tape::append_in(&mut tx, target, "message", &message).await?;
    sqlx::query("UPDATE job_runs SET posted_to = $2, pending_to = NULL WHERE id = $1")
        .bind(run)
        .bind(target)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    drop(turns);
    app.emit(target, json!({ "type": "message", "message": message })).await;
    tracing::info!("job `{job}` run {run}: report posted into session {target}");
    Ok(())
}

/// The kernel's own jobs.
async fn run_system(app: &AppState, job: &Job) -> Result<Outcome> {
    match job.action.as_deref() {
        Some("sleep") => {
            let r = crate::memory::sleep(app, "nightly").await?;
            let line = format!(
                "{} entries, {} kept, {} archived, {} promoted (scorer: {})",
                r["entries"], r["kept"], r["dropped"], r["promoted"], r["scorer"].as_str().unwrap_or("none, by recency")
            );
            Ok(Outcome::ok(line))
        }
        Some("wiki") => Ok(upkeep(crate::brain::tend(&app.db).await?)),
        Some("skills") => Ok(upkeep(crate::workshop::tend(&app.db).await)),
        Some("prune") => Ok(upkeep(prune::prune_files(&crate::zen_home()))),
        Some("engines") => {
            let repo = PathBuf::from(&app.repo);
            let script = repo.join("scripts/update-engines.sh");
            anyhow::ensure!(script.exists(), "{} not found", script.display());
            let shell = tools::run_shell(&repo, &format!("bash {}", shell_quote(&script.display().to_string())), &[], Duration::from_secs(3600), None)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            let text = crate::secrets::mask_off_thread(shell.text).await;
            let out = zen_proto::tail(text.trim(), 2000);
            match shell.status {
                Some(Ok(s)) if s.success() => Ok(Outcome::ok(out)),
                Some(Ok(s)) => Ok(Outcome::error(format!("update-engines.sh exited with {s}"), Some(out))),
                _ => Ok(Outcome::error("update-engines.sh timed out after an hour", Some(out))),
            }
        }
        other => bail!("unknown system action {other:?}"),
    }
}

/// A nightly upkeep job's outcome: its findings, one per line, or `silent` when it found nothing.
fn upkeep(notes: Vec<String>) -> Outcome {
    if notes.is_empty() {
        Outcome::silent()
    } else {
        Outcome::ok(notes.join("\n"))
    }
}

/// The system jobs whose findings go into the morning note.
const UPKEEP: [&str; 4] = ["wiki", "skills", "routes", "prune"];

/// What the nightly upkeep jobs found in the last day (each one's latest run that found something),
/// for a session's instructions next to the sleep's morning note. None when they found nothing.
pub async fn upkeep_note(db: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    let found: Vec<String> = sqlx::query_scalar(
        "SELECT output FROM (SELECT r.output,j.action,row_number() OVER(PARTITION BY j.action ORDER BY r.id DESC) AS rn FROM job_runs r JOIN jobs j ON j.id = r.job_id
         WHERE j.kind = 'system' AND j.action IN (SELECT value FROM json_each($1)) AND r.status = 'ok' AND r.output IS NOT NULL
           AND r.ended_at > strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 day')
          ) WHERE rn=1 ORDER BY action",
    )
    .bind(serde_json::json!(UPKEEP))
    .fetch_all(db)
    .await?;
    Ok((!found.is_empty()).then(|| format!("Last night's upkeep:\n{}", zen_proto::head(&found.join("\n"), 1500))))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// What a scheduled session is told on top of its instructions.
fn job_note(job: &Job) -> String {
    format!(
        "<scheduled_job name=\"{}\" schedule=\"{}\" tz=\"{}\">\nYou are running a scheduled job, with no one in the conversation. Do the task in the message, then answer \
with the report the owner will read: lead with what needs their attention, be concise. If there is nothing new or worth their \
attention, answer exactly [SILENT]. If the task can't be done, start your answer with [FAILED] and say why.\n\
You can't ask the owner, delegate, or schedule jobs. Take no outward-facing or destructive action (sending, publishing, pushing, \
spending, deleting): prepare it and say in the report what the owner should do.\n</scheduled_job>",
        job.name, job.schedule, job.tz
    )
}

/// The instructions of a job's session (`session`): the parts it picked, its skills loaded, and
/// the job note.
async fn job_instructions(app: &AppState, job: &Job, who: &crate::agents::Agent, workspace: &std::path::Path) -> Result<String> {
    // The parts the job picked, among those its agent has.
    let parts: Vec<&str> = job.context.iter().map(String::as_str).filter(|p| who.parts.contains(p)).collect();
    let memory = crate::memory::render(&app.db, &who.name).await?;
    let skills_all = crate::skills::scan_roots(&who.skill_roots());
    let index = crate::skills::index_text(&skills_all);
    let mut s = crate::compile::compose(&parts, workspace, &app.repo, &memory, None, &index, &crate::compile::instructions_for(&who.name, workspace));
    for name in &job.skills {
        let skill = crate::skills::lookup(&skills_all, name).map_err(|e| anyhow::anyhow!(e))?;
        let text = crate::skills::load(skill, None).map_err(|e| anyhow::anyhow!(e))?;
        s.push_str(&format!("\n\n<skill name=\"{}/{}\">\n{}\n</skill>", skill.domain, skill.name, text.trim_end()));
    }
    s.push_str("\n\n");
    s.push_str(&job_note(job));
    Ok(s)
}

/// The last report of `job` before `run`, for the next run to tell what's new.
async fn last_report(app: &AppState, job: &Job, run: i64) -> Result<Option<String>> {
    let last: Option<Option<String>> = sqlx::query_scalar("SELECT output FROM job_runs WHERE job_id = $1 AND id <> $2 AND status = 'ok' ORDER BY started_at DESC LIMIT 1")
        .bind(job.id)
        .bind(run)
        .fetch_optional(&app.db)
        .await?;
    Ok(last.flatten().map(|l| zen_proto::head(&l, 3000)))
}

/// A run's own session (kind `job`), recorded on the run with the command a script runs.
async fn run_session(app: &AppState, job: &Job, run: i64, model: &str, effort: Option<&str>, workspace: &std::path::Path, command: Option<&str>) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, title, model, effort, kind, workspace, agent) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(format!("job: {}", job.name))
        .bind(model)
        .bind(effort)
        .bind(JOB)
        .bind(workspace.display().to_string())
        .bind(&job.agent)
        .execute(&app.db)
        .await?;
    sqlx::query("UPDATE job_runs SET session_id = $2, command = $3 WHERE id = $1").bind(run).bind(id).bind(command).execute(&app.db).await?;
    Ok(id)
}

/// How long a script job's command may run.
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(600);

/// A script job's run: its command, as the agent's `bash` would run it, in a fresh `job`
/// session, with no model. The masked output is the report (or `report_file` is); exit 0 with
/// nothing to say is `silent`, anything else an error. The run records the exact command and its
/// exit code.
async fn run_script(app: &AppState, job: &Job, run: i64) -> Result<Outcome> {
    let command = job.command.clone().context("a script job without a command")?;
    // As the agent's bash would run it: another agent's in its sandbox and, by default, its workspace.
    let who = crate::agents::load(&job.agent)?;
    let own = who.workspace_dir().map(|w| w.display().to_string());
    let workspace = workspace_of(app, job.workspace.as_deref().or(own.as_deref()));
    let sandbox = who.sandbox();
    let started = std::time::SystemTime::now();
    let id = run_session(app, job, run, "script", None, &workspace, Some(&command)).await?;
    tape::append(&app.db, id, "message", &json!({ "role": "user", "content": format!("$ {command}"), "kernel": true, "timestamp": Utc::now().timestamp_millis() })).await?;
    let session = id.to_string();
    let shell = tools::run_shell(&workspace, &command, &[("ZEN_SESSION_ID", session.as_str())], SCRIPT_TIMEOUT, sandbox.as_ref()).await;
    let (text, status) = match shell {
        Ok(tools::Shell { text, status }) => (crate::secrets::mask_off_thread(text).await, status),
        Err(e) => return Ok(Outcome::error(format!("couldn't run the command: {e}"), None)),
    };
    let exit = match &status {
        Some(Ok(s)) => s.code(),
        _ => None,
    };
    sqlx::query("UPDATE job_runs SET exit_code = $2 WHERE id = $1").bind(run).bind(exit).execute(&app.db).await?;
    let shown = crate::output::preview_at(&text, MAX_SCRIPT_REPORT).unwrap_or_else(|| text.clone());
    tape::append(&app.db, id, "message", &json!({ "role": "assistant", "content": [{ "type": "text", "text": format!("{}\n[exit code: {}]", shown.trim_end(), exit.map(|c| c.to_string()).unwrap_or_else(|| "none".into())) }], "timestamp": Utc::now().timestamp_millis() })).await?;
    let printed = (!text.trim().is_empty()).then(|| shown.trim_end().to_string());
    match status {
        Some(Ok(s)) if s.success() => {}
        Some(Ok(s)) => {
            let code = s.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
            return Ok(Outcome::error(format!("exit code {code}: {}", zen_proto::tail(text.trim_end(), 500)), printed));
        }
        Some(Err(e)) => return Ok(Outcome::error(format!("failed waiting for the command: {e}"), None)),
        None => return Ok(Outcome::error(format!("timed out after {}s and was killed", SCRIPT_TIMEOUT.as_secs()), printed)),
    }
    Ok(match (&job.report_file, printed) {
        (Some(file), _) => file_report(workspace, file, started, sandbox).await,
        (None, Some(report)) => Outcome::ok(report),
        (None, None) => Outcome::silent(),
    })
}

/// The most of a script's output that becomes its report: its head and tail past that.
const MAX_SCRIPT_REPORT: usize = 60 * 1024;

/// Resolve each run afresh, retaining an explicitly configured job or agent model.
async fn job_model(app: &AppState, job_model: Option<String>, agent_model: Option<String>) -> (String, Option<String>) {
    crate::prefs::new_session_defaults(app, job_model.or(agent_model)).await
}

/// An agent job: a fresh session with the job's instructions and its prompt; the final answer is
/// the report.
async fn run_agent(app: &AppState, job: &Job, run: i64) -> Result<Outcome> {
    let who = crate::agents::load(&job.agent)?;
    let (model, effort) = job_model(app, job.model.clone(), who.model.clone()).await;
    let inferred = if job.workspace.is_none() { inferred_workspace(app, job) } else { None };
    let own = who.workspace_dir().map(|w| w.display().to_string());
    let workspace = workspace_of(app, job.workspace.as_deref().or(inferred.as_deref()).or(own.as_deref()));
    // A report file older than this wasn't written by this run.
    let started = std::time::SystemTime::now();
    let id = run_session(app, job, run, &model, effort.as_deref(), &workspace, None).await?;
    let base = job_instructions(app, job, &who, &workspace).await?;
    tape::append(&app.db, id, "base", &json!({ "text": base })).await?;
    let mut prompt = job.prompt.clone().unwrap_or_default();
    if let Some(report) = last_report(app, job, run).await? {
        prompt.push_str(&format!("\n\n<last_report>\nThe report from this job's previous run, to tell what's new:\n{report}\n</last_report>"));
    }
    crate::turns::run_and_wait(app, id, JOB, &prompt).await?;
    let error: Option<String> = sqlx::query_scalar("SELECT error FROM turns WHERE session_id = $1 ORDER BY started_at DESC LIMIT 1").bind(id).fetch_optional(&app.db).await?.flatten();
    let answer = crate::delegate::final_answer(&app.db, id).await;
    let trimmed = answer.trim();
    if let Some(e) = error {
        return Ok(Outcome::error(e, (!trimmed.is_empty()).then(|| trimmed.to_string())));
    }
    if trimmed.is_empty() && job.report_file.is_none() {
        return Ok(Outcome::error("the run ended without a report", None));
    }
    if trimmed.starts_with("[SILENT]") {
        return Ok(Outcome::silent());
    }
    if let Some(why) = trimmed.strip_prefix("[FAILED]") {
        return Ok(Outcome::error(zen_proto::head(why.trim(), 500), Some(trimmed.to_string())));
    }
    let Some(file) = &job.report_file else {
        return Ok(Outcome::ok(trimmed.to_string()));
    };
    // The report is the file, as the run left it; the answer stays in the session. Never the answer
    // instead: whatever is wrong with the file makes the run an error.
    Ok(file_report(workspace, file, started, who.sandbox()).await)
}

/// The folder an agent job without a workspace of its own works in: the one existing folder its
/// prompt names (none when it names several), else where its report file is (the parent of a
/// conventional output folder such as `out/`). An explicit workspace always wins. Never infer
/// authority from model output or a report from an earlier run.
fn inferred_workspace(app: &AppState, job: &Job) -> Option<String> {
    let mut dirs: Vec<PathBuf> = job.prompt.as_deref().unwrap_or("")
        .split(|c: char| c.is_whitespace() || "()<>[]\"'`,".contains(c))
        .filter(|word| word.starts_with('/'))
        .filter_map(|word| {
            let path = PathBuf::from(word.trim_end_matches(['.', ';', ':']));
            let dir = if path.is_file() {path.parent()?.to_path_buf()} else {path};
            dir.is_dir().then(|| dir.canonicalize().ok()).flatten()
        }).collect();
    dirs.sort(); dirs.dedup();
    if dirs.len()==1 { return Some(dirs[0].display().to_string()); }
    if !dirs.is_empty() { return None; }
    let path = std::path::Path::new(job.report_file.as_deref()?);
    let parent = path.parent()?;
    // report_file is stored absolute, even when originally relative. Preserve those relative
    // paths: out/message.md must still mean <workspace>/out/message.md, not out/out/message.md.
    // Never enlarge the original report boundary from a stored path changed behind the API.
    let original = workspace_of(app, job.workspace.as_deref());
    let parent_real = real_path(parent)?;
    let original_real = real_path(&original)?;
    if !parent_real.starts_with(&original_real) { return None; }
    let folder = if matches!(parent.file_name().and_then(|n| n.to_str()), Some("out"|"output"|"outputs"|"reports")) {
        parent.parent()?
    } else if parent_real == original_real { parent } else { return None; };
    Some(folder.display().to_string())
}

/// The directory a job works in: its own (relative to the kernel's workspace), else the workspace.
fn workspace_of(app: &AppState, dir: Option<&str>) -> PathBuf {
    dir.map(|w| tools::resolve(&app.workspace, w)).unwrap_or_else(|| app.workspace.clone())
}

// ---------- the agent's tool ----------

pub fn spec() -> Value {
    json!({
        "name": "schedule",
        "description": "Run a self-contained prompt in fresh unattended sessions (no ask, delegate, schedule or outward actions). Create one only when the owner asked for timed or recurring work; it is live at once. Schedules: 5-field cron in tz, every 2h, at YYYY-MM-DD HH:MM, in 30m; at most every 5 minutes. context: identity/agents/user/memory/skills/project besides soul (default user,memory). Reports go to the jobs feed or post_to. report_file: a workspace UTF-8 file (<=60 KB) the run writes, posted instead of its answer; empty is silent, invalid fails.",
        "parameters": { "type": "object", "properties": {
            "action": { "type": "string", "enum": ["create", "update", "list", "runs", "pause", "resume", "remove", "run"] },
            "name": { "type": "string", "description": "The job: lowercase letters, digits and hyphens (e.g. morning-brief)" },
            "prompt": { "type": "string", "description": "create/update: the task, self-contained (the run sees nothing of this conversation)" },
            "command": { "type": "string", "description": "Instead of prompt: a shell command run with no model" },
            "schedule": { "type": "string", "description": "create/update: when it runs" },
            "tz": { "type": "string", "description": "Optional IANA timezone (default: the owner's ZEN_TZ)" },
            "context": { "type": "array", "items": { "type": "string" }, "description": "Optional: instruction parts besides the soul" },
            "skills": { "type": "array", "items": { "type": "string" }, "description": "Optional: skills loaded into its instructions (domain/name)" },
            "dir": { "type": "string", "description": "Optional: the directory it works in (default: the workspace)" },
            "report_file": { "type": "string", "description": "Optional report file (\"\" clears)" },
            "plugin": { "type": "string", "description": "Optional plugin name when scheduling a routine from a plugin; empty clears it on update" },
            "post_to": { "type": "string", "description": "Optional: `this` or a session id (\"\" clears)" } },
          "required": ["action"] }
    })
}

fn describe(j: &Job) -> String {
    let when = match (j.enabled, &j.next_run_at) {
        (false, _) => format!("paused ({})", j.paused_reason.as_deref().unwrap_or("paused")),
        (true, Some(t)) => format!("next run {}", t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        (true, None) => "no run ahead".into(),
    };
    let last = j.last_status.as_ref().map(|s| format!("; last run: {s}")).unwrap_or_default();
    let command = j.command.as_deref().map(|c| format!("\n  $ {}", zen_proto::head(c, 300))).unwrap_or_default();
    format!("- {} ({}, `{}` {}): {when}{last}{command}", j.name, j.kind, j.schedule, j.tz)
}

async fn tool(app: &AppState, session: Uuid, args: &Value) -> Result<String> {
    // Only dispatch can supply this after consuming an audited exact owner grant.
    let by = By::Agent(session);
    let name = args["name"].as_str().unwrap_or("").trim().to_string();
    let need = || -> Result<&str> {
        anyhow::ensure!(!name.is_empty(), "this action needs the job's `name`");
        Ok(name.as_str())
    };
    let live = |j: &Job, paused: Option<String>| match paused {
        None => format!("Job `{}` is live: {}.", j.name, describe(j).trim_start_matches("- ")),
        Some(p) => format!("Job `{}` is saved but paused, {p}. Tell the owner: `zen jobs resume {}` turns it on.", j.name, j.name),
    };
    Ok(match args["action"].as_str().unwrap_or("") {
        "create" => {
            let (j, p) = create(app, Spec::from_json(args), by).await?;
            live(&j, p)
        }
        "update" => {
            let (j, p) = update(app, need()?, Spec::from_json(args), by).await?;
            live(&j, p)
        }
        "list" => {
            let jobs = list(&app.db).await?;
            if jobs.is_empty() {
                "No jobs.".into()
            } else {
                jobs.iter().map(describe).collect::<Vec<_>>().join("\n")
            }
        }
        "runs" => {
            let n = (!name.is_empty()).then_some(name.as_str());
            let rs = runs(&app.db, n, 10).await?;
            if rs.is_empty() {
                return Ok("No runs yet.".into());
            }
            let mut lines = Vec::new();
            for r in &rs {
                let body = r["output"].as_str().or(r["error"].as_str()).map(|t| zen_proto::head(t, 600)).unwrap_or_default();
                let job = r["job"].as_str().unwrap_or("");
                lines.push(format!("- {job} {} ({}, {}): {body}", r["started_at"].as_str().unwrap_or(""), r["status"].as_str().unwrap_or(""), r["trigger"].as_str().unwrap_or("")));
            }
            lines.join("\n")
        }
        "pause" => {
            let (j, _) = set_enabled(app, need()?, false, by).await?;
            format!("Paused `{}`.", j.name)
        }
        "resume" => {
            let (j, p) = set_enabled(app, need()?, true, by).await?;
            live(&j, p)
        }
        "remove" => {
            let j = get(&app.db, need()?).await?;
            anyhow::ensure!(j.kind == "agent", "`{}` is a system job: only the owner can pause it", j.name);
            ensure_own(app, &j, &by).await?;
            remove(&app.db, &j.name).await?;
            format!("Removed `{}` (its runs stay on record).", j.name)
        }
        "run" => {
            let run = run_now(app, need()?, by).await?;
            format!("Started run {run} of `{name}` in the background; its report shows in `runs` when it ends.")
        }
        other => bail!("action must be create, update, list, runs, pause, resume, remove or run, not `{other}`"),
    })
}

/// Run `schedule`. None for other tools.
pub async fn run_tool(app: &AppState, session: Uuid, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    if name != "schedule" {
        return None;
    }
    Some(tools::ToolOutput::from_result(tool(app, session, args).await, "schedule failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn job_models_follow_live_preferences_without_overriding_explicit_models() {
        let (_dir, db) = crate::test_util::database().await;
        let app = crate::test_util::app(db);
        app.catalog.lock().await.insert("faux/smoke".into(), json!({ "id": "faux/smoke", "efforts": ["low", "high"] }));
        app.catalog.lock().await.insert("faux/other".into(), json!({ "id": "faux/other", "efforts": [] }));
        assert_eq!(job_model(&app, None, None).await, ("faux/smoke".into(), None));
        crate::prefs::patch(&app, &json!({ "default_model": "faux/smoke", "default_effort": "high" })).await.unwrap();
        assert_eq!(job_model(&app, None, None).await, ("faux/smoke".into(), Some("high".into())));
        assert_eq!(job_model(&app, Some("faux/other".into()), Some("faux/smoke".into())).await, ("faux/other".into(), None));
        assert_eq!(job_model(&app, None, Some("faux/smoke".into())).await, ("faux/smoke".into(), Some("high".into())));
        crate::prefs::patch(&app, &json!({ "default_model": "faux/other" })).await.unwrap();
        assert_eq!(job_model(&app, None, None).await, ("faux/other".into(), None));
        assert_eq!(job_model(&app, Some("faux/smoke".into()), None).await, ("faux/smoke".into(), None));
        app.db.close().await;
    }

    #[test]
    fn context_always_has_the_soul_and_only_known_parts() {
        assert_eq!(context_of(None).unwrap(), ["soul", "user", "memory"]);
        assert_eq!(context_of(Some(vec!["memory".into(), "Soul".into()])).unwrap(), ["soul", "memory"]);
        assert!(context_of(Some(vec!["secrets".into()])).is_err());
        assert!(valid_name("morning-brief").is_ok() && valid_name("Morning Brief").is_err() && valid_name("-x").is_err() && valid_name("x-").is_err());
    }

}
