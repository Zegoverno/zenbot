//! Scheduled jobs (DESIGN.md, "Scheduled jobs"; D-046). One scheduler in the kernel, backed by the
//! `jobs` and `job_runs` tables, runs two kinds of job:
//!
//! - **system** jobs, the kernel's own maintenance, seeded at start: `sleep` (the nightly memory
//!   sleep, `memory::sleep`) and `engines` (`scripts/update-engines.sh`). The owner can pause and
//!   run them; nobody can remove them.
//! - **agent** jobs: a prompt run in a fresh session (kind `job`) with the instructions the job
//!   picks (`context`: the soul always, plus any of identity, agents, user, memory, skills,
//!   project) and the skills it names loaded. The session can't ask, delegate or schedule; its final
//!   answer is the report, `[SILENT]` when there's nothing worth the owner's attention.
//!
//! How it runs (patterns from Hermes and OpenClaw, read in code): the loop sleeps until the
//! earliest due job, at most a minute; due jobs are claimed with `FOR UPDATE SKIP LOCKED` and their
//! next run is set in the same transaction, so a crash loses at most one run and never repeats one;
//! the database allows one running run per job; runs left running by a restart are marked
//! `interrupted`. A run missed while the kernel was down runs once if it's within its grace (half
//! its period, 2 minutes to 2 hours), else it's recorded as missed. A failing agent job is spaced
//! out (never run more often than scheduled) and paused after 5 failures in a row.
//!
//! Who creates agent jobs: the owner (API, `zen jobs`) and the agent (`schedule`). An agent's job
//! goes live only when System One judges, from the owner's own messages in that session, that the
//! owner asked for it or clearly wants it (probability ≥ ZEN_JOB_BAR, 0.9); else it's created
//! paused until the owner resumes it. A session that read untrusted content always gets a paused
//! job. ZEN_JOBS=0 turns the scheduler off (test, smoke and eval kernels).

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{tape, tools, AppState};

/// The kind of a session a scheduled agent job runs in.
pub const JOB: &str = "job";
/// The owner's zone, the default for agent jobs (ZEN_TZ).
const DEFAULT_TZ: &str = "America/Sao_Paulo";
/// Instructions every agent job gets unless it says otherwise (the soul is always in).
const DEFAULT_CONTEXT: [&str; 3] = ["soul", "user", "memory"];
/// Shortest period of an agent job.
const MIN_PERIOD: Duration = Duration::from_secs(300);
/// Agent jobs paused after this many failed runs in a row.
const MAX_FAILURES: i32 = 5;
/// How long a failing agent job waits at least before its next run, by failures in a row.
const BACKOFF_SECS: [i64; 4] = [60, 300, 900, 3600];

/// The kernel's own jobs: (name, action, schedule in UTC, what it does).
const SYSTEM: [(&str, &str, &str, &str); 2] = [
    ("sleep", "sleep", "0 3 * * *", "tidy short-term memory: promote, rank, archive (memory::sleep)"),
    ("engines", "engines", "0 4 * * *", "update the Claude Code and Codex CLIs, tested, with rollback (scripts/update-engines.sh)"),
];

/// Wakes the loop when jobs change, so a new job's first run isn't up to a minute late.
static WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

// ---------- schedules ----------

/// When a job runs.
#[derive(Debug, Clone)]
pub enum Schedule {
    Cron(Box<croner::Cron>),
    Every(chrono::Duration),
    At(DateTime<Utc>),
}

/// A duration like `30m`, `2h`, `1d`, `90s`.
fn duration(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().ok()?;
    match unit.trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(chrono::Duration::seconds(n)),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(chrono::Duration::minutes(n)),
        "h" | "hour" | "hours" => Some(chrono::Duration::hours(n)),
        "d" | "day" | "days" => Some(chrono::Duration::days(n)),
        _ => None,
    }
}

pub fn parse_tz(tz: &str) -> Result<Tz> {
    Tz::from_str(tz.trim()).map_err(|_| anyhow::anyhow!("unknown timezone `{tz}` (use an IANA name such as America/Sao_Paulo or UTC)"))
}

/// Parse a schedule: a 5-field cron expression (or `@daily`, `@hourly`, …), `every <duration>`,
/// `at <date and time>` (read in `tz` unless it has an offset) or `in <duration>` (made into an `at`).
/// Returns the schedule and its canonical text (what's stored).
pub fn parse_schedule(text: &str, tz: Tz, now: DateTime<Utc>) -> Result<(Schedule, String)> {
    let t = text.trim();
    let lower = t.to_lowercase();
    if let Some(d) = lower.strip_prefix("every ") {
        let d = duration(d).context("`every` takes a duration such as 30m, 2h or 1d")?;
        anyhow::ensure!(d > chrono::Duration::zero(), "the interval must be positive");
        return Ok((Schedule::Every(d), format!("every {}", d_text(d))));
    }
    if let Some(d) = lower.strip_prefix("in ") {
        let d = duration(d).context("`in` takes a duration such as 30m, 2h or 1d")?;
        let at = now + d;
        return Ok((Schedule::At(at), format!("at {}", at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))));
    }
    if let Some(when) = t.strip_prefix("at ").or_else(|| t.strip_prefix("At ")) {
        let when = when.trim();
        let at = if let Ok(d) = DateTime::parse_from_rfc3339(when) {
            d.with_timezone(&Utc)
        } else {
            let naive = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
                .iter()
                .find_map(|f| NaiveDateTime::parse_from_str(when, f).ok())
                .context("`at` takes a date and time such as 2026-10-12 07:00 (in the job's timezone) or an RFC 3339 time")?;
            tz.from_local_datetime(&naive).earliest().context("that time doesn't exist in the job's timezone (a clock change)")?.with_timezone(&Utc)
        };
        return Ok((Schedule::At(at), format!("at {}", at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))));
    }
    // Minute granularity: no seconds or years field.
    anyhow::ensure!(t.starts_with('@') || t.split_whitespace().count() == 5, "a cron schedule has 5 fields: minute hour day-of-month month day-of-week (e.g. `0 7 * * 1-5`)");
    let cron = croner::Cron::from_str(t).map_err(|e| anyhow::anyhow!("bad cron expression `{t}`: {e}"))?;
    Ok((Schedule::Cron(Box::new(cron)), t.to_string()))
}

