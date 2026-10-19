//! Scheduled jobs (DESIGN.md, "Scheduled jobs"; D-046). One scheduler in the kernel, backed by the
//! `jobs` and `job_runs` tables, runs two kinds of job:
//!
//! - **system** jobs, the kernel's own maintenance, seeded at start (an install from before a job
//!   existed gets it at its next start): `sleep` (the nightly memory sleep, `memory::sleep`),
//!   `wiki` (commit the agent's wiki edits, report problems: `wiki::tend`), `skills` (flag and
//!   archive unused skills: `workshop::tend`), `routes` (switch a delegation route on clear
//!   evidence: `delegate::tune`), `prune` (old saved outputs, prompt-file backups, database dumps and eval runs) and `engines`
//!   (`scripts/update-engines.sh`). Each records its own run; a run with nothing to report is
//!   `silent`. What `wiki`, `skills`, `routes` and `prune` found in the last day reaches the next
//!   sessions with the sleep's morning note (`upkeep_note`). The owner can pause and run them;
//!   nobody can remove them.
//! - **agent** jobs: a prompt run in a fresh session (kind `job`) with the instructions the job
//!   picks (`context`: the soul always, plus any of identity, agents, user, memory, skills,
//!   project) and the skills it names loaded. The session can't ask, delegate or schedule; its final
//!   answer is the report, `[SILENT]` when there's nothing worth the owner's attention; however the
//!   run ends, its session is archived (D-057). A job with
//!   a `report_file` (D-055) reports that file instead, as the run left it, when the run ends `ok`:
//!   inside its workspace, not a symlink, hardlink or secret file, written during the run, UTF-8, at most
//!   60 KB (`read_report`); anything else makes the run an error, never the answer posted instead.
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
//! job. A run whose session read untrusted content has its report marked (`job_runs.tainted`):
//! the next run gets it as untrusted `<last_report>` (and is tainted from its start), and
//! `schedule runs` shows it as untrusted content (D-053). ZEN_JOBS=0 turns the scheduler off (test,
//! smoke and eval kernels).
//! D-062: reports retain numeric origin risk and writer dependencies, including report_file's
//! exact bytes before trimming. Only clean reports reach idle, unstopped owner sessions;
//! delivery serializes with denial of the run, its known sources and the target. Listings
//! withhold contaminated reports; prompt parts admit only 0–1. Metadata failures fail closed.

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
/// Instructions every agent job gets unless it says otherwise (the soul is always in).
const DEFAULT_CONTEXT: [&str; 3] = ["soul", "user", "memory"];
/// Shortest period of an agent job.
const MIN_PERIOD: Duration = Duration::from_secs(300);
/// Agent jobs paused after this many failed runs in a row.
const MAX_FAILURES: i32 = 5;
/// How long a failing agent job waits at least before its next run, by failures in a row.
const BACKOFF_SECS: [i64; 4] = [60, 300, 900, 3600];

/// The kernel's own jobs: (name, action, schedule in UTC, what it does). The nightly ones run ten
/// minutes apart after the sleep, so each has the machine to itself and its own record.
const SYSTEM: [(&str, &str, &str, &str); 6] = [
    ("sleep", "sleep", "0 3 * * *", "tidy short-term memory: promote, rank, archive (memory::sleep)"),
    ("wiki", "wiki", "10 3 * * *", "commit the agent's wiki edits since the last capture, report pages without a summary and broken links (wiki::tend)"),
    ("skills", "skills", "20 3 * * *", "flag skills unused for 30 days, archive them at 90 (workshop::tend)"),
    ("routes", "routes", "30 3 * * *", "switch a delegation route where the owner's verdicts clearly favour another model (delegate::tune)"),
    ("prune", "prune", "40 3 * * *", "remove saved tool outputs older than 30 days, prompt-file backups older than 90, database dumps but the newest 3 and eval runs older than 7 days (jobs::prune_files)"),
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

/// A duration like `30m`, `2h`, `1d`, `90s`. None for a number too large to be a duration.
fn duration(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().ok()?;
    match unit.trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => chrono::Duration::try_seconds(n),
        "m" | "min" | "mins" | "minute" | "minutes" => chrono::Duration::try_minutes(n),
        "h" | "hour" | "hours" => chrono::Duration::try_hours(n),
        "d" | "day" | "days" => chrono::Duration::try_days(n),
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
        anyhow::ensure!(now.checked_add_signed(d).is_some(), "that interval is too long");
        return Ok((Schedule::Every(d), format!("every {}", d_text(d))));
    }
    if let Some(d) = lower.strip_prefix("in ") {
        let d = duration(d).context("`in` takes a duration such as 30m, 2h or 1d")?;
        let at = now.checked_add_signed(d).context("that is too far ahead")?;
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
        Schedule::Every(d) => after.checked_add_signed(*d),
        Schedule::At(t) => (*t > after).then_some(*t),
    }
}

/// The run after the one that was due at `due`, strictly after `now`. An interval keeps its rhythm
/// from the due time (`due + k·d`), not from when the tick saw it, so it doesn't drift; runs it
/// passed while late are skipped (the tick already ran or recorded the one due).
pub fn next_from(s: &Schedule, tz: Tz, due: DateTime<Utc>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match s {
        Schedule::Every(d) if due <= now && *d > chrono::Duration::zero() => {
            let (late, step) = ((now - due).num_milliseconds(), d.num_milliseconds().max(1));
            due.checked_add_signed(chrono::Duration::try_milliseconds((late / step + 1).checked_mul(step)?)?)
        }
        _ => next_after(s, tz, now),
    }
}

/// How many runs ahead `period` looks at: a cron expression's gaps can differ (`0,30-35 * * * *`).
const PERIOD_SAMPLES: usize = 50;

/// The shortest time between two runs (of the next PERIOD_SAMPLES after `from`); None for a one-shot.
pub fn period(s: &Schedule, tz: Tz, from: DateTime<Utc>) -> Option<chrono::Duration> {
    match s {
        Schedule::At(_) => None,
        Schedule::Every(d) => Some(*d),
        Schedule::Cron(_) => {
            let mut prev = next_after(s, tz, from)?;
            let mut min: Option<chrono::Duration> = None;
            for _ in 0..PERIOD_SAMPLES {
                let Some(next) = next_after(s, tz, prev) else { break };
                min = Some(min.map_or(next - prev, |m| m.min(next - prev)));
                prev = next;
            }
            min
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
    /// The file whose text is the report of a run that ends `ok` (D-055), inside the workspace.
    pub report_file: Option<String>,
    /// The session a run's report is posted into (D-060), instead of only the jobs feed.
    pub post_to: Option<Uuid>,
    pub enabled: bool,
    pub paused_reason: Option<String>,
    pub created_by: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub failures: i32,
}

const COLUMNS: &str = "id, name, kind, action, prompt, context, skills, schedule, tz, model, workspace, report_file, post_to, enabled, paused_reason, created_by, \
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
    }
}

