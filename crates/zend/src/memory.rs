//! Short-term memory and its nightly sleep (DESIGN.md, "Memory and knowledge"; D-028).
//!
//! Anything can be saved to short-term memory with the `remember` tool. At a session's start the
//! short-term entries are rendered into `MEMORY.md` inside the session's fixed instructions, so a
//! write shows from the next session and the prompt cache holds. The space is fixed
//! (`ZEN_MEMORY_CHARS`), so entries compete for it: the sleep (nightly, or at once when writes pass
//! the hard ceiling) ranks them, keeps what fits, archives the rest, and promotes to long-term only
//! really impactful memories: System One must judge them durable and impactful with a very high
//! probability on the lower end of several samples, and their source must be the owner's words or a
//! verified result. Without a System One model the sleep keeps the most recent entries and promotes
//! nothing. Nothing is ever deleted. While long-term memory has no reader (`ZEN_MEMORY_PROMOTE`
//! unset or `shadow`), promotions are only proposed.

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::App;

/// The size of MEMORY.md the sleep tidies to, in characters (ZEN_MEMORY_CHARS, default 4000).
pub fn cap() -> usize {
    crate::env_num("ZEN_MEMORY_CHARS", 4000.0).max(200.0) as usize
}

/// The hard limit (twice the size): reaching it starts a sleep at once, and `add` is refused
/// while memory is over it. Memory is never cut silently (Hermes refuses writes when full).
fn ceiling() -> usize {
    cap() * 2
}

/// The bar a memory must clear on every sample to reach long-term (ZEN_MEMORY_PROMOTE_BAR, 0.95).
fn promote_bar() -> f64 {
    crate::env_num("ZEN_MEMORY_PROMOTE_BAR", 0.95)
}

/// Whether promotion moves memories to long-term (`on`) or only proposes it (`shadow`, the
/// default until long-term memory is searchable).
fn promote_on() -> bool {
    std::env::var("ZEN_MEMORY_PROMOTE").is_ok_and(|v| v.trim() == "on")
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub text: String,
    pub source: String,
    pub age_days: f64,
    pub idle_days: f64,
    pub uses: i32,
}

fn line(e: &Entry) -> String {
    format!("- [m{}] {}\n", e.id, e.text.replace('\n', " "))
}

async fn short_entries(db: &PgPool) -> Result<Vec<Entry>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, text, source, uses,
                (EXTRACT(EPOCH FROM now() - created_at) / 86400)::float8 AS age,
                (EXTRACT(EPOCH FROM now() - GREATEST(updated_at, COALESCE(used_at, updated_at))) / 86400)::float8 AS idle
         FROM memories WHERE tier = 'short' ORDER BY created_at, id",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Entry {
            id: r.get("id"),
            text: r.get("text"),
            source: r.get("source"),
            age_days: r.get::<Option<f64>, _>("age").unwrap_or(0.0),
            idle_days: r.get::<Option<f64>, _>("idle").unwrap_or(0.0),
            uses: r.get("uses"),
        })
        .collect())
}

/// Short-term memory as it goes into a session's instructions: every entry (writes may take it
/// past its size until the sleep tidies it, never past the hard limit, where only the newest that
/// fit are shown with a count of the rest).
pub async fn render(db: &PgPool) -> Result<String, sqlx::Error> {
    let entries = short_entries(db).await?;
    Ok(render_entries(&entries, ceiling()))
}

fn render_entries(entries: &[Entry], cap: usize) -> String {
    let total: usize = entries.iter().map(|e| line(e).len()).sum();
    if total <= cap {
        return entries.iter().map(line).collect();
    }
    // Newest first until full, then shown in their original order.
    let mut kept: Vec<&Entry> = Vec::new();
    let mut size = 0;
    for e in entries.iter().rev() {
        let l = line(e).len();
        if size + l > cap {
            continue;
        }
        size += l;
        kept.push(e);
    }
    kept.reverse();
    let mut out: String = kept.iter().map(|e| line(e)).collect();
    out.push_str(&format!("({} older entries are left out until tonight's sleep tidies memory.)\n", entries.len() - kept.len()));
    out
}