fn d_text(d: chrono::Duration) -> String {
    let s = d.num_seconds();
    if s % 86400 == 0 {
        format!("{}d", s / 86400)
    } else if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// The first run strictly after `after`, None when there's none (a one-shot in the past).
pub fn next_after(s: &Schedule, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match s {
        Schedule::Cron(c) => c.find_next_occurrence(&after.with_timezone(&tz), false).ok().map(|t| t.with_timezone(&Utc)),
        Schedule::Every(d) => Some(after + *d),
        Schedule::At(t) => (*t > after).then_some(*t),
    }
}

/// The time between two runs (the first two after `from`); None for a one-shot.
pub fn period(s: &Schedule, tz: Tz, from: DateTime<Utc>) -> Option<chrono::Duration> {
    match s {
        Schedule::At(_) => None,
        Schedule::Every(d) => Some(*d),
        Schedule::Cron(_) => {
            let a = next_after(s, tz, from)?;
            let b = next_after(s, tz, a)?;
            Some(b - a)
        }
    }
}

/// How late a run may start and still run: half its period, between 2 minutes and 2 hours (2 hours
/// for a one-shot).
pub fn grace(period: Option<chrono::Duration>) -> chrono::Duration {
    let p = period.unwrap_or(chrono::Duration::hours(4));
    (p / 2).clamp(chrono::Duration::minutes(2), chrono::Duration::hours(2))
}

// ---------- jobs ----------

#[derive(Debug, Clone)]
pub struct Job {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub action: Option<String>,
    pub prompt: Option<String>,
    pub context: Vec<String>,
    pub skills: Vec<String>,
    pub schedule: String,
    pub tz: String,
    pub model: Option<String>,
    pub workspace: Option<String>,
    pub enabled: bool,
    pub paused_reason: Option<String>,
    pub created_by: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub failures: i32,
}

const COLUMNS: &str = "id, name, kind, action, prompt, context, skills, schedule, tz, model, workspace, enabled, paused_reason, created_by, \
next_run_at, last_run_at, last_status, last_error, failures";

fn job_of(r: &sqlx::postgres::PgRow) -> Job {
    Job {
        id: r.get("id"),
        name: r.get("name"),
        kind: r.get("kind"),
        action: r.get("action"),
        prompt: r.get("prompt"),
        context: r.get("context"),
        skills: r.get("skills"),
        schedule: r.get("schedule"),
        tz: r.get("tz"),
        model: r.get("model"),
        workspace: r.get("workspace"),
        enabled: r.get("enabled"),
        paused_reason: r.get("paused_reason"),
        created_by: r.get("created_by"),
        next_run_at: r.get("next_run_at"),
        last_run_at: r.get("last_run_at"),
        last_status: r.get("last_status"),
        last_error: r.get("last_error"),
        failures: r.get("failures"),
    }
}

pub fn job_json(j: &Job) -> Value {
    json!({
        "name": j.name, "kind": j.kind, "action": j.action, "prompt": j.prompt, "context": j.context, "skills": j.skills,
        "schedule": j.schedule, "tz": j.tz, "model": j.model, "workspace": j.workspace, "enabled": j.enabled,
        "paused_reason": j.paused_reason, "created_by": j.created_by, "next_run_at": j.next_run_at, "last_run_at": j.last_run_at,
        "last_status": j.last_status, "last_error": j.last_error, "failures": j.failures,
    })
}

pub async fn get(db: &PgPool, name: &str) -> Result<Job> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM jobs WHERE name = $1 AND removed_at IS NULL"))
        .bind(name.trim())
        .fetch_optional(db)
        .await?
        .with_context(|| format!("no job `{}`; list them with `zen jobs`", name.trim()))?;
    Ok(job_of(&row))
}

pub async fn list(db: &PgPool) -> Result<Vec<Job>> {
    let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM jobs WHERE removed_at IS NULL ORDER BY kind DESC, name")).fetch_all(db).await?;
    Ok(rows.iter().map(job_of).collect())
}

/// Recent runs, newest first: of one job, or of all.
pub async fn runs(db: &PgPool, name: Option<&str>, limit: i64) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT r.id, j.name, r.trigger, r.status, r.session_id, r.started_at, r.ended_at, r.output, r.error
         FROM job_runs r JOIN jobs j ON j.id = r.job_id
         WHERE ($1::text IS NULL OR (j.name = $1 AND j.removed_at IS NULL)) ORDER BY r.started_at DESC, r.id DESC LIMIT $2",
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
            })
        })
        .collect())
}