pub fn job_json(j: &Job) -> Value {
    json!({
        "name": j.name, "kind": j.kind, "action": j.action, "prompt": j.prompt, "context": j.context, "skills": j.skills,
        "schedule": j.schedule, "tz": j.tz, "model": j.model, "workspace": j.workspace, "report_file": j.report_file, "post_to": j.post_to, "enabled": j.enabled,
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
        "SELECT r.id, j.name, r.trigger, r.status, r.session_id, r.started_at, r.ended_at, r.output, r.error, (r.risk>=4 OR zen_session_contaminated(r.session_id)) AS tainted, (CASE WHEN zen_session_contaminated(r.session_id) THEN 5::smallint ELSE r.risk END) AS risk, r.posted_to, r.pending_to
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
                "risk": r.get::<i16, _>("risk"), "untrusted": r.get::<bool, _>("tainted"), "posted_to": r.get::<Option<Uuid>, _>("posted_to"),
                "pending_to": r.get::<Option<Uuid>, _>("pending_to"),
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
    /// `Some("")` clears it.
    pub report_file: Option<String>,
    /// A session id (or 8+ character prefix), or `this` (the agent's session); `Some("")` clears it.
    pub post_to: Option<String>,
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
            report_file: v["report_file"].as_str().map(|f| f.trim().to_string()),
            post_to: v["post_to"].as_str().map(|f| f.trim().to_string()),
        }
    }
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

/// The session a job posts its reports into (D-060): one of the owner's own sessions (not a job's,
/// a subagent's or a verifier's), named by its id, an 8+ character prefix of it, or `this` (the
/// agent's session). Empty clears it (None).
async fn post_target(db: &PgPool, text: &str, by: &By) -> Result<Option<Uuid>> {
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
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM sessions WHERE id::text LIKE $1 || '%' AND kind IS NULL LIMIT 2")
        .bind(&prefix)
        .fetch_all(db)
        .await?;
    match ids.as_slice() {
        [id] => Ok(Some(*id)),
        [] => bail!("no session of the owner's matches `{text}` (a job posts only into the owner's own sessions)"),
        _ => bail!("`{text}` matches more than one session; give more of the id"),
    }
}

/// Create an agent job. Returns the job and, for the agent, why it's paused if it is.
pub async fn create(app: &AppState, spec: Spec, by: By) -> Result<(Job, Option<String>)> {
    let name = spec.name.clone().context("a job needs a `name`")?;
    valid_name(&name)?;
    let prompt = spec.prompt.clone().context("a job needs a `prompt`: the task, self-contained (it sees nothing of this conversation)")?;
    let tz_text = spec.tz.clone().unwrap_or_else(|| crate::config::get().tz.clone());
    let tz = parse_tz(&tz_text)?;
    let (schedule, next) = checked_schedule(spec.schedule.as_deref().context("a job needs a `schedule`")?, tz, Utc::now())?;
    let context = context_of(spec.context.clone())?;
    let skills = spec.skills.clone().unwrap_or_default();
    check_skills(&skills)?;
    let workspace = workspace_of(app, spec.workspace.as_deref());
    let report_file = match spec.report_file.as_deref() {
        Some(f) if !f.is_empty() => Some(checked_report_file(&workspace, f)?),
        _ => None,
    };
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
        anyhow::ensure!(app.routes.lock().await.contains_key(m), "no worker serves model `{m}`");
    }
    let (approval, paused) = match &by {
        By::Owner => (None, None),
        By::Agent(s) => gate(app, *s, &name, &prompt, &schedule, report_file.as_deref(), post_to).await,
    };
    let row = sqlx::query(&format!(
        "INSERT INTO jobs (name, kind, prompt, context, skills, schedule, tz, model, workspace, enabled, paused_reason, created_by, created_in, approval, next_run_at, report_file, post_to, owner_approved)
         VALUES ($1, 'agent', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) RETURNING {COLUMNS}"
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
    .bind(&report_file)
    .bind(post_to)
    .bind(matches!(by, By::Owner))
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
    // Checked again against the workspace it will have: a moved workspace can leave it outside.
    let workspace = workspace_of(app, spec.workspace.as_deref().or(job.workspace.as_deref()));
    let report_file = match spec.report_file.as_deref().or(job.report_file.as_deref()) {
        Some(f) if !f.is_empty() => Some(checked_report_file(&workspace, f)?),
        _ => None,
    };
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
    let (approval, paused) = match &by {
        By::Owner => (None, if job.enabled { None } else { job.paused_reason.clone() }),
        By::Agent(s) => gate(app, *s, &job.name, &prompt, &schedule, report_file.as_deref(), post_to).await,
    };
    let enabled = match &by {
        By::Owner => job.enabled,
        By::Agent(_) => job.enabled && paused.is_none(),
    };
    let paused = if enabled { None } else { paused.or(job.paused_reason.clone()) };
    let row = sqlx::query(&format!(
        "UPDATE jobs SET prompt = $2, context = $3, skills = $4, schedule = $5, tz = $6, model = $7, workspace = COALESCE($8, workspace),
                enabled = $9, paused_reason = $10, approval = COALESCE($11, approval), next_run_at = $12, report_file = $13, post_to = $14, owner_approved = $15, failures = 0, updated_at = now()
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
    .bind(&report_file)
    .bind(post_to)
    .bind(matches!(by, By::Owner))
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
            let (_, paused) = gate(app, s, &job.name, job.prompt.as_deref().unwrap_or(""), &job.schedule, job.report_file.as_deref(), job.post_to).await;
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
        if matches!(by, By::Owner) && !job.enabled {
            // The owner going live with a job S1 kept paused says it should have gone live.
            if let Some(approval) = approval_of(&app.db, job.id).await {
                crate::s1::learn::label_job(&app.db, &approval, true).await;
            }
        }
    }
    let row = sqlx::query(&format!("UPDATE jobs SET enabled = $2, paused_reason = $3, next_run_at = $4, owner_approved = CASE WHEN $5 THEN true ELSE owner_approved END, failures = CASE WHEN $2 THEN 0 ELSE failures END, updated_at = now() WHERE id = $1 RETURNING {COLUMNS}"))
        .bind(job.id)
        .bind(on)
        .bind(&reason)
        .bind(next)
        .bind(on && matches!(by, By::Owner))
        .fetch_one(&app.db)
        .await?;
    WAKE.notify_one();
    Ok((job_of(&row), None))
}

/// How long a run waits for the owner to answer an action it held.
const HOLD_WAIT: Duration = Duration::from_secs(2 * 3600);

/// A run that held an action waits for the owner instead of failing (D-068): the hold is a queue
/// item (Matrix posts it to the owner); approving it resumes the session with the exact action,
/// and the run goes on once that turn ends. A refusal, a deny, or no answer within `HOLD_WAIT`
/// ends the wait; the run is then judged on what it has.
async fn wait_for_owner(app: &AppState, session: Uuid) -> Result<()> {
    let deadline = std::time::Instant::now() + HOLD_WAIT;
    loop {
        let pending: Option<i64> = sqlx::query_scalar(
            "SELECT g.id FROM action_gates g WHERE g.session_id=$1 AND g.created_at>now()-interval '1 day'
              AND ((g.decision IS NULL AND g.allowed_at IS NULL) OR (g.decision IN ('approved','rejected') AND NOT EXISTS
                (SELECT 1 FROM turns t WHERE t.session_id=g.session_id AND t.started_at>=g.resolved_at AND t.ended_at IS NOT NULL)))
              ORDER BY g.id DESC LIMIT 1",
        )
        .bind(session)
        .fetch_optional(&app.db)
        .await?;
        let Some(gate) = pending else { return Ok(()) };
        tracing::info!("job session {session} waits for the owner on held action {gate}");
        loop {
            anyhow::ensure!(std::time::Instant::now() < deadline, "timed out waiting for owner review of held job action #{gate}");
            anyhow::ensure!(!crate::taint::stopped(&app.db, session).await?, "held job stopped by owner denial");
            let decision: (Option<String>, Option<DateTime<Utc>>) = sqlx::query_as("SELECT decision,resolved_at FROM action_gates WHERE id=$1")
                .bind(gate).fetch_one(&app.db).await?;
            match decision.0.as_deref() {
                None => tokio::time::sleep(Duration::from_secs(1)).await,
                Some("denied") => anyhow::bail!("held job action #{gate} denied by the owner"),
                Some(_) => {
                    // Approval/rejection commits before its continuation starts. Wait for the
                    // actual continuation's recorded end, not a guessed sleep followed by busy.
                    let completed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM turns WHERE session_id=$1 AND started_at >= $2 AND ended_at IS NOT NULL)")
                        .bind(session).bind(decision.1).fetch_one(&app.db).await?;
                    if completed { break; }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
}

/// Registrable domain including private suffixes: neighbouring github.io tenants stay separate.
pub fn registrable(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    static RULES: std::sync::LazyLock<std::collections::HashSet<&'static str>> = std::sync::LazyLock::new(|| {
        include_str!("../public-suffix-list.dat").lines().map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//")).collect()
    });
    if host.parse::<std::net::IpAddr>().is_ok() { return host; }
    let labels: Vec<&str> = host.split('.').collect();
    let mut suffix = 1;
    for i in 0..labels.len() {
        let tail = labels[i..].join(".");
        if RULES.contains(format!("!{tail}").as_str()) {
            suffix = labels.len() - i - 1;
            break;
        }
        if RULES.contains(tail.as_str()) || (i + 1 < labels.len() && RULES.contains(format!("*.{}", labels[i + 1..].join(".")).as_str())) {
            suffix = suffix.max(labels.len() - i);
        }
    }
    labels[labels.len().saturating_sub(suffix + 1)..].join(".")
}

/// Domains named by an explicitly owner-created/approved job's unchanged prompt (D-068).
/// A job S1 alone let go live gets no owner source authority. Empty for other sessions.
pub async fn owner_job_domains(db: &PgPool, session: Uuid) -> Vec<String> {
    let row = sqlx::query(
        "SELECT j.prompt FROM job_runs r JOIN jobs j ON j.id = r.job_id WHERE r.session_id = $1 AND j.kind = 'agent' AND j.owner_approved AND j.updated_at <= r.started_at
         LIMIT 1",
    )
    .bind(session)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let Some(row) = row else { return Vec::new() };
    let prompt: Option<String> = row.get("prompt");
    let mut out: Vec<String> = prompt.unwrap_or_default()
        .split(|c: char| c.is_whitespace() || "()<>[]\"'`,".contains(c))
        .filter_map(|word| {
            let word = word.trim_end_matches(['.', ';', '!']);
            let url = if word.contains("://") { word.to_string() } else { format!("https://{word}") };
            let parsed = reqwest::Url::parse(&url).ok()?;
            if !matches!(parsed.scheme(), "https"|"http") || !parsed.username().is_empty() || parsed.password().is_some() {return None;}
            let host = parsed.host_str()?;
            (host.contains('.') && !word.starts_with('/')).then(|| registrable(host))
        }).collect();
    out.sort();
    out.dedup();
    out
}

/// Whether `host` is under a domain the session's owner-approved job names.
pub async fn names_host(db: &PgPool, session: Uuid, host: &str) -> bool {
    let domain = registrable(host);
    owner_job_domains(db, session).await.contains(&domain)
}

/// The S1 judgment a job was created or changed with (`jobs.approval`), if any.
pub async fn approval_of(db: &PgPool, id: i64) -> Option<Value> {
    sqlx::query_scalar::<_, Option<Value>>("SELECT approval FROM jobs WHERE id = $1").bind(id).fetch_optional(db).await.ok().flatten().flatten()
}

/// The owner removing a job S1 kept paused, never run live: the pause was right.
pub async fn label_removed(db: &PgPool, name: &str) {
    if let Ok(job) = get(db, name).await {
        if !job.enabled {
            if let Some(approval) = approval_of(db, job.id).await {
                crate::s1::learn::label_job(db, &approval, false).await;
            }
        }
    }
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
/// or clearly want it; the task the agent wrote is shown as untrusted data, not evidence. Returns the judgment (for the record) and, when it may not, why it's paused.
async fn gate(app: &AppState, session: Uuid, name: &str, prompt: &str, schedule: &str, report_file: Option<&str>, post_to: Option<Uuid>) -> (Option<Value>, Option<String>) {
    if crate::taint::limited(&app.db, session, crate::taint::Limit::StartJob).await {
        return (None, Some("awaiting the owner's approval: created in a session that read untrusted content".into()));
    }
    let owner = owner_words(&app.db, session).await;
    if owner.is_empty() {
        return (None, Some("awaiting the owner's approval: the owner said nothing in this session".into()));
    }
    if !crate::s1::on() {
        return (None, Some("awaiting the owner's approval: no System One model to judge it (ZEN_S1_MODEL)".into()));
    }
    // The task is the agent's own text: it goes in marked as untrusted data, to be compared with
    // what the owner said, so whatever it claims (that the owner agreed, say) can't argue for it.
    let task = crate::taint::untrusted("agent", "the job's task, as the agent wrote it", &zen_proto::head(prompt, 3000));
    let mut job = json!({ "name": name, "schedule": schedule, "post_to": post_to, "task_written_by_agent": task });
    if let Some(f) = report_file {
        // What gets posted to the owner: the file, not the run's answer (D-055).
        job["report_file"] = json!(f);
    }
    // s1/job_approval.toml: what each mistake costs sets the bar (9 : 1, live from 0.9).
    let d = crate::s1::decide(&app.db, "job_approval", crate::s1::Facts::new(Some(session)).with("owner_messages", json!(owner)).with("job", job)).await;
    let p = d.p.or(d.prob("wanted"));
    let record = Some(json!({ "probability": p, "decision": d.id, "outcome": d.outcome, "error": d.error }));
    if d.is("live") {
        (record, None)
    } else {
        let why = match (p, &d.error, d.outcome.as_deref()) {
            (_, _, Some("ask_owner")) => "awaiting the owner's approval: System One wasn't sure enough".to_string(),
            (Some(p), _, _) => match crate::s1::threshold("job_approval") {
                Some(bar) => format!("awaiting the owner's approval: System One gave {p:.2} (needs {bar:.2}) that the owner asked for it"),
                None => format!("awaiting the owner's approval: System One gave {p:.2} that the owner asked for it"),
            },
            (None, Some(e), _) => format!("awaiting the owner's approval: System One couldn't judge it ({})", zen_proto::head(e, 200)),
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

/// Create the kernel's own jobs if they aren't there (an install from before a job existed gets it
/// here). One whose name an agent job already has isn't created: logged, so the owner can rename theirs.
pub async fn seed(db: &PgPool) -> Result<()> {
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
    match sqlx::query(
        "WITH r AS (UPDATE job_runs SET status = 'interrupted', ended_at = now(), error = 'the kernel stopped during the run'
                    WHERE status = 'running' RETURNING job_id, session_id),
              s AS (UPDATE sessions SET archived = true WHERE id IN (SELECT session_id FROM r))
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
    deliver_pending(app).await?;
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
        let next = next_from(&sched, tz, due, now);
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
        // The database allows one running run per job: one still running (or started by the owner
        // just now) means this one is skipped, the next stays scheduled, and the tick goes on.
        let run: Option<i64> = sqlx::query_scalar("INSERT INTO job_runs (job_id, trigger, status) VALUES ($1, $2, 'running') ON CONFLICT DO NOTHING RETURNING id")
            .bind(job.id)
            .bind(trigger)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(run) = run {
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
async fn archive_session(db: &PgPool, run: i64) -> Result<()> {
    sqlx::query("UPDATE sessions SET archived = true WHERE id = (SELECT session_id FROM job_runs WHERE id = $1)").bind(run).execute(db).await?;
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

/// Record how a run ended; space out or pause a failing agent job. A run whose session read
/// untrusted content has its report marked (taint.rs): it reaches the next run and the main
/// session as untrusted content. Its session, however it ended, is archived (D-057).
///
/// Returns the session the report is to be posted into (D-060): the job's `post_to`, for a clean
/// `ok` run with a report. The intent is recorded (`pending_to`) together with the status, so the Matrix
/// bridge never posts a queued report in the jobs room too; `deliver_pending` records delivery. A marked run's report
/// stays in the jobs feed: posted into a session it would mark that session (D-053).
async fn finish(db: &PgPool, job: &Job, run: i64, o: &Outcome) -> Result<Option<Uuid>> {
    archive_session(db, run).await?;
    // By origin rating (D-065): rated 4 or more, or stopped (a source of it denied).
    let tainted: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT s.risk >= 4 OR s.tainted_at IS NOT NULL OR zen_session_contaminated(s.id) FROM job_runs r JOIN sessions s ON s.id = r.session_id WHERE r.id = $1), false)",
    )
    .bind(run)
    .fetch_one(db)
    .await?;
    let has_report = o.status == "ok" && o.output.as_deref().is_some_and(|t| !t.trim().is_empty());
    let target = job.post_to.filter(|_| has_report && !tainted);
    if job.post_to.is_some() && has_report && tainted {
        tracing::warn!("job `{}` run {run} read untrusted content: its report stays in the jobs feed, not posted into a session", job.name);
    }
    sqlx::query("UPDATE job_runs SET status = $2, ended_at = now(), output = $3, error = $4, tainted = $5, pending_to = $6 WHERE id = $1")
        .bind(run)
        .bind(o.status)
        .bind(&o.output)
        .bind(&o.error)
        .bind(tainted)
        .bind(target)
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
        return Ok(target);
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
        let turns = app.turns.lock().await;
        let mut tx = app.db.begin().await?;
        crate::sources::lock_provenance(&mut tx).await?;
        // Serialize with a denial of the run's session or any source of it (taint::deny takes
        // its cause's chain lock first): a deny either commits before the check below, so the
        // report is withheld, or waits until the delivery and its dependency edge commit, and
        // then stops the target session too. Locks in id order; the turns lock serializes
        // deliveries.
        let mut sources: Vec<Uuid> = sqlx::query_scalar(
            "WITH RECURSIVE edges AS (
                SELECT id AS reader, parent AS writer FROM sessions WHERE parent IS NOT NULL
                UNION SELECT reader, writer FROM session_inputs
             ), ancestry(id) AS (
                SELECT session_id FROM job_runs WHERE id = $1 AND session_id IS NOT NULL
                UNION SELECT e.writer FROM edges e JOIN ancestry a ON a.id = e.reader
             ) SELECT id FROM ancestry ORDER BY id",
        )
        .bind(run)
        .fetch_all(&mut *tx)
        .await?;
        sources.push(target);
        sources.sort();
        sources.dedup();
        for s in &sources {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 7))").bind(s).execute(&mut *tx).await?;
        }
        let row = sqlx::query(
            "SELECT r.output, r.session_id, r.risk, (r.risk>=4 OR zen_session_contaminated(r.session_id)) AS tainted, j.name FROM job_runs r JOIN jobs j ON j.id = r.job_id
             WHERE r.id = $1 AND r.pending_to = $2 AND r.status = 'ok' FOR UPDATE OF r",
        )
        .bind(run)
        .bind(target)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else { continue };
        let eligible: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sessions WHERE id = $1 AND kind IS NULL AND risk < 5 AND NOT zen_session_contaminated(id))")
            .bind(target)
            .fetch_one(&mut *tx)
            .await?;
        let too_late: bool = sqlx::query_scalar("SELECT ended_at < now() - interval '2 hours' FROM job_runs WHERE id = $1")
            .bind(run)
            .fetch_one(&mut *tx)
            .await?;
        if row.get::<bool, _>("tainted") || !eligible || too_late {
            sqlx::query("UPDATE job_runs SET pending_to = NULL, error = $2 WHERE id = $1")
                .bind(run)
                .bind("session delivery unavailable, expired or untrusted; report retained in the jobs feed")
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            continue;
        }
        if turns.contains_key(&target) {
            continue;
        }
        let job: String = row.get("name");
        let risk: i16 = row.get("risk");
        let report = crate::context::label(&format!("job {job} run {run}"), risk, &row.get::<Option<String>, _>("output").unwrap_or_default());
        sqlx::query("UPDATE sessions SET risk=GREATEST(risk,$2) WHERE id=$1").bind(target).bind(risk).execute(&mut *tx).await?;
        let message = json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": format!("⏰ *{job}*\n\n{}", report.trim()) }],
            "job": job, "job_run": run, "timestamp": Utc::now().timestamp_millis(),
        });
        // The target now holds the run's words: it depends on the run's session (D-062).
        if let Some(writer) = row.get::<Option<Uuid>, _>("session_id") {
            sqlx::query("INSERT INTO session_inputs (reader, writer) VALUES ($1, $2) ON CONFLICT DO NOTHING")
                .bind(target)
                .bind(writer)
                .execute(&mut *tx)
                .await?;
        }
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
            // What System One learned from the day's outcomes (s1/learn.rs).
            let learned = crate::s1::learn::nightly(&app.db).await.unwrap_or_else(|e| format!("System One learning failed: {e:#}"));
            Ok(Outcome { status: "ok", output: Some(format!("{line}; {learned}")), error: None })
        }
        Some("wiki") => Ok(upkeep(crate::wiki::tend().await)),
        Some("skills") => Ok(upkeep(crate::workshop::tend(&app.db).await)),
        Some("routes") => Ok(upkeep(crate::delegate::tune(&app.db).await)),
        Some("prune") => Ok(upkeep(prune_files(&crate::zen_home()))),
        Some("engines") => {
            let repo = PathBuf::from(&app.repo);
            let script = repo.join("scripts/update-engines.sh");
            anyhow::ensure!(script.exists(), "{} not found", script.display());
            let shell = tools::run_shell(&repo, &format!("bash {}", shell_quote(&script.display().to_string())), &[], tools::Sandbox::None, &[], Duration::from_secs(3600))
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

/// A nightly upkeep job's outcome: its findings, one per line, or `silent` when it found nothing.
fn upkeep(notes: Vec<String>) -> Outcome {
    if notes.is_empty() {
        Outcome { status: "silent", output: None, error: None }
    } else {
        Outcome { status: "ok", output: Some(notes.join("\n")), error: None }
    }
}

/// The system jobs whose findings go into the morning note.
const UPKEEP: [&str; 4] = ["wiki", "skills", "routes", "prune"];

/// What the nightly upkeep jobs found in the last day (each one's latest run that found something),
/// for a session's instructions next to the sleep's morning note. None when they found nothing.
pub async fn upkeep_note(db: &PgPool) -> Result<Option<String>, sqlx::Error> {
    let found: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT ON (j.action) r.output FROM job_runs r JOIN jobs j ON j.id = r.job_id
         WHERE j.kind = 'system' AND j.action = ANY($1) AND r.status = 'ok' AND r.output IS NOT NULL
           AND r.ended_at > now() - interval '1 day'
         ORDER BY j.action, r.id DESC",
    )
    .bind(UPKEEP.as_slice())
    .fetch_all(db)
    .await?;
    Ok((!found.is_empty()).then(|| format!("Last night's upkeep:\n{}", zen_proto::head(&found.join("\n"), 1500))))
}

/// Days a prompt file's backup is kept in `<zen home>/backups/prompt-files`.
const BACKUP_DAYS: u64 = 90;

/// Days a saved tool output (cut bash/MCP output, fetched PDFs) is kept in `<zen home>/outputs`.
const OUTPUT_DAYS: u64 = 30;

/// The `prune` job: old saved outputs, prompt-file backups, database dumps and eval runs under the
/// zen home `home`.
fn prune_files(home: &std::path::Path) -> Vec<String> {
    let mut notes = Vec::new();
    let backups = prune_backups(&home.join(crate::memory::BACKUPS), BACKUP_DAYS);
    if backups > 0 {
        notes.push(format!("backups: removed {backups} prompt-file backup(s) older than {BACKUP_DAYS} days"));
    }
    let outputs = prune_outputs(&home.join("outputs"), OUTPUT_DAYS);
    if outputs > 0 {
        notes.push(format!("outputs: removed {outputs} saved tool output file(s) older than {OUTPUT_DAYS} days"));
    }
    let dumps = prune_dumps(&home.join("backups"), DUMP_KEEP);
    if dumps > 0 {
        notes.push(format!("backups: removed {dumps} database dump(s), keeping the newest {DUMP_KEEP}"));
    }
    let evals = prune_eval_runs(&home.join("evals"), EVAL_DAYS, Utc::now());
    if evals > 0 {
        notes.push(format!("evals: removed {evals} eval run(s) older than {EVAL_DAYS} days"));
    }
    let tmps = prune_session_tmps(&std::env::temp_dir().join("zen-sandbox"), 7);
    if tmps > 0 {
        notes.push(format!("sandbox: removed {tmps} session /tmp folder(s) unused for 7 days"));
    }
    notes
}

/// Database dumps (`<zen home>/backups/<UTC time>-<label>.dump`, scripts/db.sh `db_backup`) kept.
/// The install takes one right before applying migrations, and a failed install's rollback names
/// that one (the newest) in its restore hint, so the newest few always stay.
const DUMP_KEEP: usize = 3;

/// Days an eval run (`<zen home>/evals/<UTC time>/`, scripts/eval.sh) is kept. The base results
/// it reuses live in `evals/cache` and the base builds in `evals/builds`; eval.sh prunes those.
const EVAL_DAYS: i64 = 7;

/// Whether `name` starts with the UTC time stamp the scripts name things with (`20261013T203126Z`),
/// which is returned.
fn stamp(name: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(name.get(..16)?, "%Y%m%dT%H%M%SZ").ok()
}

/// Remove all but the newest `keep` database dumps directly in `dir` (by the time stamp in their
/// name); returns how many. Only `<stamp>-<label>.dump` regular files count; anything else stays.
fn prune_dumps(dir: &std::path::Path, keep: usize) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let mut dumps: Vec<(NaiveDateTime, PathBuf)> = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let at = stamp(&name).filter(|_| name.as_bytes().get(16) == Some(&b'-') && name.ends_with(".dump"))?;
            Some((at, e.path()))
        })
        .collect();
    dumps.sort_by(|a, b| b.cmp(a));
    dumps.into_iter().skip(keep.max(1)).filter(|(_, p)| std::fs::remove_file(p).is_ok()).count()
}