/// Write short-term memory to `~/.zenbot/MEMORY.md`, for the owner to read (own your data).
pub async fn export(db: &PgPool) {
    let path = crate::zen_home().join("MEMORY.md");
    match short_entries(db).await {
        Ok(entries) => {
            let body: String = entries.iter().map(line).collect();
            let text = format!(
                "# MEMORY.md\n\nzenbot's short-term memory ({} of {} characters). Written by the kernel; edit it with the `remember` tool, not here.\n\n{body}",
                body.len(),
                cap()
            );
            if let Err(e) = std::fs::write(&path, text) {
                tracing::warn!("writing {}: {e}", path.display());
            }
        }
        Err(e) => tracing::warn!("exporting memory: {e}"),
    }
}

pub fn spec() -> Value {
    json!({
        "name": "remember",
        "description": "Save something to your short-term memory, which every new session starts with (MEMORY.md in your \
instructions; changes show from the next session). Save what will matter again: the owner's preferences and decisions, \
facts about their projects, lessons from mistakes, where things are. Write facts (\"The owner prefers X\"), not orders to \
yourself. Not for what's in the code, the docs or git history, or only matters to this conversation. Memory has a fixed size and entries compete for it: a nightly sleep keeps the most \
useful, archives the rest, and promotes the few that matter for good. Keep entries short and self-contained. \
Use `replace` to update an entry (by its id, e.g. m12) rather than adding a near-duplicate, and `remove` for one that's wrong. \
Mark `source`: owner for the owner's own words, verified for a result you checked, inferred otherwise.",
        "parameters": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["add", "replace", "remove"], "description": "add (default), replace or remove" },
                "text": { "type": "string", "description": "The memory, one or two sentences (add, replace)" },
                "id": { "type": "string", "description": "The entry to replace or remove, e.g. m12" },
                "source": { "type": "string", "enum": ["owner", "verified", "inferred"], "description": "Where it comes from (default inferred)" }
            }
        }
    })
}

fn parse_id(v: &Value) -> Option<i64> {
    v.as_str().map(|s| s.trim().trim_start_matches(['m', 'M'])).and_then(|s| s.parse().ok()).or_else(|| v.as_i64())
}