/// What a new or changed agent job asks for (from the tool, the API or `zen jobs add`).
#[derive(Debug, Default, Clone)]
pub struct Spec {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub schedule: Option<String>,
    pub tz: Option<String>,
    pub context: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub model: Option<String>,
    pub workspace: Option<String>,
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
            schedule: s("schedule"),
            tz: s("tz"),
            context: strings(&v["context"]),
            skills: strings(&v["skills"]),
            model: s("model"),
            workspace: s("workspace").or_else(|| s("dir")),
        }
    }
}

fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !name.starts_with('-');
    anyhow::ensure!(ok, "a job name is lowercase letters, digits and hyphens, up to 40 characters (e.g. `morning-brief`)");
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

fn check_skills(skills: &[String]) -> Result<()> {
    let all = crate::skills::scan(&crate::skills::root());
    for s in skills {
        crate::skills::lookup(&all, s).map_err(|e| anyhow::anyhow!(e))?;
    }
    Ok(())
}

/// The schedule checked for an agent job: parsed, not more often than every 5 minutes, and with a run
/// ahead. Returns the canonical text and the first run.
fn checked_schedule(text: &str, tz: Tz, now: DateTime<Utc>) -> Result<(String, DateTime<Utc>)> {
    let (s, canon) = parse_schedule(text, tz, now)?;
    if let Some(p) = period(&s, tz, now) {
        anyhow::ensure!(p >= chrono::Duration::from_std(MIN_PERIOD).unwrap_or_default(), "an agent job runs at most every 5 minutes");
    }
    let next = next_after(&s, tz, now).context("that schedule has no run ahead (a time in the past?)")?;
    Ok((canon, next))
}

/// Who is creating or changing a job, and whether it may go live.
#[derive(Clone, Copy)]
pub enum By {
    Owner,
    /// The agent, in this session; whether it may go live is decided by the gate.
    Agent(Uuid),
}

/// Create an agent job. Returns the job and, for the agent, why it's paused if it is.
pub async fn create(app: &AppState, spec: Spec, by: By) -> Result<(Job, Option<String>)> {
    let name = spec.name.clone().context("a job needs a `name`")?;
    valid_name(&name)?;
    let prompt = spec.prompt.clone().context("a job needs a `prompt`: the task, self-contained (it sees nothing of this conversation)")?;
    let tz_text = spec.tz.clone().unwrap_or_else(|| std::env::var("ZEN_TZ").unwrap_or_else(|_| DEFAULT_TZ.into()));
    let tz = parse_tz(&tz_text)?;
    let (schedule, next) = checked_schedule(spec.schedule.as_deref().context("a job needs a `schedule`")?, tz, Utc::now())?;
    let context = context_of(spec.context.clone())?;
    let skills = spec.skills.clone().unwrap_or_default();
    check_skills(&skills)?;
    let (created_by, session, model) = match &by {
        By::Owner => ("owner", None, spec.model.clone()),
        // The agent can't point unattended spending at another model: the default it is.
        By::Agent(s) => ("agent", Some(*s), None),
    };
    if let Some(m) = &model {
        anyhow::ensure!(app.routes.lock().await.contains_key(m), "no worker serves model `{m}`");
    }
    let (approval, paused) = match &by {
        By::Owner => (None, None),
        By::Agent(s) => gate(app, *s, &name, &prompt, &schedule).await,
    };
    let row = sqlx::query(&format!(
        "INSERT INTO jobs (name, kind, prompt, context, skills, schedule, tz, model, workspace, enabled, paused_reason, created_by, created_in, approval, next_run_at)
         VALUES ($1, 'agent', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING {COLUMNS}"
    ))
    .bind(&name)
    .bind(&prompt)
    .bind(&context)
    .bind(&skills)
    .bind(&schedule)
    .bind(tz.name())
    .bind(&model)
    .bind(&spec.workspace)
    .bind(paused.is_none())
    .bind(&paused)
    .bind(created_by)
    .bind(session)
    .bind(&approval)
    .bind(next)
    .fetch_one(&app.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => anyhow::anyhow!("a job named `{name}` already exists; change it with `update`"),
        _ => e.into(),
    })?;
    WAKE.notify_one();
    Ok((job_of(&row), paused))
}