/// Remove eval run directories directly in `dir` named exactly by a UTC time stamp more than `days`
/// days before `now`; returns how many. `builds`, `cache` and anything else stay.
fn prune_eval_runs(dir: &std::path::Path, days: i64, now: DateTime<Utc>) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let limit = now.naive_utc() - chrono::Duration::days(days);
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.len() == 16 && stamp(&name).is_some_and(|at| at < limit)
        })
        .filter(|e| std::fs::remove_dir_all(e.path()).is_ok())
        .count()
}

/// Session /tmp folders of contained shells (`tools::session_tmp`, named by session id) not touched
/// for `days` days.
fn prune_session_tmps(dir: &std::path::Path, days: u64) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let limit = std::time::Duration::from_secs(days * 86_400);
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && e.file_name().to_string_lossy().parse::<Uuid>().is_ok())
        .filter(|e| e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > limit))
        .filter(|e| std::fs::remove_dir_all(e.path()).is_ok())
        .count()
}

/// Remove regular files directly in `dir` whose name `ours` accepts and that weren't modified for
/// `days` days; returns how many. Anything else there is left alone.
fn prune(dir: &std::path::Path, days: u64, ours: impl Fn(&str) -> bool) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let limit = std::time::Duration::from_secs(days * 86_400);
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && ours(&e.file_name().to_string_lossy()))
        .filter(|e| e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > limit))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// Prompt-file backups (`*.md`) older than `days` days.