async fn used_chars(db: &PgPool) -> i64 {
    sqlx::query_scalar::<_, Option<i64>>("SELECT SUM(length(text) + 10)::bigint FROM memories WHERE tier = 'short'")
        .fetch_one(db)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// Run the `remember` tool.
pub async fn run_tool(app: &crate::AppState, session: Uuid, args: &Value) -> (String, bool) {
    match remember(app, session, args).await {
        Ok(text) => (text, false),
        Err(e) => (e.to_string(), true),
    }
}

async fn remember(app: &crate::AppState, session: Uuid, args: &Value) -> Result<String> {
    let db = &app.db;
    let action = args["action"].as_str().unwrap_or("add");
    let source = match args["source"].as_str().unwrap_or("inferred") {
        s @ ("owner" | "verified" | "inferred") => s,
        other => anyhow::bail!("source must be owner, verified or inferred, not `{other}`"),
    };
    // A session that read web content can't vouch for what it saves: web text could have put it
    // there, so it never counts as the owner's words or a checked result (and can't be promoted).
    let source = if source != "inferred" && crate::web::tainted(db, session).await { "inferred" } else { source };
    let text = args["text"].as_str().map(str::trim).unwrap_or("");
    let text = crate::secrets::mask(text);
    let done = match action {
        "add" => {
            anyhow::ensure!(!text.is_empty(), "add needs `text`");
            anyhow::ensure!(text.len() <= 600, "keep a memory under 600 characters; split it or say it shorter");
            let used = used_chars(db).await as usize;
            if used + text.len() + 10 > ceiling() {
                start_sleep(app, "ceiling");
                anyhow::bail!(
                    "Memory is full ({used}/{} characters). Free room first: replace or remove entries by id (they are in your \
instructions), or leave it; it is being tidied now. Then carry on with the owner's request.",
                    ceiling()
                );
            }
            let id: i64 = sqlx::query_scalar("INSERT INTO memories (text, source, session_id) VALUES ($1, $2, $3) RETURNING id")
                .bind(&text)
                .bind(source)
                .bind(session)
                .fetch_one(db)
                .await?;
            format!("Saved as m{id}.")
        }
        "replace" => {
            let id = parse_id(&args["id"]).ok_or_else(|| anyhow::anyhow!("replace needs the entry's `id`, e.g. m12"))?;
            anyhow::ensure!(!text.is_empty(), "replace needs the new `text`");
            anyhow::ensure!(text.len() <= 600, "keep a memory under 600 characters");
            let n = sqlx::query("UPDATE memories SET text = $2, source = $3, updated_at = now(), used_at = now(), uses = uses + 1 WHERE id = $1 AND tier = 'short'")
                .bind(id)
                .bind(&text)
                .bind(source)
                .execute(db)
                .await?
                .rows_affected();
            anyhow::ensure!(n == 1, "no short-term memory m{id}");
            format!("Replaced m{id}.")
        }
        "remove" => {
            let id = parse_id(&args["id"]).ok_or_else(|| anyhow::anyhow!("remove needs the entry's `id`, e.g. m12"))?;
            let n = sqlx::query("UPDATE memories SET tier = 'archived', reason = 'removed by the agent', updated_at = now() WHERE id = $1 AND tier = 'short'")
                .bind(id)
                .execute(db)
                .await?
                .rows_affected();
            anyhow::ensure!(n == 1, "no short-term memory m{id}");
            format!("Removed m{id} (archived, not deleted).")
        }
        other => anyhow::bail!("action must be add, replace or remove, not `{other}`"),
    };
    export(db).await;
    let used = used_chars(db).await;
    Ok(format!("{done} Memory: {used}/{} characters (tidied nightly to {}); it shows in your instructions from the next session.", ceiling(), cap()))
}

/// Start a sleep in the background, unless one is running.
fn start_sleep(app: &crate::AppState, trigger: &'static str) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = sleep(&app, trigger).await {
            tracing::error!("memory sleep ({trigger}): {e:#}");
        }
        RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

// ---------- sleep ----------

/// What System One said about an entry.
#[derive(Clone, Debug, Default)]
pub struct Judged {
    /// Likely need in the coming days, 0..1.
    pub needed: f64,
    pub durable: f64,
    /// How much it changes how the agent should act across jobs, 0..1.
    pub impact: f64,
    pub covered: f64,
    pub about_owner: f64,
}

/// The questions the sleep asks about each entry (System One, `s1.decide`).
pub fn questions() -> Value {
    json!({
        "needed": { "type": "score", "instructions": "How likely is the agent to need this memory in its work over the coming days?",
            "criteria": ["Unlikely: a one-off detail", "Possibly", "Likely", "Almost certainly: it comes up all the time"] },
        "durable": { "type": "bool", "instructions": "Will this memory still be true and useful months from now?",
            "criteria": { "true": "A lasting fact, preference, decision or lesson", "false": "About a passing task, state or moment" } },
        "impact": { "type": "score", "instructions": "How much does this memory change how the agent should act for the owner, across many jobs?",
            "criteria": ["Not at all", "A little, in rare cases", "Noticeably, in some kinds of work", "A lot, in most of its work"] },
        "covered": { "type": "bool", "instructions": "Is this memory already covered by another entry or by the owner's profile (other_memories, user_profile)?",
            "criteria": { "true": "Another entry or the profile already says the same", "false": "It says something new" } },
        "about_owner": { "type": "bool", "instructions": "Is this memory about the owner as a person: who they are, their preferences, their context?",
            "criteria": { "true": "About the owner", "false": "About work, projects, tools or the world" } }
    })
}

fn score01(a: &Value) -> Option<f64> {
    // A score answer is a level index (0..3) with a confidence; a bool is a probability.
    a["probability"].as_f64().or_else(|| a["score"].as_f64().map(|s| (s / 3.0).clamp(0.0, 1.0)))
}

fn judged_from(answers: &Value) -> Option<Judged> {
    Some(Judged {
        needed: score01(&answers["needed"])?,
        durable: score01(&answers["durable"])?,
        impact: score01(&answers["impact"])?,
        covered: score01(&answers["covered"]).unwrap_or(0.0),
        about_owner: score01(&answers["about_owner"]).unwrap_or(0.0),
    })
}

/// What the sleep does with one entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Fate {
    Keep,
    Drop(&'static str),
    Promote,
}

/// The sleep's ranking: likely need, nudged by recency and use; the owner's own words and verified
/// results rank a little higher than inferences. Without judgments, recency alone.
pub fn priority(e: &Entry, j: Option<&Judged>) -> f64 {
    let recency = 1.0 / (1.0 + e.idle_days / 7.0);
    let source = match e.source.as_str() {
        "owner" => 0.1,
        "verified" => 0.05,
        _ => 0.0,
    };
    let use_bonus = (e.uses as f64).min(5.0) * 0.02;
    match j {
        Some(j) => j.needed * 0.7 + recency * 0.2 + source + use_bonus,
        None => recency + source + use_bonus,
    }
}

/// Decide every entry's fate: covered entries go; the rest are ranked and kept while they fit in
/// `cap`; of those that don't fit, entries promoted are the ones whose `lower` bound (the lowest of
/// several samples) of durable and impact clears `bar` and whose source is the owner or a check.
pub fn plan(entries: &[Entry], judged: &[Option<Judged>], lower: &[Option<(f64, f64)>], cap: usize, bar: f64) -> Vec<Fate> {
    let mut fates = vec![Fate::Keep; entries.len()];
    let mut order: Vec<usize> = (0..entries.len()).collect();
    for (i, j) in judged.iter().enumerate() {
        if j.as_ref().is_some_and(|j| j.covered >= 0.8) {
            fates[i] = Fate::Drop("already covered");
        }
    }
    order.retain(|&i| fates[i] == Fate::Keep);
    order.sort_by(|&a, &b| priority(&entries[b], judged[b].as_ref()).partial_cmp(&priority(&entries[a], judged[a].as_ref())).unwrap_or(std::cmp::Ordering::Equal));
    let mut size = 0;
    for &i in &order {
        let l = line(&entries[i]).len();
        let promotable = matches!(entries[i].source.as_str(), "owner" | "verified") && lower[i].is_some_and(|(d, m)| d >= bar && m >= bar);
        if promotable {
            fates[i] = Fate::Promote;
        } else if size + l <= cap {
            size += l;
        } else {
            fates[i] = Fate::Drop("didn't fit");
        }
    }
    fates
}

/// The material System One judges an entry by: the entry, the other entries and the owner's
/// profile (to tell whether it's covered).
fn state_for(e: &Entry, all: &[Entry], user_md: &str) -> Value {
    let others: Vec<String> = all.iter().filter(|o| o.id != e.id).map(|o| zen_proto::head(&o.text, 300)).collect();
    json!({
        "memory": e.text,
        "source": e.source,
        "age_days": (e.age_days * 10.0).round() / 10.0,
        "days_since_used": (e.idle_days * 10.0).round() / 10.0,
        "other_memories": others,
        "user_profile": zen_proto::head(user_md, 4000),
    })
}

/// Tidy short-term memory: rank, keep what fits, archive the rest, promote (or propose) the few
/// that clear the bar. Records a `sleep_runs` row and one `decisions` row per entry.
pub async fn sleep(app: &App, trigger: &str) -> Result<Value> {
    // One sleep at a time: a second one (the timer while a ceiling sleep runs) waits, then finds
    // memory already tidied.
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    let db = &app.db;
    let run: i64 = sqlx::query_scalar("INSERT INTO sleep_runs (trigger, scorer) VALUES ($1, $2) RETURNING id")
        .bind(trigger)
        .bind(crate::score::scorer().filter(|_| crate::score::private_ok()))
        .fetch_one(db)
        .await?;
    let res = sleep_inner(app, run).await;
    if let Err(e) = &res {
        sqlx::query("UPDATE sleep_runs SET ended_at = now(), error = $2 WHERE id = $1").bind(run).bind(format!("{e:#}")).execute(db).await?;
    }
    export(db).await;
    res
}

async fn sleep_inner(app: &App, run: i64) -> Result<Value> {
    let db = &app.db;
    let entries = short_entries(db).await?;
    let user_md = std::fs::read_to_string(crate::zen_home().join("USER.md")).unwrap_or_default();
    // Memories are private content: System One sees them only when allowed (ZEN_S1_PRIVATE).
    let scorer = crate::score::scorer().filter(|_| crate::score::private_ok());
    let mut judged: Vec<Option<Judged>> = vec![None; entries.len()];
    let mut raw: Vec<Value> = vec![Value::Null; entries.len()];
    let mut lower: Vec<Option<(f64, f64)>> = vec![None; entries.len()];
    if scorer.is_some() && !entries.is_empty() {
        let qs = questions();
        for (i, e) in entries.iter().enumerate() {
            let state = state_for(e, &entries, &user_md);
            match crate::score::decide(app, &state, &qs).await {
                Ok(v) if v["error"].is_null() => {
                    judged[i] = judged_from(&v["answers"]);
                    raw[i] = v["answers"].clone();
                }
                Ok(v) => tracing::warn!("sleep: judging m{}: {}", e.id, v["error"]),
                Err(e2) => tracing::warn!("sleep: judging m{}: {e2:#}", e.id),
            }
            // The lower end of the confidence interval: candidates are asked twice more, and the
            // lowest of the three answers counts.
            if let Some(j) = &judged[i] {
                let bar = promote_bar();
                if j.durable >= bar && j.impact >= bar && matches!(e.source.as_str(), "owner" | "verified") {
                    let (mut d, mut m) = (j.durable, j.impact);
                    let repeat = json!({ "durable": qs["durable"], "impact": qs["impact"] });
                    for _ in 0..2 {
                        match crate::score::decide(app, &state, &repeat).await {
                            Ok(v) if v["error"].is_null() => {
                                d = d.min(score01(&v["answers"]["durable"]).unwrap_or(0.0));
                                m = m.min(score01(&v["answers"]["impact"]).unwrap_or(0.0));
                            }
                            _ => {
                                d = 0.0;
                            }
                        }
                    }
                    lower[i] = Some((d, m));
                }
            }
        }
    }
    let fates = plan(&entries, &judged, &lower, cap(), promote_bar());
    let (mut kept, mut dropped, mut promoted, mut proposed) = (0, 0, 0, 0);
    let mut notes: Vec<String> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let user_fact = judged[i].as_ref().is_some_and(|j| j.about_owner >= 0.8 && j.durable >= 0.8);
        let scores = if raw[i].is_null() { None } else { Some(json!({ "answers": raw[i], "lower": lower[i].map(|(d, m)| json!({ "durable": d, "impact": m })) })) };
        let (tier, reason, proposal, chosen) = match &fates[i] {
            Fate::Keep => {
                kept += 1;
                ("short", None, user_fact.then_some("user"), "keep")
            }
            Fate::Drop(why) => {
                dropped += 1;
                ("archived", Some(*why), user_fact.then_some("user"), "drop")
            }
            Fate::Promote if promote_on() => {
                promoted += 1;
                notes.push(format!("promoted m{}: {}", e.id, zen_proto::head(&e.text, 120)));
                ("long", Some("promoted by the sleep"), user_fact.then_some("user"), "promote")
            }
            Fate::Promote => {
                proposed += 1;
                notes.push(format!("would promote m{}: {}", e.id, zen_proto::head(&e.text, 120)));
                ("archived", Some("proposed for long-term (shadow)"), Some("promote"), "promote")
            }
        };
        if proposal == Some("user") {
            notes.push(format!("about the owner, for USER.md: m{}: {}", e.id, zen_proto::head(&e.text, 120)));
        }
        sqlx::query("UPDATE memories SET tier = $2, reason = COALESCE($3, reason), proposed = COALESCE($4, proposed), scores = COALESCE($5, scores), updated_at = CASE WHEN tier = $2 THEN updated_at ELSE now() END WHERE id = $1")
            .bind(e.id)
            .bind(tier)
            .bind(reason)
            .bind(proposal)
            .bind(&scores)
            .execute(db)
            .await?;
        let p = judged[i].as_ref().map(|j| if chosen == "promote" { lower[i].map(|(d, m)| d.min(m)).unwrap_or(j.durable) } else { j.needed });
        sqlx::query("INSERT INTO decisions (point, model, input, answer, chosen, probability, acted) VALUES ('sleep', $1, $2, $3, $4, $5, $6)")
            .bind(&scorer)
            .bind(json!({ "memory": e.id, "text": e.text, "source": e.source }))
            .bind(&raw[i])
            .bind(chosen)
            .bind(p)
            .bind(chosen != "promote" || promote_on())
            .execute(db)
            .await?;
    }
    let note = if notes.is_empty() { None } else { Some(notes.join("\n")) };
    sqlx::query("UPDATE sleep_runs SET ended_at = now(), entries = $2, kept = $3, dropped = $4, promoted = $5, proposed = $6, note = $7 WHERE id = $1")
        .bind(run)
        .bind(entries.len() as i32)
        .bind(kept)
        .bind(dropped)
        .bind(promoted)
        .bind(proposed)
        .bind(&note)
        .execute(db)
        .await?;
    Ok(json!({ "run": run, "entries": entries.len(), "kept": kept, "dropped": dropped, "promoted": promoted, "proposed": proposed, "scorer": scorer, "note": note }))
}