/// Change an agent job. A change by the agent passes the gate again (a changed task is a new one).
pub async fn update(app: &AppState, name: &str, spec: Spec, by: By) -> Result<(Job, Option<String>)> {
    let job = get(&app.db, name).await?;
    anyhow::ensure!(job.kind == "agent", "`{}` is a system job: only pause, resume and run apply", job.name);
    let prompt = spec.prompt.clone().or(job.prompt.clone()).unwrap_or_default();
    let tz = parse_tz(spec.tz.as_deref().unwrap_or(&job.tz))?;
    let (schedule, next) = checked_schedule(spec.schedule.as_deref().unwrap_or(&job.schedule), tz, Utc::now())?;
    let context = match spec.context.clone() {
        Some(c) => context_of(Some(c))?,
        None => job.context.clone(),
    };
    let skills = spec.skills.clone().unwrap_or(job.skills.clone());
    check_skills(&skills)?;
    let model = match &by {
        By::Owner => spec.model.clone().or(job.model.clone()),
        By::Agent(_) => {
            anyhow::ensure!(spec.model.is_none(), "only the owner sets a job's model");
            job.model.clone()
        }
    };
    let (approval, paused) = match &by {
        By::Owner => (None, if job.enabled { None } else { job.paused_reason.clone() }),
        By::Agent(s) => gate(app, *s, &job.name, &prompt, &schedule).await,
    };
    let enabled = match &by {
        By::Owner => job.enabled,
        By::Agent(_) => job.enabled && paused.is_none(),
    };
    let paused = if enabled { None } else { paused.or(job.paused_reason.clone()) };
    let row = sqlx::query(&format!(
        "UPDATE jobs SET prompt = $2, context = $3, skills = $4, schedule = $5, tz = $6, model = $7, workspace = COALESCE($8, workspace),
                enabled = $9, paused_reason = $10, approval = COALESCE($11, approval), next_run_at = $12, failures = 0, updated_at = now()
         WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(job.id)
    .bind(&prompt)
    .bind(&context)
    .bind(&skills)
    .bind(&schedule)
    .bind(tz.name())
    .bind(&model)
    .bind(&spec.workspace)
    .bind(enabled)
    .bind(&paused)
    .bind(&approval)
    .bind(next)
    .fetch_one(&app.db)
    .await?;
    WAKE.notify_one();
    Ok((job_of(&row), if enabled { None } else { paused }))
}

/// Pause or resume a job. The agent may pause any agent job; resuming one passes the gate.
pub async fn set_enabled(app: &AppState, name: &str, on: bool, by: By) -> Result<(Job, Option<String>)> {
    let job = get(&app.db, name).await?;
    let mut reason = (!on).then(|| match by {
        By::Owner => "paused by the owner".to_string(),
        By::Agent(_) => "paused by the agent".to_string(),
    });
    if let By::Agent(s) = by {
        anyhow::ensure!(job.kind == "agent", "only the owner pauses or resumes a system job");
        if on {
            let (_, paused) = gate(app, s, &job.name, job.prompt.as_deref().unwrap_or(""), &job.schedule).await;
            if let Some(p) = paused {
                return Ok((job, Some(p)));
            }
        }
    }
    // A job resumed after a while starts from now: its missed runs aren't made up.
    let next = if on {
        let tz = parse_tz(&job.tz)?;
        let (s, _) = parse_schedule(&job.schedule, tz, Utc::now())?;
        next_after(&s, tz, Utc::now())
    } else {
        job.next_run_at
    };
    if on {
        reason = None;
    }
    let row = sqlx::query(&format!("UPDATE jobs SET enabled = $2, paused_reason = $3, next_run_at = $4, failures = CASE WHEN $2 THEN 0 ELSE failures END, updated_at = now() WHERE id = $1 RETURNING {COLUMNS}"))
        .bind(job.id)
        .bind(on)
        .bind(&reason)
        .bind(next)
        .fetch_one(&app.db)
        .await?;
    WAKE.notify_one();
    Ok((job_of(&row), None))
}

/// Remove an agent job (its runs stay).
pub async fn remove(db: &PgPool, name: &str) -> Result<()> {
    let job = get(db, name).await?;
    anyhow::ensure!(job.kind == "agent", "`{}` is a system job: pause it instead", job.name);
    sqlx::query("UPDATE jobs SET removed_at = now(), enabled = false, updated_at = now() WHERE id = $1").bind(job.id).execute(db).await?;
    Ok(())
}

/// Run a job now, in the background. Returns the run's id. Refused when it's already running, or
/// for a paused agent job (it may be awaiting the owner's approval) unless the owner asks.
pub async fn run_now(app: &AppState, name: &str, by: By) -> Result<i64> {
    let job = get(&app.db, name).await?;
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

// ---------- the gate for the agent's jobs ----------

/// Whether a job the agent creates (or changes, or resumes) may go live: System One reads the owner's
/// own messages in the session (never tool output or web text) and judges whether they asked for it
/// or clearly want it. Returns the judgment (for the record) and, when it may not, why it's paused.
async fn gate(app: &AppState, session: Uuid, name: &str, prompt: &str, schedule: &str) -> (Option<Value>, Option<String>) {
    if crate::taint::tainted(&app.db, session).await {
        return (None, Some("awaiting the owner's approval: created in a session that read untrusted content".into()));
    }
    let bar = crate::env_num("ZEN_JOB_BAR", 0.9);
    let owner = owner_words(&app.db, session).await;
    if owner.is_empty() {
        return (None, Some("awaiting the owner's approval: the owner said nothing in this session".into()));
    }
    if crate::score::scorer().is_none() {
        return (None, Some("awaiting the owner's approval: no System One model to judge it (ZEN_S1_MODEL)".into()));
    }
    let state = json!({ "owner_messages": owner, "job": { "name": name, "task": zen_proto::head(prompt, 3000), "schedule": schedule } });
    let questions = json!({ "wanted": {
        "type": "bool",
        "instructions": "An AI agent wants to schedule this job to run on its own, without the owner present. Read only the owner's messages. Did the owner ask for this job, or clearly agree to it?",
        "criteria": {
            "true": "The owner asked for this recurring or timed task, or clearly agreed when it was proposed, and its task and timing match what they said",
            "false": "The owner didn't ask for it or agree to it, it's only the agent's idea, or its task or timing differs from what the owner said"
        }
    }});
    let res = crate::score::decide(app, &state, &questions).await;
    let (answer, p, error) = match &res {
        Ok(v) if v["error"].is_null() => (v.clone(), v["answers"]["wanted"]["probability"].as_f64(), None),
        Ok(v) => (v.clone(), None, v["error"].as_str().map(String::from)),
        Err(e) => (Value::Null, None, Some(format!("{e:#}"))),
    };
    let live = p.is_some_and(|p| p >= bar);
    crate::agent::log_decision(&app.db, Some(session), "job_approval", &state, &answer, Some(if live { "live" } else { "paused" }), p, true, error.as_deref()).await;
    let record = Some(json!({ "probability": p, "bar": bar, "error": error }));
    if live {
        (record, None)
    } else {
        let why = match (p, &error) {
            (Some(p), _) => format!("awaiting the owner's approval: System One gave {p:.2} (needs {bar:.2}) that the owner asked for it"),
            (None, Some(e)) => format!("awaiting the owner's approval: System One couldn't judge it ({})", zen_proto::head(e, 200)),
            _ => "awaiting the owner's approval".into(),
        };
        (record, Some(why))
    }
}

/// The owner's own messages in a session, newest last (not kernel prompts), about 6,000 characters.
async fn owner_words(db: &PgPool, session: Uuid) -> Vec<String> {
    let blocks = tape::load(db, session, &["message"]).await.unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut size = 0;
    for b in blocks.iter().rev().filter(|b| b.payload["role"] == "user" && b.payload["kernel"] != true) {
        let t = zen_proto::text_of(&b.payload["content"]);
        if t.trim().is_empty() {
            continue;
        }
        let t = zen_proto::head(&t, 2000);
        size += t.len();
        out.push(t);
        if size > 6000 || out.len() >= 12 {
            break;
        }
    }
    out.reverse();
    out
}

// ---------- the scheduler ----------

/// Create the kernel's own jobs if they aren't there.
pub async fn seed(db: &PgPool) -> Result<()> {
    for (name, action, schedule, _) in SYSTEM {
        let (s, canon) = parse_schedule(schedule, Tz::UTC, Utc::now())?;
        let next = next_after(&s, Tz::UTC, Utc::now());
        sqlx::query(
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
    }
    Ok(())
}

/// Whether the scheduler runs in this kernel (ZEN_JOBS, on by default).
pub fn enabled() -> bool {
    std::env::var("ZEN_JOBS").map(|v| v.trim() != "0").unwrap_or(true)
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
    match sqlx::query(
        "WITH r AS (UPDATE job_runs SET status = 'interrupted', ended_at = now(), error = 'the kernel stopped during the run'
                    WHERE status = 'running' RETURNING job_id)
         UPDATE jobs SET last_status = 'interrupted', updated_at = now() WHERE id IN (SELECT job_id FROM r)",
    )
    .execute(&app.db)
    .await
    {
        Ok(r) if r.rows_affected() > 0 => tracing::warn!("{} job run(s) interrupted by a restart", r.rows_affected()),
        Ok(_) => {}
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
        let most = Duration::from_secs_f64(crate::env_num("ZEN_JOBS_TICK", 60.0).clamp(0.2, 60.0));
        let wait = wait.clamp(Duration::from_millis(200).min(most), most);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = WAKE.notified() => {}
        }
    }
}

/// Start the runs that are due.
async fn tick(app: &AppState) -> Result<()> {
    let now = Utc::now();
    let mut tx = app.db.begin().await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE enabled AND removed_at IS NULL AND next_run_at <= now() ORDER BY next_run_at LIMIT 8 FOR UPDATE SKIP LOCKED"
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
                sqlx::query("UPDATE jobs SET enabled = false, paused_reason = $2, last_status = 'error', last_error = $2, updated_at = now() WHERE id = $1")
                    .bind(job.id)
                    .bind(format!("paused: its schedule can't be read ({e:#})"))
                    .execute(&mut *tx)
                    .await?;
                continue;
            }
        };
        let next = next_after(&sched, tz, now);
        let late = now - due;
        if late > grace(period(&sched, tz, due)) {
            sqlx::query("UPDATE jobs SET next_run_at = $2, last_status = 'missed', updated_at = now() WHERE id = $1").bind(job.id).bind(next).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO job_runs (job_id, trigger, status, ended_at, error) VALUES ($1, 'schedule', 'missed', now(), $2)")
                .bind(job.id)
                .bind(format!("due at {due}, {} minutes late (the kernel was down); skipped", late.num_minutes()))
                .execute(&mut *tx)
                .await?;
            continue;
        }
        sqlx::query("UPDATE jobs SET next_run_at = $2, updated_at = now() WHERE id = $1").bind(job.id).bind(next).execute(&mut *tx).await?;
        let trigger = if late > chrono::Duration::minutes(2) { "catchup" } else { "schedule" };
        let running: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM job_runs WHERE job_id = $1 AND status = 'running')").bind(job.id).fetch_one(&mut *tx).await?;
        if running {
            continue; // still on the previous run: this one is skipped, the next stays scheduled
        }
        let run: i64 = sqlx::query_scalar("INSERT INTO job_runs (job_id, trigger, status) VALUES ($1, $2, 'running') RETURNING id").bind(job.id).bind(trigger).fetch_one(&mut *tx).await?;
        start.push((job, run));
    }
    tx.commit().await?;
    for (job, run) in start {
        spawn_run(app, job, run);
    }
    Ok(())
}