fn prune_backups(dir: &std::path::Path, days: u64) -> usize {
    prune(dir, days, |n| n.ends_with(".md"))
}

/// Saved outputs older than `days` days: only files the kernel saves (`*.log`, `mcp-*.txt`,
/// `web-*.pdf`).
fn prune_outputs(dir: &std::path::Path, days: u64) -> usize {
    prune(dir, days, |n| n.ends_with(".log") || (n.starts_with("mcp-") && n.ends_with(".txt")) || (n.starts_with("web-") && n.ends_with(".pdf")))
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
/// the job note. Like any session's, only memories, prompt and project files and skills rated
/// 0–1 by origin get in, and the session depends on their writers (D-062); a skill the job names
/// that isn't admitted fails the run rather than running without it.
async fn job_instructions(app: &AppState, job: &Job, workspace: &std::path::Path, session: Uuid) -> Result<String> {
    let parts: Vec<&str> = job.context.iter().map(String::as_str).collect();
    let memory = crate::memory::render(&app.db, session).await?;
    let skills_all = crate::skills::scan(&crate::skills::root());
    let index = crate::skills::instruction_index(&app.db, session).await?;
    let admitted = crate::compile::admitted(&app.db, session, workspace).await?;
    let mut s = crate::compile::compose(&parts, workspace, &app.repo, &memory, None, &index, &admitted);
    for name in &job.skills {
        let skill = crate::skills::lookup(&skills_all, name).map_err(|e| anyhow::anyhow!(e))?;
        anyhow::ensure!(
            crate::context::admit(&app.db, session, &skill.text, 1).await?.is_some(),
            "the skill {}/{} isn't loaded into a job's instructions: its current version was written by a session that read outside content, or one the owner stopped",
            skill.domain, skill.name
        );
        let text = crate::skills::load(skill, None).map_err(|e| anyhow::anyhow!(e))?;
        s.push_str(&format!("\n\n<skill name=\"{}/{}\">\n{}\n</skill>", skill.domain, skill.name, text.trim_end()));
    }
    s.push_str("\n\n");
    s.push_str(&job_note(job));
    Ok(s)
}

/// The last report of `job` before `run`, for the run's session `id` to tell what's new, at its
/// origin rating (D-062): wrapped when rated 2 or more, and a report whose run was stopped stops
/// this one (an error). The run depends on the session that wrote it, recorded before the report
/// is handed over.
async fn last_report(app: &AppState, job: &Job, run: i64, id: Uuid) -> Result<Option<String>> {
    let last: Option<(Option<String>, i16, Option<Uuid>)> = sqlx::query_as("SELECT output, CASE WHEN zen_session_contaminated(session_id) THEN 5::smallint ELSE risk END, session_id FROM job_runs WHERE job_id = $1 AND id <> $2 AND status = 'ok' ORDER BY started_at DESC LIMIT 1")
        .bind(job.id)
        .bind(run)
        .fetch_optional(&app.db)
        .await?;
    let Some((Some(l), risk, writer)) = last else { return Ok(None) };
    if risk < 5 {
        crate::sources::depend(&app.db, id, &writer.into_iter().collect::<Vec<_>>()).await?;
    }
    Ok(Some(crate::taint::hand_over_rating(app, id, risk, "job report", &format!("the previous run of {}", job.name), &zen_proto::head(&l, 3000)).await?))
}

/// An agent job: a fresh session with the job's instructions and its prompt; the final answer is
/// the report.
async fn run_agent(app: &AppState, job: &Job, run: i64) -> Result<Outcome> {
    let model = job.model.clone().unwrap_or_else(|| app.default_model.clone());
    let approved: bool = sqlx::query_scalar("SELECT owner_approved FROM jobs WHERE id=$1").bind(job.id).fetch_one(&app.db).await?;
    let inferred = if approved && job.workspace.is_none() { inferred_workspace(job) } else { None };
    let workspace = workspace_of(app, job.workspace.as_deref().or(inferred.as_deref()));
    // A report file older than this wasn't written by this run.
    let started = std::time::SystemTime::now();
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
    let base = job_instructions(app, job, &workspace, id).await?;
    anyhow::ensure!(!crate::taint::stopped(&app.db, id).await?, "instructions withheld: a source of them was stopped by the owner");
    tape::append(&app.db, id, "base", &json!({ "text": base })).await?;
    let mut prompt = job.prompt.clone().unwrap_or_default();
    if let Some(report) = last_report(app, job, run, id).await? {
        prompt.push_str(&format!("\n\n<last_report>\nThe report from this job's previous run, to tell what's new:\n{report}\n</last_report>"));
    }
    crate::turns::run_and_wait(app, id, JOB, &prompt).await?;
    wait_for_owner(app, id).await?;
    anyhow::ensure!(!crate::taint::stopped(&app.db, id).await?, "job stopped by owner denial");
    let error: Option<String> = sqlx::query_scalar("SELECT error FROM turns WHERE session_id = $1 ORDER BY started_at DESC LIMIT 1").bind(id).fetch_optional(&app.db).await?.flatten();
    let answer = crate::delegate::final_answer(&app.db, id).await;
    let trimmed = answer.trim();
    if let Some(e) = error {
        return Ok(Outcome { status: "error", output: (!trimmed.is_empty()).then(|| trimmed.to_string()), error: Some(e) });
    }
    if trimmed.is_empty() && job.report_file.is_none() {
        return Ok(Outcome { status: "error", output: None, error: Some("the run ended without a report".into()) });
    }
    if trimmed.starts_with("[SILENT]") {
        return Ok(Outcome { status: "silent", output: None, error: None });
    }
    if let Some(why) = trimmed.strip_prefix("[FAILED]") {
        return Ok(Outcome { status: "error", output: Some(trimmed.to_string()), error: Some(zen_proto::head(why.trim(), 500)) });
    }
    let Some(file) = &job.report_file else {
        return Ok(Outcome { status: "ok", output: Some(trimmed.to_string()), error: None });
    };
    // The report is the file, as the run left it; the answer stays in the session. Never the answer
    // instead: whatever is wrong with the file makes the run an error.
    let secrets = crate::secrets::secret_files();
    let (path, started) = (tools::resolve(&workspace, file), started);
    let report = tokio::task::spawn_blocking(move || read_report(&workspace, &path, started, &secrets)).await?;
    // The file's exact bytes keep their origin (D-062): the run depends on whoever wrote them and
    // takes their rating, so the stored report (rated by its run's session) carries it too.
    // Bytes no session registered were written during this run, so they are the run's own.
    let report = match report {
        Ok(Some(raw)) => match report_rating(app, id, &raw).await {
            Ok(()) => Ok(Some(raw.trim_end().to_string())),
            Err(e) => Err(e),
        },
        other => other,
    };
    Ok(match report {
        Ok(Some(text)) => Outcome { status: "ok", output: Some(crate::secrets::mask_off_thread(text).await), error: None },
        Ok(None) => Outcome { status: "silent", output: None, error: None },
        Err(e) => Outcome { status: "error", output: None, error: Some(format!("{e:#}")) },
    })
}

/// Rate a report file's exact bytes for the run's session `id`: depend on their writers and raise
/// the session to their rating. Written by a stopped session: an error, never the report.
async fn report_rating(app: &AppState, id: Uuid, raw: &str) -> Result<()> {
    let (risk, writers) = crate::context::rate(&app.db, raw, 1).await?;
    anyhow::ensure!(risk < 5, "the report file was written by a session the owner stopped; it isn't posted");
    crate::sources::depend(&app.db, id, &writers).await?;
    crate::sources::raise(&app.db, id, risk).await?;
    anyhow::ensure!(!crate::taint::stopped(&app.db, id).await?, "report withheld: a writer was stopped during the read");
    Ok(())
}

/// The directory a job works in: its own (relative to the kernel's workspace), else the workspace.
/// An approved job's unambiguous existing folder, else the report file's parent. Explicit workspace
/// always wins. Never infer authority from model output or a report from an earlier run.
fn inferred_workspace(job: &Job) -> Option<String> {
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
    parent.is_absolute().then(|| parent.display().to_string())
}

fn workspace_of(app: &AppState, dir: Option<&str>) -> PathBuf {
    dir.map(|w| tools::resolve(&app.workspace, w)).unwrap_or_else(|| app.workspace.clone())
}

/// The largest report file a run may leave (D-055).
const REPORT_MAX: u64 = 60 * 1024;

/// `path` with the part of it that exists resolved (symlinks, `..`), and the rest appended: where a
/// file not written yet would really be. None when the part that doesn't exist ends in `..`.
fn real_path(path: &std::path::Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(real) = cur.canonicalize() {
            return Some(rest.iter().rev().fold(real, |p, c| p.join(c)));
        }
        rest.push(cur.file_name()?.to_os_string());
        cur = cur.parent()?.to_path_buf();
    }
}