/// A line about the last sleep for a session's instructions, when it ran in the last day (the
/// "morning note": what was kept, archived and proposed, so the agent knows its memory changed).
pub async fn morning_note(db: &PgPool) -> Result<Option<String>, sqlx::Error> {
    let r = sqlx::query(
        "SELECT ended_at, entries, kept, dropped, promoted, proposed, note FROM sleep_runs
         WHERE ended_at > now() - interval '1 day' AND error IS NULL ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(r.map(|r| {
        let n = |k: &str| r.get::<Option<i32>, _>(k).unwrap_or(0);
        let when = r.get::<chrono::DateTime<chrono::Utc>, _>("ended_at").format("%Y-%m-%d %H:%M UTC");
        let mut line = format!(
            "Last sleep ({when}): {} entries, {} kept, {} archived, {} promoted, {} proposed for long-term.",
            n("entries"),
            n("kept"),
            n("dropped"),
            n("promoted"),
            n("proposed")
        );
        if let Some(note) = r.get::<Option<String>, _>("note") {
            line.push_str(&format!("\n{}", zen_proto::head(&note, 1500)));
        }
        line
    }))
}

/// The latest sleep, for `zen status` and `zen memory`.
pub async fn last_run(db: &PgPool) -> Result<Value, sqlx::Error> {
    let r = sqlx::query("SELECT id, started_at, ended_at, trigger, scorer, entries, kept, dropped, promoted, proposed, note, error FROM sleep_runs ORDER BY id DESC LIMIT 1")
        .fetch_optional(db)
        .await?;
    Ok(r.map(|r| {
        json!({
            "id": r.get::<i64, _>("id"), "started_at": r.get::<chrono::DateTime<chrono::Utc>, _>("started_at"),
            "ended_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("ended_at"), "trigger": r.get::<String, _>("trigger"),
            "scorer": r.get::<Option<String>, _>("scorer"), "entries": r.get::<Option<i32>, _>("entries"), "kept": r.get::<Option<i32>, _>("kept"),
            "dropped": r.get::<Option<i32>, _>("dropped"), "promoted": r.get::<Option<i32>, _>("promoted"), "proposed": r.get::<Option<i32>, _>("proposed"),
            "note": r.get::<Option<String>, _>("note"), "error": r.get::<Option<String>, _>("error"),
        })
    })
    .unwrap_or(Value::Null))
}

/// Memories by tier, newest first, for `zen memory`.
pub async fn list(db: &PgPool, tier: &str) -> Result<Value, sqlx::Error> {
    let rows = sqlx::query("SELECT id, text, source, tier, proposed, reason, created_at, updated_at FROM memories WHERE $1 = 'all' OR tier = $1 ORDER BY id DESC LIMIT 500")
        .bind(tier)
        .fetch_all(db)
        .await?;
    Ok(Value::Array(
        rows.iter()
            .map(|r| {
                json!({ "id": format!("m{}", r.get::<i64, _>("id")), "text": r.get::<String, _>("text"), "source": r.get::<String, _>("source"),
                        "tier": r.get::<String, _>("tier"), "proposed": r.get::<Option<String>, _>("proposed"), "reason": r.get::<Option<String>, _>("reason"),
                        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at") })
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: i64, text: &str, source: &str, idle: f64) -> Entry {
        Entry { id, text: text.into(), source: source.into(), age_days: idle, idle_days: idle, uses: 0 }
    }

    #[test]
    fn render_keeps_newest_entries_when_over_the_size() {
        let all = vec![e(1, "old one", "inferred", 9.0), e(2, "middle", "inferred", 5.0), e(3, "newest", "owner", 0.0)];
        assert_eq!(render_entries(&all, 1000), "- [m1] old one\n- [m2] middle\n- [m3] newest\n");
        let short = render_entries(&all, 30);
        assert!(short.contains("[m3] newest") && !short.contains("old one"));
        assert!(short.contains("1 older entries are left out"));
    }

    #[test]
    fn plan_drops_covered_keeps_by_need_and_promotes_only_proven_sourced_entries() {
        let all = vec![
            e(1, "duplicate of the profile", "owner", 1.0),
            e(2, "rarely needed detail", "inferred", 1.0),
            e(3, "often needed fact", "inferred", 1.0),
            e(4, "lasting owner rule", "owner", 1.0),
            e(5, "lasting inferred rule", "inferred", 1.0),
        ];
        let j = |needed: f64, covered: f64| Some(Judged { needed, durable: 0.5, impact: 0.5, covered, about_owner: 0.0 });
        let judged = vec![j(0.9, 0.9), j(0.1, 0.0), j(0.9, 0.0), j(0.5, 0.0), j(0.5, 0.0)];
        // m4 and m5 cleared the bar on every sample; only m4 has a source that may be promoted.
        let lower = vec![None, None, None, Some((0.97, 0.96)), Some((0.99, 0.99))];
        let room = line(&all[2]).len() + line(&all[4]).len();
        let fates = plan(&all, &judged, &lower, room, 0.95);
        assert_eq!(fates, vec![Fate::Drop("already covered"), Fate::Drop("didn't fit"), Fate::Keep, Fate::Promote, Fate::Keep]);
        // One sample below the bar is enough to stay out of long-term memory.
        let fates = plan(&all, &judged, &[None, None, None, Some((0.97, 0.90)), None], 10_000, 0.95);
        assert_eq!(fates[3], Fate::Keep);
    }

    #[test]
    fn without_judgments_recency_decides() {
        let all = vec![e(1, "stale", "inferred", 30.0), e(2, "fresh", "inferred", 0.0)];
        let fates = plan(&all, &[None, None], &[None, None], line(&all[1]).len(), 0.95);
        assert_eq!(fates, vec![Fate::Drop("didn't fit"), Fate::Keep]);
    }

    #[test]
    fn ids_parse_with_or_without_the_prefix() {
        assert_eq!(parse_id(&json!("m12")), Some(12));
        assert_eq!(parse_id(&json!("12")), Some(12));
        assert_eq!(parse_id(&json!(7)), Some(7));
        assert_eq!(parse_id(&json!("x")), None);
    }

    #[test]
    fn answers_map_to_probabilities() {
        let j = judged_from(&json!({ "needed": { "score": 3, "confidence": 0.9 }, "durable": { "probability": 0.7 }, "impact": { "score": 0 } })).unwrap();
        assert_eq!((j.needed, j.durable, j.impact, j.covered), (1.0, 0.7, 0.0, 0.0));
        assert!(judged_from(&json!({})).is_none());
    }
}