/// A `running` row for a run of this job, unless one is running (None).
async fn start_run(db: &PgPool, job: i64, trigger: &str) -> Result<Option<i64>> {
    let r = sqlx::query_scalar("INSERT INTO job_runs (job_id, trigger, status) VALUES ($1, $2, 'running') ON CONFLICT DO NOTHING RETURNING id")
        .bind(job)
        .bind(trigger)
        .fetch_optional(db)
        .await?;
    Ok(r)
}

/// How a run ended.
struct Outcome {
    status: &'static str, // ok | silent | error
    output: Option<String>,
    error: Option<String>,
}

fn spawn_run(app: &AppState, job: Job, run: i64) {
    let app = app.clone();
    tokio::spawn(async move {
        // An upgrade waits for a running job.
        let _busy = app.background_work();
        tracing::info!("job `{}` run {run} started", job.name);
        let outcome = match job.kind.as_str() {
            "system" => run_system(&app, &job).await,
            _ => run_agent(&app, &job, run).await,
        };
        let outcome = outcome.unwrap_or_else(|e| Outcome { status: "error", output: None, error: Some(format!("{e:#}")) });
        if let Err(e) = finish(&app.db, &job, run, &outcome).await {
            tracing::error!("recording job `{}` run {run}: {e:#}", job.name);
        }
        tracing::info!("job `{}` run {run}: {}", job.name, outcome.status);
    });
}