/// Why `real` (a resolved path) may not be a job's report file, if it may not: it must lie inside
/// the workspace and not be (or be in) a secret file (secrets.rs, the guard a verifier's `read` has).
fn report_place(workspace: &std::path::Path, real: &std::path::Path, secrets: &[PathBuf]) -> Result<()> {
    let root = workspace.canonicalize().with_context(|| format!("the job's workspace {} can't be read", workspace.display()))?;
    anyhow::ensure!(real.starts_with(&root) && real != root, "a report file lies inside the job's workspace ({}); {} doesn't", root.display(), real.display());
    anyhow::ensure!(!secrets.iter().filter_map(|f| f.canonicalize().ok()).any(|f| real.starts_with(f)), "a secret file can't be a job's report");
    Ok(())
}

/// A report file checked when it's set: where it would really be is inside the job's workspace and
/// isn't a secret file. Returns the absolute path that's stored (it's checked again at each run).
fn checked_report_file(workspace: &std::path::Path, file: &str) -> Result<String> {
    let path = tools::resolve(workspace, file);
    let real = real_path(&path).with_context(|| format!("the report file {} can't be resolved", path.display()))?;
    report_place(workspace, &real, &crate::secrets::secret_files())?;
    Ok(path.display().to_string())
}

/// The report a run left in its job's report file (D-055): None when the file is empty (the run is
/// `silent`). Fails closed, so the run becomes an error with the reason: missing, a symlink or not a
/// regular file, outside the workspace or a secret file (judged on the file actually opened, through
/// `/proc/self/fd`, so a path swapped after a check can't point elsewhere), last modified before the
/// run started, over 60 KB, or not UTF-8.
fn read_report(workspace: &std::path::Path, path: &std::path::Path, started: std::time::SystemTime, secrets: &[PathBuf]) -> Result<Option<String>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let shown = path.display();
    // No symlink as the last part (O_NOFOLLOW), and a FIFO put there can't hold the kernel (O_NONBLOCK).
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow::anyhow!("the run didn't write its report file {shown}"),
        _ if e.raw_os_error() == Some(libc::ELOOP) => anyhow::anyhow!("the report file {shown} is a symlink; it must be a regular file"),
        _ => anyhow::anyhow!("the report file {shown} can't be opened: {e}"),
    })?;
    let meta = file.metadata()?;
    anyhow::ensure!(meta.is_file(), "the report file {shown} isn't a regular file");
    anyhow::ensure!(meta.nlink() == 1, "the report file {shown} is hardlinked; it must have a single link");
    let real = std::fs::read_link(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&file))).context("can't tell where the report file really is")?;
    report_place(workspace, &real, secrets)?;
    anyhow::ensure!(meta.modified()? >= started, "the report file {shown} wasn't written by this run (it's older than the run)");
    anyhow::ensure!(meta.len() <= REPORT_MAX, "the report file {shown} has {} bytes; a report is at most 60 KB", meta.len());
    let mut bytes = Vec::new();
    (&file).take(REPORT_MAX + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    anyhow::ensure!(
        (meta.len(), meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec(), meta.nlink())
            == (after.len(), after.mtime(), after.mtime_nsec(), after.ctime(), after.ctime_nsec(), after.nlink()),
        "the report file {shown} changed while it was read"
    );
    anyhow::ensure!(bytes.len() as u64 <= REPORT_MAX, "the report file {shown} grew past 60 KB while it was read");
    let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("the report file {shown} isn't UTF-8 text"))?;
    // Exact bytes, as read: their rating is looked up by digest before trimming.
    Ok((!text.trim().is_empty()).then_some(text))
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
`context` picks the instructions it gets besides the soul: identity, agents, user, memory, skills, project (default: user, memory). \
`report_file`: when a script builds the report and it must reach the owner exactly, the run writes it to this file (inside the job's \
workspace, written during the run, UTF-8, at most 60 KB; empty means nothing to report) and the kernel posts the file instead of \
the final answer; anything wrong with the file makes the run fail. \
`post_to`: a session its reports go into as your message, where the owner can answer (`this` for this session, or a session id); \
otherwise they go to the jobs feed.",
        "parameters": { "type": "object", "properties": {
            "action": { "type": "string", "enum": ["create", "update", "list", "runs", "pause", "resume", "remove", "run"] },
            "name": { "type": "string", "description": "The job: lowercase letters, digits and hyphens (e.g. morning-brief)" },
            "prompt": { "type": "string", "description": "create/update: the task, self-contained (the run sees nothing of this conversation)" },
            "schedule": { "type": "string", "description": "create/update: when it runs" },
            "tz": { "type": "string", "description": "Optional IANA timezone (default America/Sao_Paulo)" },
            "context": { "type": "array", "items": { "type": "string" }, "description": "Optional: instruction parts besides the soul" },
            "skills": { "type": "array", "items": { "type": "string" }, "description": "Optional: skills loaded into its instructions (domain/name)" },
            "dir": { "type": "string", "description": "Optional: the directory it works in (default: the workspace)" },
            "report_file": { "type": "string", "description": "Optional: the file the run writes its report to, posted as is instead of the final answer (\"\" clears it)" },
            "post_to": { "type": "string", "description": "Optional: the session its reports are posted into: `this`, or a session id (\"\" clears it)" } },
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

async fn tool(app: &AppState, session: Uuid, args: &Value, owner_allowed: bool) -> Result<String> {
    // Only dispatch can supply this after consuming an audited exact owner grant.
    let by = if owner_allowed { By::Owner } else { By::Agent(session) };
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
            // Each report at its origin rating (D-062): wrapped when rated 2 or more, after the
            // reader's dependency on the run's session is recorded. A stopped run's report is
            // left out of a listing (it would stop the reader for a list of runs).
            let mut lines = Vec::new();
            for r in &rs {
                let body = r["output"].as_str().or(r["error"].as_str()).map(|t| zen_proto::head(t, 600)).unwrap_or_default();
                let job = r["job"].as_str().unwrap_or("");
                let risk = r["risk"].as_i64().map(|x| x as i16).unwrap_or(5);
                let body = if risk >= 5 {
                    "[report withheld: the owner stopped the session that wrote it, or a source of it]".to_string()
                } else {
                    let writer: Vec<Uuid> = r["session"].as_str().and_then(|s| s.parse().ok()).into_iter().collect();
                    crate::sources::depend(&app.db, session, &writer).await?;
                    crate::taint::hand_over_rating(app, session, risk, "job report", &format!("a run of {job}"), &body).await?
                };
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
pub async fn run_tool(app: &AppState, session: Uuid, name: &str, args: &Value, owner_allowed: bool) -> Option<tools::ToolOutput> {
    if name != "schedule" {
        return None;
    }
    Some(match tool(app, session, args, owner_allowed).await {
        Ok(content) => tools::ToolOutput { rating: None, content, is_error: false },
        Err(e) => tools::ToolOutput { rating: None, content: format!("schedule failed: {e:#}"), is_error: true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "SOURCE_TEST_DB_URL must name a disposable database"]
    async fn owner_job_authority_database() {
        let db = PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap()).await.unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        let app = crate::test_util::app(db.clone());
        let session = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model,kind) VALUES($1,'faux/smoke','job')").bind(session).execute(&db).await.unwrap();
        let name = format!("job-test-{}", &session.to_string()[..8]);
        let spec = Spec {name:Some(name.clone()), prompt:Some("fetch https://liturgiadiaria.edicoescnbb.com.br/ and https://alice.github.io/".into()), schedule:Some("every 1h".into()), ..Default::default()};
        let (job, _) = create(&app, spec, By::Owner).await.unwrap();
        sqlx::query("INSERT INTO job_runs(job_id,trigger,status,session_id) VALUES($1,'owner','running',$2)")
            .bind(job.id).bind(session).execute(&db).await.unwrap();
        assert!(names_host(&db,session,"api-liturgia.edicoescnbb.com.br").await);
        assert!(!names_host(&db,session,"bob.github.io").await);
        let change = Spec {prompt:Some("fetch https://attacker.test/".into()), ..Default::default()};
        update(&app, &name, change, By::Agent(session)).await.unwrap();
        let approved: bool = sqlx::query_scalar("SELECT owner_approved FROM jobs WHERE id=$1").bind(job.id).fetch_one(&db).await.unwrap();
        assert!(!approved,"an agent edit cannot retain its earlier owner approval");
        assert!(!names_host(&db,session,"attacker.test").await);
        db.close().await;
    }

    #[test]
    fn registrable_domains() {
        assert_eq!(registrable("api-liturgia.edicoescnbb.com.br"), "edicoescnbb.com.br");
        assert_eq!(registrable("liturgiadiaria.edicoescnbb.com.br"), "edicoescnbb.com.br");
        assert_eq!(registrable("www.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(registrable("docs.rs"), "docs.rs");
        assert_ne!(registrable("alice.github.io"), registrable("bob.github.io"));
        assert_eq!(registrable("a.city.kawasaki.jp"), "city.kawasaki.jp");
        assert_eq!(registrable("a.b.ck"), "a.b.ck");
        assert_eq!(registrable("a.www.ck"), "www.ck");
        assert_eq!(registrable("127.0.0.1"), "127.0.0.1");
        assert_eq!(registrable("api.github.com"), "github.com");
        assert_eq!(registrable("Evil.edicoescnbb.com.br.attacker.io"), "attacker.io");
    }

    #[test]
    fn old_saved_outputs_are_pruned_and_others_kept() {
        let dir = crate::test_util::TestDir::new("outputs");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86_400);
        for name in ["1.log", "mcp-a.txt", "web-a.pdf", "notes.md", "fresh.log"] {
            std::fs::write(dir.join(name), "x").unwrap();
            if name != "fresh.log" {
                std::fs::File::options().write(true).open(dir.join(name)).unwrap().set_modified(old).unwrap();
            }
        }
        assert_eq!(prune_outputs(&dir, 30), 3);
        assert!(dir.join("notes.md").exists() && dir.join("fresh.log").exists());
    }

    #[test]
    fn only_the_newest_database_dumps_are_kept() {
        let dir = crate::test_util::TestDir::new("dumps");
        let names = [
            "20261007T133742Z-c9945db.dump",
            "20261009T164854Z-b78e535.dump",
            "20261013T004556Z-26f9477.dump",
            "20261013T203736Z-282d1b3.dump",
            "20261013T221423Z-manual.dump",
            "old.dump",
            "20261001T000000Z-x.tgz",
            "env.bak",
        ];
        for name in names {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        std::fs::create_dir(dir.join("20261001T000000Z-dir.dump")).unwrap();
        assert_eq!(prune_dumps(&dir, 3), 2);
        assert!(!dir.join("20261007T133742Z-c9945db.dump").exists() && !dir.join("20261009T164854Z-b78e535.dump").exists());
        for kept in &names[2..] {
            assert!(dir.join(kept).exists(), "{kept}");
        }
        assert!(dir.join("20261001T000000Z-dir.dump").is_dir());
        // The newest always stays, even when asked to keep none.
        assert_eq!(prune_dumps(&dir, 0), 2);
        assert!(dir.join("20261013T221423Z-manual.dump").exists());
    }

    #[test]
    fn old_eval_runs_are_pruned_and_builds_kept() {
        let dir = crate::test_util::TestDir::new("evals");
        for name in ["20261001T000000Z", "20261006T235959Z", "20261008T000001Z", "builds", "cache", "20261001T000000Z-x"] {
            std::fs::create_dir_all(dir.join(name).join("work")).unwrap();
        }
        std::fs::write(dir.join("20261002T000000Z"), "a file, not a run").unwrap();
        assert_eq!(prune_eval_runs(&dir, 7, t("2026-10-14T00:00:00Z")), 2);
        assert!(!dir.join("20261001T000000Z").exists() && !dir.join("20261006T235959Z").exists());
        for kept in ["20261008T000001Z", "builds", "cache", "20261001T000000Z-x", "20261002T000000Z"] {
            assert!(dir.join(kept).exists(), "{kept}");
        }
    }

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
    fn huge_durations_are_errors_not_panics() {
        let now = t("2026-10-09T12:00:00Z");
        for text in ["every 9999999999999d", "in 9999999999999d", "every 9223372036854775807s", "in 99999999999h", "every 300000000d"] {
            assert!(parse_schedule(text, Tz::UTC, now).is_err(), "{text}");
        }
        // A valid duration that takes the date past what a date can hold.
        assert!(parse_schedule("in 100000000d", Tz::UTC, now).is_err());
        assert!(parse_schedule("every 100000000d", Tz::UTC, now).is_err());
    }

    #[test]
    fn the_shortest_gap_of_a_cron_counts_toward_the_minimum() {
        let now = t("2026-10-09T12:00:00Z");
        let (s, _) = parse_schedule("0,30-35 * * * *", Tz::UTC, now).unwrap();
        assert_eq!(period(&s, Tz::UTC, now), Some(chrono::Duration::minutes(1)), "not the 30 minutes between the first two");
        assert!(checked_schedule("0,30-35 * * * *", Tz::UTC, now).is_err());
        assert!(checked_schedule("0,30 * * * *", Tz::UTC, now).is_ok());
    }

    #[test]
    fn an_interval_keeps_its_rhythm_from_the_due_time() {
        let due = t("2026-10-09T12:00:00Z");
        let (s, _) = parse_schedule("every 1h", Tz::UTC, due).unwrap();
        // Seen 20 seconds late: the next run is still on the hour.
        assert_eq!(next_from(&s, Tz::UTC, due, t("2026-10-09T12:00:20Z")), Some(t("2026-10-09T13:00:00Z")));
        // Seen three and a half hours late: the runs it passed are skipped, the rhythm kept.
        assert_eq!(next_from(&s, Tz::UTC, due, t("2026-10-09T15:30:00Z")), Some(t("2026-10-09T16:00:00Z")));
        // Exactly on a later beat: strictly after now.
        assert_eq!(next_from(&s, Tz::UTC, due, t("2026-10-09T14:00:00Z")), Some(t("2026-10-09T15:00:00Z")));
        let (c, _) = parse_schedule("0 * * * *", Tz::UTC, due).unwrap();
        assert_eq!(next_from(&c, Tz::UTC, due, t("2026-10-09T12:00:20Z")), Some(t("2026-10-09T13:00:00Z")));
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
    fn a_report_file_is_read_only_when_it_is_safe_and_fresh() {
        let dir = crate::test_util::TestDir::new("report-file");
        let ws = dir.join("ws");
        std::fs::create_dir_all(ws.join("out")).unwrap();
        let secret = ws.join("token");
        std::fs::write(&secret, "s3cret").unwrap();
        std::fs::write(dir.join("outside.md"), "elsewhere").unwrap();
        let secrets = vec![secret.clone()];
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::write(ws.join("out/stale.md"), "yesterday").unwrap();
        std::fs::File::options().write(true).open(ws.join("out/stale.md")).unwrap().set_modified(old).unwrap();
        let started = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        let read = |p: &str| read_report(&ws, &ws.join(p), started, &secrets);
        std::fs::write(ws.join("out/message.md"), "Leitura I\n\nPalavra do Senhor.\n\n").unwrap();
        assert_eq!(read("out/message.md").unwrap().as_deref(), Some("Leitura I\n\nPalavra do Senhor.\n\n"), "the exact bytes (trimmed after rating)");
        std::fs::hard_link(ws.join("out/message.md"), ws.join("out/hardlink.md")).unwrap();
        assert!(read("out/hardlink.md").unwrap_err().to_string().contains("hardlinked"));
        std::fs::remove_file(ws.join("out/hardlink.md")).unwrap();
        std::fs::write(ws.join("out/empty.md"), " \n").unwrap();
        assert_eq!(read("out/empty.md").unwrap(), None, "empty means silent");
        let err = |p: &str| format!("{:#}", read(p).unwrap_err());
        assert!(err("out/missing.md").contains("didn't write"));
        assert!(err("out/stale.md").contains("older than the run"));
        std::os::unix::fs::symlink(dir.join("outside.md"), ws.join("out/link.md")).unwrap();
        assert!(err("out/link.md").contains("symlink"));
        std::os::unix::fs::symlink(&*dir, ws.join("up")).unwrap();
        assert!(err("up/outside.md").contains("inside the job's workspace"), "a folder symlink out is judged by where the file is");
        assert!(err("token").contains("secret"));
        std::fs::write(ws.join("out/big.md"), "x".repeat(REPORT_MAX as usize + 1)).unwrap();
        assert!(err("out/big.md").contains("at most 60 KB"));
        std::fs::write(ws.join("out/bin.md"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(err("out/bin.md").contains("UTF-8"));
        assert!(err("out").contains("regular file"));
    }

    #[test]
    fn a_report_file_is_set_only_inside_the_workspace() {
        let dir = crate::test_util::TestDir::new("report-set");
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let stored = checked_report_file(&ws, "out/message.md").unwrap();
        assert_eq!(stored, ws.join("out/message.md").display().to_string(), "stored absolute, before it exists");
        assert!(checked_report_file(&ws, "../elsewhere.md").is_err());
        assert!(checked_report_file(&ws, "/tmp/message.md").is_err());
        assert!(checked_report_file(&ws, "out/../../x.md").is_err());
        assert!(checked_report_file(&ws, ".").is_err(), "not the workspace itself");
        std::os::unix::fs::symlink("/tmp", ws.join("tmp")).unwrap();
        assert!(checked_report_file(&ws, "tmp/message.md").is_err(), "a symlink out of the workspace is outside");
    }

    #[test]
    fn context_always_has_the_soul_and_only_known_parts() {
        assert_eq!(context_of(None).unwrap(), ["soul", "user", "memory"]);
        assert_eq!(context_of(Some(vec!["memory".into(), "Soul".into()])).unwrap(), ["soul", "memory"]);
        assert!(context_of(Some(vec!["secrets".into()])).is_err());
        assert!(valid_name("morning-brief").is_ok() && valid_name("Morning Brief").is_err() && valid_name("-x").is_err() && valid_name("x-").is_err());
    }

    /// Job reports keep their origin (D-062): `<last_report>`, `schedule runs`, report files and
    /// delivery into the owner's session. SOURCE_TEST_DB_URL must name a throwaway database.
    #[tokio::test]
    #[ignore]
    async fn job_report_provenance_database() {
        let db = PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap()).await.unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        let app = crate::test_util::app(db.clone());
        let session = |risk: i16, kind: Option<&'static str>| {
            let db = db.clone();
            async move {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO sessions(id,title,model,risk,kind) VALUES($1,'job provenance','faux/smoke',$2,$3)")
                    .bind(id).bind(risk).bind(kind).execute(&db).await.unwrap();
                id
            }
        };
        let deny = |cause: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query("INSERT INTO contamination(cause,affected) VALUES($1,$1) ON CONFLICT(cause,affected) DO UPDATE SET cleared_at=NULL")
                    .bind(cause).execute(&db).await.unwrap();
            }
        };
        let name = format!("prov-{}", &Uuid::new_v4().to_string()[..8]);
        sqlx::query("INSERT INTO jobs(name,kind,prompt,schedule,tz,created_by) VALUES($1,'agent','p','every 1h','UTC','owner')")
            .bind(&name).execute(&db).await.unwrap();
        let job = get(&db, &name).await.unwrap();
        let run_row = |writer: Uuid, output: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, i64>("INSERT INTO job_runs(job_id,trigger,status,session_id,output,ended_at) VALUES($1,'owner','ok',$2,$3,now()) RETURNING id")
                    .bind(job.id).bind(writer).bind(output).fetch_one(&db).await.unwrap()
            }
        };
        let inputs = |reader: Uuid, writer: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM session_inputs WHERE reader=$1 AND writer=$2")
                    .bind(reader).bind(writer).fetch_one(&db).await.unwrap()
            }
        };
        let risk_of = |id: Uuid| {
            let db = db.clone();
            async move { sqlx::query_scalar::<_, i16>("SELECT risk FROM sessions WHERE id=$1").bind(id).fetch_one(&db).await.unwrap() }
        };

        // A level-4 report reaches the next run wrapped, raises it, and records the dependency.
        let writer4 = session(4, Some(JOB)).await;
        run_row(writer4, "PREVIOUS REPORT four").await;
        let next = session(1, Some(JOB)).await;
        let current = run_row(next, "running").await;
        let shown = last_report(&app, &job, current, next).await.unwrap().unwrap();
        assert!(shown.starts_with("<untrusted source=\"job report\"") && shown.contains("PREVIOUS REPORT four"));
        assert_eq!(risk_of(next).await, 4);
        assert_eq!(inputs(next, writer4).await, 1);

        // `schedule runs` wraps it too and raises the reader; a stopped run's report is withheld
        // from the listing without stopping the reader.
        let reader = session(1, None).await;
        let listed = tool(&app, reader, &json!({"action":"runs","name":name}), false).await.unwrap();
        assert!(listed.contains("<untrusted source=\"job report\"") && listed.contains("PREVIOUS REPORT four"));
        assert_eq!(risk_of(reader).await, 4);
        assert_eq!(inputs(reader, writer4).await, 1);
        let denied = session(1, Some(JOB)).await;
        run_row(denied, "STOPPED REPORT").await;
        deny(denied).await;
        let reader2 = session(1, None).await;
        let listed = tool(&app, reader2, &json!({"action":"runs","name":name}), false).await.unwrap();
        assert!(!listed.contains("STOPPED REPORT") && listed.contains("report withheld"));
        assert!(!crate::taint::stopped(&db, reader2).await.unwrap());

        // The newest report is the stopped one: handing it to the next run stops that run.
        let after = session(1, Some(JOB)).await;
        let current = run_row(after, "running").await;
        assert!(last_report(&app, &job, current, after).await.is_err());
        assert!(crate::taint::stopped(&db, after).await.unwrap());

        // A report file keeps its writer's rating; one a stopped session wrote is an error.
        let file_writer = session(4, None).await;
        let dir = crate::test_util::TestDir::new("report-origin");
        let raw = format!("report bytes {}\n", Uuid::new_v4());
        crate::sources::record_bytes(&db, file_writer, &dir.join("r.md"), &raw, None).await.unwrap();
        let run_session = session(1, Some(JOB)).await;
        report_rating(&app, run_session, &raw).await.unwrap();
        assert_eq!(risk_of(run_session).await, 4, "the run takes the file's rating");
        assert_eq!(inputs(run_session, file_writer).await, 1);
        deny(file_writer).await;
        let run2 = session(1, Some(JOB)).await;
        assert!(report_rating(&app, run2, &raw).await.is_err(), "a stopped writer's file is never the report");
        let unknown = format!("written during the run {}", Uuid::new_v4());
        let run3 = session(1, Some(JOB)).await;
        report_rating(&app, run3, &unknown).await.unwrap();
        assert_eq!(risk_of(run3).await, 1, "unregistered bytes are the run's own");

        // Delivery: a run denied while queued is withheld; a delivered report ties the target
        // to the run, so a later denial stops the target too.
        let target = session(1, None).await;
        let queued_writer = session(1, Some(JOB)).await;
        let queued = run_row(queued_writer, "QUEUED REPORT").await;
        sqlx::query("UPDATE job_runs SET pending_to=$2 WHERE id=$1").bind(queued).bind(target).execute(&db).await.unwrap();
        deny(queued_writer).await;
        deliver_pending(&app).await.unwrap();
        let (pending, posted): (Option<Uuid>, Option<Uuid>) = sqlx::query_as("SELECT pending_to, posted_to FROM job_runs WHERE id=$1").bind(queued).fetch_one(&db).await.unwrap();
        assert_eq!((pending, posted), (None, None), "withheld, not posted");
        let messages = |id: Uuid| {
            let db = db.clone();
            async move { sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tape_events WHERE session_id=$1 AND kind='message'").bind(id).fetch_one(&db).await.unwrap() }
        };
        assert_eq!(messages(target).await, 0);
        let clean_writer = session(1, Some(JOB)).await;
        let clean = run_row(clean_writer, "CLEAN REPORT").await;
        sqlx::query("UPDATE job_runs SET pending_to=$2 WHERE id=$1").bind(clean).bind(target).execute(&db).await.unwrap();
        deliver_pending(&app).await.unwrap();
        let posted: Option<Uuid> = sqlx::query_scalar("SELECT posted_to FROM job_runs WHERE id=$1").bind(clean).fetch_one(&db).await.unwrap();
        assert_eq!(posted, Some(target));
        assert_eq!(messages(target).await, 1);
        assert_eq!(inputs(target, clean_writer).await, 1);
        assert!(!crate::taint::stopped(&db, target).await.unwrap());
        deny(clean_writer).await;
        assert!(crate::taint::stopped(&db, target).await.unwrap(), "a later denial of the run reaches the session it posted into");

        // A stopped target must not receive a clean report either.
        let stopped_target = session(1, None).await;
        crate::taint::deny(&app, stopped_target, "test").await.unwrap();
        let clean_writer = session(1, Some(JOB)).await;
        let withheld = run_row(clean_writer, "CLEAN TO STOPPED TARGET").await;
        sqlx::query("UPDATE job_runs SET pending_to=$2 WHERE id=$1").bind(withheld).bind(stopped_target).execute(&db).await.unwrap();
        deliver_pending(&app).await.unwrap();
        assert_eq!(messages(stopped_target).await, 0);

        // Force the deny-before-delivery boundary while delivery is in flight. The global
        // provenance lock keeps both ancestry and the handover stable until denial commits.
        let blocked_writer = session(1, Some(JOB)).await;
        let blocked_target = session(1, None).await;
        let blocked_run = run_row(blocked_writer, "BLOCKED REPORT").await;
        sqlx::query("UPDATE job_runs SET pending_to=$2 WHERE id=$1").bind(blocked_run).bind(blocked_target).execute(&db).await.unwrap();
        let mut barrier = db.begin().await.unwrap();
        crate::sources::lock_provenance(&mut barrier).await.unwrap();
        let delivery = deliver_pending(&app);
        tokio::pin!(delivery);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut delivery).await.is_err());
        sqlx::query("INSERT INTO contamination(cause,affected) VALUES($1,$1)").bind(blocked_writer).execute(&mut *barrier).await.unwrap();
        barrier.commit().await.unwrap();
        delivery.await.unwrap();
        assert_eq!(messages(blocked_target).await, 0);

        // Real concurrent delivery/deny: either withheld, or delivered before denial and the
        // recorded edge stops the target. Repeat for both a run and an upstream writer.
        for upstream in [false, true] {
            for _ in 0..4 {
                let target = session(1, None).await;
                let writer = session(1, Some(JOB)).await;
                let cause = if upstream {
                    let source = session(1, None).await;
                    crate::sources::depend(&db, writer, &[source]).await.unwrap();
                    source
                } else { writer };
                let queued = run_row(writer, "RACING REPORT").await;
                sqlx::query("UPDATE job_runs SET pending_to=$2 WHERE id=$1").bind(queued).bind(target).execute(&db).await.unwrap();
                let (delivered, denied) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    tokio::join!(deliver_pending(&app), crate::taint::deny(&app, cause, "race test"))
                }).await.expect("delivery and denial must not deadlock");
                delivered.unwrap();
                denied.unwrap();
                let posted: Option<Uuid> = sqlx::query_scalar("SELECT posted_to FROM job_runs WHERE id=$1").bind(queued).fetch_one(&db).await.unwrap();
                if posted.is_some() {
                    assert!(crate::taint::stopped(&db, target).await.unwrap());
                    assert_eq!(inputs(target, writer).await, 1);
                } else {
                    assert_eq!(messages(target).await, 0);
                }
            }
        }

        // Every numeric tier survives the report-file path; no reduction to a risky boolean.
        for tier in 0..=4i16 {
            let writer = session(tier, None).await;
            let raw = format!("tier {tier} {}", Uuid::new_v4());
            crate::sources::record_bytes(&db, writer, &dir.join("tier.md"), &raw, None).await.unwrap();
            let reader = session(0, Some(JOB)).await;
            report_rating(&app, reader, &raw).await.unwrap();
            assert_eq!(risk_of(reader).await, tier.max(1));
            assert_eq!(inputs(reader, writer).await, 1);
        }

        // Metadata errors fail closed.
        let closed = PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap()).await.unwrap();
        closed.close().await;
        let broken = crate::test_util::app(closed);
        assert!(report_rating(&broken, run3, &unknown).await.is_err());
        assert!(last_report(&broken, &job, current, after).await.is_err());
    }

}