/// Record how a run ended; space out or pause a failing agent job.
async fn finish(db: &PgPool, job: &Job, run: i64, o: &Outcome) -> Result<()> {
    sqlx::query("UPDATE job_runs SET status = $2, ended_at = now(), output = $3, error = $4 WHERE id = $1")
        .bind(run)
        .bind(o.status)
        .bind(&o.output)
        .bind(&o.error)
        .execute(db)
        .await?;
    let failed = o.status == "error";
    let failures: i32 = sqlx::query_scalar(
        "UPDATE jobs SET last_run_at = now(), last_status = $2, last_error = $3, failures = CASE WHEN $4 THEN failures + 1 ELSE 0 END, updated_at = now()
         WHERE id = $1 RETURNING failures",
    )
    .bind(job.id)
    .bind(o.status)
    .bind(&o.error)
    .bind(failed)
    .fetch_one(db)
    .await?;
    if !failed || job.kind != "agent" {
        return Ok(());
    }
    if failures >= MAX_FAILURES {
        sqlx::query("UPDATE jobs SET enabled = false, paused_reason = $2, updated_at = now() WHERE id = $1")
            .bind(job.id)
            .bind(format!("paused after {failures} failed runs in a row; last: {}", zen_proto::head(o.error.as_deref().unwrap_or("?"), 300)))
            .execute(db)
            .await?;
    } else {
        // Never more often than scheduled: the next run waits at least the backoff.
        let wait = BACKOFF_SECS[(failures as usize - 1).min(BACKOFF_SECS.len() - 1)];
        sqlx::query("UPDATE jobs SET next_run_at = GREATEST(next_run_at, now() + make_interval(secs => $2)) WHERE id = $1 AND next_run_at IS NOT NULL")
            .bind(job.id)
            .bind(wait as f64)
            .execute(db)
            .await?;
    }
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
            Ok(Outcome { status: "ok", output: Some(line), error: None })
        }
        Some("engines") => {
            let repo = PathBuf::from(&app.repo);
            let script = repo.join("scripts/update-engines.sh");
            anyhow::ensure!(script.exists(), "{} not found", script.display());
            let shell = tools::run_shell(&repo, &format!("bash {}", shell_quote(&script.display().to_string())), &[], false, Duration::from_secs(3600))
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            let text = crate::secrets::mask_off_thread(shell.text).await;
            let out = zen_proto::tail(text.trim(), 2000);
            match shell.status {
                Some(Ok(s)) if s.success() => Ok(Outcome { status: "ok", output: Some(out), error: None }),
                Some(Ok(s)) => Ok(Outcome { status: "error", output: Some(out), error: Some(format!("update-engines.sh exited with {s}")) }),
                _ => Ok(Outcome { status: "error", output: Some(out), error: Some("update-engines.sh timed out after an hour".into()) }),
            }
        }
        other => bail!("unknown system action {other:?}"),
    }
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

/// The instructions of a job's session: the parts it picked, its skills loaded, and the job note.
async fn job_instructions(app: &AppState, job: &Job, workspace: &std::path::Path) -> Result<String> {
    let parts: Vec<&str> = job.context.iter().map(String::as_str).collect();
    let memory = crate::memory::render(&app.db).await?;
    let skills_all = crate::skills::scan(&crate::skills::root());
    let index = crate::skills::index_text(&skills_all);
    let mut s = crate::compile::compose(&parts, workspace, &app.repo, &memory, None, &index);
    for name in &job.skills {
        let skill = crate::skills::lookup(&skills_all, name).map_err(|e| anyhow::anyhow!(e))?;
        let text = crate::skills::load(skill, None).map_err(|e| anyhow::anyhow!(e))?;
        s.push_str(&format!("\n\n<skill name=\"{}/{}\">\n{}\n</skill>", skill.domain, skill.name, text.trim_end()));
    }
    s.push_str("\n\n");
    s.push_str(&job_note(job));
    Ok(s)
}

/// An agent job: a fresh session with the job's instructions and its prompt; the final answer is
/// the report.
async fn run_agent(app: &AppState, job: &Job, run: i64) -> Result<Outcome> {
    let model = job.model.clone().unwrap_or_else(|| app.default_model.clone());
    let workspace = job.workspace.as_deref().map(|w| tools::resolve(&app.workspace, w)).unwrap_or_else(|| app.workspace.clone());
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, title, model, kind, workspace) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(format!("job: {}", job.name))
        .bind(&model)
        .bind(JOB)
        .bind(workspace.display().to_string())
        .execute(&app.db)
        .await?;
    sqlx::query("UPDATE job_runs SET session_id = $2 WHERE id = $1").bind(run).bind(id).execute(&app.db).await?;
    let base = job_instructions(app, job, &workspace).await?;
    tape::append(&app.db, id, "base", &json!({ "text": base })).await?;
    // The last report, so the run can tell what's new.
    let last: Option<String> = sqlx::query_scalar("SELECT output FROM job_runs WHERE job_id = $1 AND id <> $2 AND status = 'ok' ORDER BY started_at DESC LIMIT 1")
        .bind(job.id)
        .bind(run)
        .fetch_optional(&app.db)
        .await?
        .flatten();
    let mut prompt = job.prompt.clone().unwrap_or_default();
    if let Some(l) = last {
        prompt.push_str(&format!("\n\n<last_report>\nThe report from this job's previous run, to tell what's new:\n{}\n</last_report>", zen_proto::head(&l, 3000)));
    }
    crate::turns::run_and_wait(app, id, JOB, &prompt).await?;
    let error: Option<String> = sqlx::query_scalar("SELECT error FROM turns WHERE session_id = $1 ORDER BY started_at DESC LIMIT 1").bind(id).fetch_optional(&app.db).await?.flatten();
    let answer = crate::delegate::final_answer(&app.db, id).await;
    let trimmed = answer.trim();
    if let Some(e) = error {
        return Ok(Outcome { status: "error", output: (!trimmed.is_empty()).then(|| trimmed.to_string()), error: Some(e) });
    }
    if trimmed.is_empty() {
        return Ok(Outcome { status: "error", output: None, error: Some("the run ended without a report".into()) });
    }
    if trimmed.starts_with("[SILENT]") {
        // Nothing for the owner: the session is kept, out of the session list.
        sqlx::query("UPDATE sessions SET archived = true WHERE id = $1").bind(id).execute(&app.db).await?;
        return Ok(Outcome { status: "silent", output: None, error: None });
    }
    if let Some(why) = trimmed.strip_prefix("[FAILED]") {
        return Ok(Outcome { status: "error", output: Some(trimmed.to_string()), error: Some(zen_proto::head(why.trim(), 500)) });
    }
    Ok(Outcome { status: "ok", output: Some(trimmed.to_string()), error: None })
}

// ---------- the agent's tool ----------

pub fn spec() -> Value {
    json!({
        "name": "schedule",
        "description": "Schedule work to run on its own: a job runs your `prompt` in a fresh session at the times you set, with no one \
in the conversation (it can't ask, delegate or schedule, and takes no outward action); its final answer is a report the owner \
reads with `zen jobs runs`, or nothing when there's nothing new. Create one when the owner asks for recurring or timed work \
(\"every weekday at 7, …\", \"remind me tomorrow…\"). A job goes live only if the owner's own words in this conversation show \
they asked for it; otherwise it's created paused until they approve it, so if you aren't sure the owner wants it, ask first. \
Actions: create, update (changes the fields you give), list, runs (recent reports), pause, resume, remove, run (now). \
Schedules: a 5-field cron expression read in `tz` (`0 7 * * 1-5`), `every 2h`, `at 2026-10-12 07:00`, or `in 30m`; at most every 5 minutes. \
`context` picks the instructions it gets besides the soul: identity, agents, user, memory, skills, project (default: user, memory).",
        "parameters": { "type": "object", "properties": {
            "action": { "type": "string", "enum": ["create", "update", "list", "runs", "pause", "resume", "remove", "run"] },
            "name": { "type": "string", "description": "The job: lowercase letters, digits and hyphens (e.g. morning-brief)" },
            "prompt": { "type": "string", "description": "create/update: the task, self-contained (the run sees nothing of this conversation)" },
            "schedule": { "type": "string", "description": "create/update: when it runs" },
            "tz": { "type": "string", "description": "Optional IANA timezone (default America/Sao_Paulo)" },
            "context": { "type": "array", "items": { "type": "string" }, "description": "Optional: instruction parts besides the soul" },
            "skills": { "type": "array", "items": { "type": "string" }, "description": "Optional: skills loaded into its instructions (domain/name)" },
            "dir": { "type": "string", "description": "Optional: the directory it works in (default: the workspace)" } },
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
    format!("- {} ({}, `{}` {}): {when}{last}", j.name, j.kind, j.schedule, j.tz)
}

async fn tool(app: &AppState, session: Uuid, args: &Value) -> Result<String> {
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
            let (j, p) = create(app, Spec::from_json(args), By::Agent(session)).await?;
            live(&j, p)
        }
        "update" => {
            let (j, p) = update(app, need()?, Spec::from_json(args), By::Agent(session)).await?;
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
            rs.iter()
                .map(|r| {
                    let body = r["output"].as_str().or(r["error"].as_str()).map(|t| zen_proto::head(t, 600)).unwrap_or_default();
                    format!("- {} {} ({}, {}): {body}", r["job"].as_str().unwrap_or(""), r["started_at"].as_str().unwrap_or(""), r["status"].as_str().unwrap_or(""), r["trigger"].as_str().unwrap_or(""))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        "pause" => {
            let (j, _) = set_enabled(app, need()?, false, By::Agent(session)).await?;
            format!("Paused `{}`.", j.name)
        }
        "resume" => {
            let (j, p) = set_enabled(app, need()?, true, By::Agent(session)).await?;
            live(&j, p)
        }
        "remove" => {
            let j = get(&app.db, need()?).await?;
            anyhow::ensure!(j.kind == "agent", "`{}` is a system job: only the owner can pause it", j.name);
            remove(&app.db, &j.name).await?;
            format!("Removed `{}` (its runs stay on record).", j.name)
        }
        "run" => {
            let run = run_now(app, need()?, By::Agent(session)).await?;
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
    Some(match tool(app, session, args).await {
        Ok(content) => tools::ToolOutput { content, is_error: false },
        Err(e) => tools::ToolOutput { content: format!("schedule failed: {e:#}"), is_error: true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn cron_runs_in_the_jobs_timezone() {
        let tz = parse_tz("America/Sao_Paulo").unwrap();
        let (s, canon) = parse_schedule("0 7 * * 1-5", tz, Utc::now()).unwrap();
        assert_eq!(canon, "0 7 * * 1-5");
        // Friday 2026-10-09 12:00 UTC (09:00 in São Paulo): next is Monday 07:00 there, 10:00 UTC.
        assert_eq!(next_after(&s, tz, t("2026-10-09T12:00:00Z")), Some(t("2026-10-12T10:00:00Z")));
        assert_eq!(period(&s, tz, t("2026-10-12T11:00:00Z")), Some(chrono::Duration::days(1)));
    }

    #[test]
    fn every_in_and_at_parse() {
        let now = t("2026-10-09T12:00:00Z");
        let tz = parse_tz("America/Sao_Paulo").unwrap();
        let (s, canon) = parse_schedule("every 2h", tz, now).unwrap();
        assert_eq!(canon, "every 2h");
        assert_eq!(next_after(&s, tz, now), Some(t("2026-10-09T14:00:00Z")));
        let (s, canon) = parse_schedule("in 30m", tz, now).unwrap();
        assert_eq!(canon, "at 2026-10-09T12:30:00Z");
        assert_eq!(next_after(&s, tz, now), Some(t("2026-10-09T12:30:00Z")));
        assert_eq!(next_after(&s, tz, t("2026-10-09T13:00:00Z")), None, "a one-shot has nothing after it");
        let (_, canon) = parse_schedule("at 2026-10-12 07:00", tz, now).unwrap();
        assert_eq!(canon, "at 2026-10-12T10:00:00Z", "read in the job's timezone");
        assert!(parse_schedule("* * * * * *", tz, now).is_err(), "no seconds field");
        assert!(parse_schedule("every soon", tz, now).is_err());
        assert!(parse_tz("Mars/Olympus").is_err());
    }

    #[test]
    fn agent_jobs_run_at_most_every_five_minutes_and_have_a_run_ahead() {
        let now = t("2026-10-09T12:00:00Z");
        assert!(checked_schedule("* * * * *", Tz::UTC, now).is_err());
        assert!(checked_schedule("every 1m", Tz::UTC, now).is_err());
        assert!(checked_schedule("*/5 * * * *", Tz::UTC, now).is_ok());
        assert!(checked_schedule("at 2026-10-01 07:00", Tz::UTC, now).is_err(), "in the past");
    }

    #[test]
    fn grace_is_half_the_period_within_bounds() {
        assert_eq!(grace(Some(chrono::Duration::days(1))), chrono::Duration::hours(2));
        assert_eq!(grace(Some(chrono::Duration::minutes(30))), chrono::Duration::minutes(15));
        assert_eq!(grace(Some(chrono::Duration::minutes(2))), chrono::Duration::minutes(2));
        assert_eq!(grace(None), chrono::Duration::hours(2));
    }

    #[test]
    fn context_always_has_the_soul_and_only_known_parts() {
        assert_eq!(context_of(None).unwrap(), ["soul", "user", "memory"]);
        assert_eq!(context_of(Some(vec!["memory".into(), "Soul".into()])).unwrap(), ["soul", "memory"]);
        assert!(context_of(Some(vec!["secrets".into()])).is_err());
        assert!(valid_name("morning-brief").is_ok() && valid_name("Morning Brief").is_err() && valid_name("-x").is_err());
    }
}
