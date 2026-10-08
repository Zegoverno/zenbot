//! Short-term memory and its nightly sleep (DESIGN.md, "Memory and knowledge"; D-028).
//!
//! Anything can be saved to short-term memory with the `remember` tool. At a session's start the
//! short-term entries are rendered into `MEMORY.md` inside the session's fixed instructions, so a
//! write shows from the next session and the prompt cache holds. The space is fixed
//! (`ZEN_MEMORY_CHARS`), so entries compete for it: the sleep (nightly, or at once when writes pass
//! the hard ceiling) ranks them, keeps what fits and archives the rest. Without a System One model
//! the sleep keeps the most recent entries. Nothing is ever deleted.
//!
//! Promotion out of short-term memory (D-045, automatic): a lasting entry about the owner moves to
//! `USER.md`, lasting guidance on how the agent acts moves to `IDENTITY.md` (each under `## Learned`,
//! after a dated backup in `<zen home>/backups/prompt-files/`), and lasting knowledge is copied into the wiki
//! (the entry stays while it's still needed). Only the owner's words and verified results reach the
//! prompt files. There is no long-term tier any more (old `long` rows stay searchable).

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::AppState;

/// The size of MEMORY.md the sleep tidies to, in characters (ZEN_MEMORY_CHARS, default 4000).
pub fn cap() -> usize {
    crate::env_num("ZEN_MEMORY_CHARS", 4000.0).max(200.0) as usize
}

/// The hard limit (twice the size): reaching it starts a sleep at once, and `add` is refused
/// while memory is over it. Memory is never cut silently (Hermes refuses writes when full).
fn ceiling() -> usize {
    cap() * 2
}

/// The lower end of the one-sided Wilson score interval for `k` successes in `n` trials (model
/// routing's evidence, delegate.rs).
pub fn wilson_lower(k: u64, n: u64, z: f64) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let (n, p) = (n as f64, k as f64 / n as f64);
    let z2 = z * z;
    ((p + z2 / (2.0 * n)) - z * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt()) / (1.0 + z2 / n)
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub text: String,
    pub source: String,
    pub age_days: f64,
    pub idle_days: f64,
    pub uses: i32,
    /// Already copied into the wiki by an earlier sleep.
    pub captured: bool,
}

fn line(e: &Entry) -> String {
    format!("- [m{}] {}\n", e.id, e.text.replace('\n', " "))
}

async fn short_entries(db: &PgPool) -> Result<Vec<Entry>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, text, source, uses, proposed IS NOT DISTINCT FROM 'wiki' AS captured,
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
            captured: r.get("captured"),
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
    let total: usize = entries.iter().map(|e| line(e).chars().count()).sum();
    if total <= cap {
        return entries.iter().map(line).collect();
    }
    // Newest first until full, then shown in their original order.
    let mut kept: Vec<&Entry> = Vec::new();
    let mut size = 0;
    for e in entries.iter().rev() {
        let l = line(e).chars().count();
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

/// Write short-term memory to `~/.zenbot/global/MEMORY.md`, for the owner to read (own your data).
pub async fn export(db: &PgPool) {
    let path = crate::layout::global_dir().join("MEMORY.md");
    match short_entries(db).await {
        Ok(entries) => {
            let body: String = entries.iter().map(line).collect();
            let text = format!(
                "# MEMORY.md\n\nzenbot's short-term memory ({} of {} characters). Written by the kernel; edit it with the `remember` tool, not here.\n\n{body}",
                body.chars().count(),
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
        "description": "Save where things stand to your short-term memory, which every new session starts with (it shows from the next session): the owner's decisions, open questions, the state of their projects and jobs, where things are; not what's in the code, docs or git. Not traits, guidance or preferences (how you should act, what the owner likes): when one is really important and lasting, write it straight into IDENTITY.md (about you) or USER.md (about the owner), keeping the file compact; when it isn't, leave it out. The nightly sleep also promotes lasting entries there and into the wiki. Write facts, not orders to yourself. Space is fixed and tidied nightly: `replace` an entry by id (m12) rather than adding a near-duplicate; `remove` one that's wrong. `source`: owner (their words), verified (you checked it) or inferred (default).",
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
    // there, so it never counts as the owner's words or a checked result.
    let source = if source != "inferred" && crate::taint::tainted(db, session).await { "inferred" } else { source };
    let text = args["text"].as_str().map(str::trim).unwrap_or("");
    let text = crate::secrets::mask(text);
    let done = match action {
        "add" => {
            anyhow::ensure!(!text.is_empty(), "add needs `text`");
            anyhow::ensure!(text.chars().count() <= 600, "keep a memory under 600 characters; split it or say it shorter");
            let used = used_chars(db).await as usize;
            if used + text.chars().count() + 10 > ceiling() {
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
            anyhow::ensure!(text.chars().count() <= 600, "keep a memory under 600 characters");
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
    // Reset even if the sleep panics, so later ceiling sleeps aren't blocked until a restart.
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let app = app.clone();
    tokio::spawn(async move {
        let _done = Done;
        if let Err(e) = sleep(&app, trigger).await {
            tracing::error!("memory sleep ({trigger}): {e:#}");
        }
    });
}

// ---------- sleep ----------

/// What System One said about an entry.
#[derive(Clone, Debug, Default)]
pub struct Judged {
    /// Likely need in the coming days, 0..1.
    pub needed: f64,
    pub durable: f64,
    pub covered: f64,
    pub about_owner: f64,
    /// A trait, standing guidance or preference: how the agent should act, rather than where things stand.
    pub guidance: f64,
    /// Lasting knowledge worth keeping for good (a decision and why, how something works, a lesson).
    pub knowledge: f64,
}

/// The questions the sleep asks about each entry (System One via `score.rs`).
pub fn questions() -> Value {
    json!({
        "needed": { "type": "score", "instructions": "How likely is the agent to need this memory in its work over the coming days?",
            "criteria": ["Unlikely: a one-off detail", "Possibly", "Likely", "Almost certainly: it comes up all the time"] },
        "durable": { "type": "bool", "instructions": "Will this memory still be true and useful months from now?",
            "criteria": { "true": "A lasting fact, preference, decision or lesson", "false": "About a passing task, state or moment" } },
        "covered": { "type": "bool", "instructions": "Is this memory already covered by another entry, the owner's profile or the agent's identity (other_memories, user_profile, agent_identity)?",
            "criteria": { "true": "One of them already says the same", "false": "It says something new" } },
        "about_owner": { "type": "bool", "instructions": "Is this memory about the owner as a person: who they are, their preferences, their context?",
            "criteria": { "true": "About the owner", "false": "About work, projects, tools or the world" } },
        "guidance": { "type": "bool", "instructions": "Is this memory a trait, standing guidance or preference about how the agent should act, rather than a fact about work or where things stand?",
            "criteria": { "true": "Guidance on how to act (e.g. how to answer, what to check, what to avoid)", "false": "A fact, a state, a decision or where something is" } },
        "knowledge": { "type": "bool", "instructions": "Is this memory lasting knowledge worth keeping for good in notes: a decision and why it was made, how something works, a lesson, a project's history? Not an open question, a to-do or a passing state.",
            "criteria": { "true": "Lasting knowledge", "false": "A passing state, an open question or a to-do" } }
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
        covered: score01(&answers["covered"]).unwrap_or(0.0),
        about_owner: score01(&answers["about_owner"]).unwrap_or(0.0),
        guidance: score01(&answers["guidance"]).unwrap_or(0.0),
        knowledge: score01(&answers["knowledge"]).unwrap_or(0.0),
    })
}

/// What the sleep does with one entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Fate {
    Keep,
    Drop(&'static str),
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
/// `cap`.
pub fn plan(entries: &[Entry], judged: &[Option<Judged>], cap: usize) -> Vec<Fate> {
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
        let l = line(&entries[i]).chars().count();
        if size + l <= cap {
            size += l;
        } else {
            fates[i] = Fate::Drop("didn't fit");
        }
    }
    fates
}

/// The bar a judgment must clear for the sleep to promote an entry on its own (`ZEN_PROMOTE_BAR`,
/// 0.9); a move into a prompt file must clear it on the lowest of three samples.
fn promote_bar() -> f64 {
    crate::env_num("ZEN_PROMOTE_BAR", 0.9)
}

/// Where a lasting entry is promoted to, if anywhere: `USER.md` (about the owner), `IDENTITY.md`
/// (guidance on how the agent acts) or the wiki (lasting knowledge).
pub fn belongs_in(j: &Judged, bar: f64) -> Option<&'static str> {
    if j.durable < bar {
        None
    } else if j.about_owner >= bar {
        Some("USER.md")
    } else if j.guidance >= bar {
        Some("IDENTITY.md")
    } else if j.knowledge >= bar {
        Some("wiki")
    } else {
        None
    }
}

/// Whether an entry may go into a prompt file on its own: only the owner's words and verified
/// results, so text read on the web (saved as `inferred`) never steers every session.
fn may_steer(source: &str) -> bool {
    matches!(source, "owner" | "verified")
}

/// `text` with `line` added at the end of its `## Learned` section (made at the end when missing).
pub fn with_learned(text: &str, line: &str) -> String {
    let body = text.trim_end();
    let Some(h) = body.find("\n## Learned").map(|i| i + 1).or_else(|| body.starts_with("## Learned").then_some(0)) else {
        return format!("{body}\n\n## Learned\n\n{line}\n");
    };
    let after = h + body[h..].find('\n').unwrap_or(body.len() - h);
    match body[after..].find("\n## ") {
        Some(n) => {
            let end = after + n;
            format!("{}\n{line}\n{}\n", body[..end].trim_end(), &body[end..])
        }
        None => format!("{body}\n{line}\n"),
    }
}

/// Save a copy of a prompt file in `<zen home>/backups/prompt-files/<name>-<time>.md` before it changes (by the
/// sleep or an agent's edit); returns the copy's path. Kept 90 days.
pub(crate) fn backup_file(home: &std::path::Path, path: &std::path::Path) -> Result<std::path::PathBuf> {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let dir = home.join(BACKUPS);
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    // Never over an earlier backup (two changes in the same millisecond).
    let mut backup = dir.join(format!("{name}-{stamp}.md"));
    let mut n = 2;
    while backup.exists() {
        backup = dir.join(format!("{name}-{stamp}-{n}.md"));
        n += 1;
    }
    std::fs::write(&backup, std::fs::read(path).unwrap_or_default())?;
    Ok(backup)
}

/// Replace a prompt file's text (backup first, then an atomic rename); returns the backup.
fn replace_file(home: &std::path::Path, rel: &str, new: &str) -> Result<std::path::PathBuf> {
    let path = home.join(rel);
    let backup = backup_file(home, &path)?;
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, new)?;
    std::fs::rename(&tmp, &path)?;
    Ok(backup)
}

/// Move a memory into a prompt file (`rel` under the zen home) as a dated line under `## Learned`,
/// after a backup. Returns the backup's path. A file pushed over its size is compacted next.
fn promote_to_file(home: &std::path::Path, rel: &str, id: i64, text: &str) -> Result<std::path::PathBuf> {
    let old = std::fs::read_to_string(home.join(rel)).unwrap_or_default();
    let line = format!("- {}: {} (from memory m{id})", chrono::Utc::now().format("%Y-%m-%d"), crate::secrets::mask(text).replace('\n', " "));
    replace_file(home, rel, &with_learned(&old, &line))
}

/// What a prompt file over its size is cut to when compacted: 80% of its cap, room to grow.
fn compact_target(cap: usize) -> usize {
    cap * 8 / 10
}

/// The new text from a compaction session's answer, if it is a usable file: between the markers,
/// starting with a heading, within `cap`, and not gutted (at least a quarter of the target).
pub fn compacted_text(answer: &str, cap: usize) -> Option<String> {
    let start = answer.find("<<<FILE")? + "<<<FILE".len();
    let end = start + answer[start..].find("FILE>>>")?;
    let text = answer[start..end].trim();
    let n = text.chars().count();
    (text.starts_with('#') && n <= cap && n >= compact_target(cap) / 4).then(|| format!("{text}\n"))
}

/// Compact a prompt file over its size: a session of the kernel's own (a subagent: it can't write
/// prompt files itself) rewrites it, keeping only what really matters; the kernel checks the
/// answer, backs the file up and writes it. Returns what it did, for the morning note.
async fn compact_file(app: &AppState, home: &std::path::Path, rel: &str) -> Result<String> {
    let old = std::fs::read_to_string(home.join(rel))?;
    let (n, cap) = (old.chars().count(), crate::compile::file_cap(rel));
    let target = compact_target(cap);
    let file = std::path::Path::new(rel).file_name().and_then(|s| s.to_str()).unwrap_or(rel);
    let prompt = format!(
        "Compact {file}. It is {n} characters; only {cap} fit in your instructions. Rewrite it to at most {target} characters. \
Keep only what really matters across jobs; merge what overlaps; drop what is stale or no longer needed (finished projects, \
superseded decisions, old dated details). Keep its title, its section headings, its voice and every hard rule. Fold the dated \
lines under `## Learned` into the sections they belong to, or keep the few that still matter there, shortened. Don't use tools \
and don't write the file: answer with the whole new file only, between a line `<<<FILE` and a line `FILE>>>`.\n\n<<<FILE\n{old}\nFILE>>>"
    );
    let id = crate::turns::run_kernel_session(app, crate::delegate::SUBAGENT, &format!("sleep: compact {file}"), &prompt, home, &app.default_model).await?;
    let answer = crate::delegate::final_answer(&app.db, id).await;
    let new = compacted_text(&answer, cap).ok_or_else(|| anyhow::anyhow!("session {} gave no usable file (between the markers, a heading first, {} to {cap} characters)", &id.to_string()[..8], target / 4))?;
    let backup = replace_file(home, rel, &new)?;
    Ok(format!("compacted {file} from {n} to {} characters (session {}, backup {})", new.chars().count(), &id.to_string()[..8], backup.display()))
}

/// The material System One judges an entry by: the entry, the other entries and the owner's
/// profile (to tell whether it's covered).
fn state_for(e: &Entry, all: &[Entry], user_md: &str, identity_md: &str) -> Value {
    let others: Vec<String> = all.iter().filter(|o| o.id != e.id).map(|o| zen_proto::head(&o.text, 300)).collect();
    json!({
        "memory": e.text,
        "source": e.source,
        "age_days": (e.age_days * 10.0).round() / 10.0,
        "days_since_used": (e.idle_days * 10.0).round() / 10.0,
        "other_memories": others,
        "user_profile": zen_proto::head(user_md, 4000),
        "agent_identity": zen_proto::head(identity_md, 4000),
    })
}

/// Tidy short-term memory: promote lasting entries (to `USER.md`, `IDENTITY.md` or the wiki), rank
/// the rest, keep what fits and archive the others. Records a `sleep_runs` row and one `decisions` row per entry.
pub async fn sleep(app: &AppState, trigger: &str) -> Result<Value> {
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

async fn sleep_inner(app: &AppState, run: i64) -> Result<Value> {
    let db = &app.db;
    let entries = short_entries(db).await?;
    let home = crate::zen_home();
    let user_md = std::fs::read_to_string(home.join("USER.md")).unwrap_or_default();
    let identity_md = std::fs::read_to_string(home.join(crate::layout::IDENTITY)).unwrap_or_default();
    // Memories are private content: System One sees them only when allowed (ZEN_S1_PRIVATE).
    let scorer = crate::score::scorer().filter(|_| crate::score::private_ok());
    let mut judged: Vec<Option<Judged>> = vec![None; entries.len()];
    let mut raw: Vec<Value> = vec![Value::Null; entries.len()];
    if scorer.is_some() && !entries.is_empty() {
        let qs = questions();
        for (i, e) in entries.iter().enumerate() {
            let state = state_for(e, &entries, &user_md, &identity_md);
            match crate::score::decide(app, &state, &qs).await {
                Ok(v) if v["error"].is_null() => {
                    judged[i] = judged_from(&v["answers"]);
                    raw[i] = v["answers"].clone();
                }
                Ok(v) => tracing::warn!("sleep: judging m{}: {}", e.id, v["error"]),
                Err(e2) => tracing::warn!("sleep: judging m{}: {e2:#}", e.id),
            }
            // A move into a prompt file is asked twice more; the lowest of the three answers counts.
            if let Some(j) = judged[i].as_mut().filter(|j| matches!(belongs_in(j, promote_bar()), Some("USER.md" | "IDENTITY.md"))) {
                let repeat = json!({ "durable": qs["durable"], "about_owner": qs["about_owner"], "guidance": qs["guidance"] });
                for _ in 0..2 {
                    let a = match crate::score::decide(app, &state, &repeat).await {
                        Ok(v) if v["error"].is_null() => v["answers"].clone(),
                        _ => Value::Null,
                    };
                    j.durable = j.durable.min(score01(&a["durable"]).unwrap_or(0.0));
                    j.about_owner = j.about_owner.min(score01(&a["about_owner"]).unwrap_or(0.0));
                    j.guidance = j.guidance.min(score01(&a["guidance"]).unwrap_or(0.0));
                }
            }
        }
    }
    // Promotion: into a prompt file (the entry leaves memory: the file is in every session's
    // instructions) or a copy into the wiki (the entry stays while it's needed).
    let mut notes: Vec<String> = Vec::new();
    let mut moved: Vec<Option<&'static str>> = vec![None; entries.len()];
    let mut captured = vec![false; entries.len()];
    for (i, e) in entries.iter().enumerate() {
        let Some(j) = &judged[i] else { continue };
        if j.covered >= 0.8 {
            continue;
        }
        match belongs_in(j, promote_bar()) {
            Some(file @ ("USER.md" | "IDENTITY.md")) if may_steer(&e.source) => {
                let rel = if file == "USER.md" { "USER.md" } else { crate::layout::IDENTITY };
                match promote_to_file(&home, rel, e.id, &e.text) {
                    Ok(backup) => {
                        moved[i] = Some(file);
                        notes.push(format!("promoted m{} to {file} (backup {}): {}", e.id, backup.display(), zen_proto::head(&e.text, 120)));
                    }
                    Err(err) => notes.push(format!("m{} belongs in {file} but wasn't moved: {err:#}", e.id)),
                }
            }
            Some(file @ ("USER.md" | "IDENTITY.md")) => {
                notes.push(format!("m{} looks like it belongs in {file}, but it's an inference: propose it in the conversation: {}", e.id, zen_proto::head(&e.text, 120)));
            }
            Some(_) if !e.captured => match crate::wiki::capture_memory(app, e.id, &e.text, &e.source).await {
                Ok(out) => {
                    captured[i] = true;
                    notes.push(format!("promoted m{} to the wiki: {}", e.id, out.lines().next().unwrap_or("")));
                }
                Err(err) => notes.push(format!("m{} wasn't copied to the wiki: {err:#}", e.id)),
            },
            _ => {}
        }
    }
    // A prompt file over its size (after promotions, or edited past it) is compacted, not cut.
    if scorer.is_some() {
        for rel in ["USER.md", crate::layout::IDENTITY] {
            let size = std::fs::read_to_string(home.join(rel)).map(|t| t.chars().count()).unwrap_or(0);
            if size > crate::compile::file_cap(rel) {
                match compact_file(app, &home, rel).await {
                    Ok(done) => notes.push(done),
                    Err(err) => notes.push(format!("{rel} is over its size and wasn't compacted (the middle is cut from the instructions): {err:#}")),
                }
            }
        }
    }
    let pruned_backups = prune_backups(&home.join(BACKUPS), BACKUP_DAYS);
    if pruned_backups > 0 {
        notes.push(format!("backups: removed {pruned_backups} prompt-file backup(s) older than {BACKUP_DAYS} days"));
    }
    // What stays competes for the space; moved entries don't count.
    let rest: Vec<usize> = (0..entries.len()).filter(|&i| moved[i].is_none()).collect();
    let rest_entries: Vec<Entry> = rest.iter().map(|&i| entries[i].clone()).collect();
    let rest_judged: Vec<Option<Judged>> = rest.iter().map(|&i| judged[i].clone()).collect();
    let mut fates: Vec<Fate> = moved.iter().map(|m| Fate::Drop(if m == &Some("USER.md") { "promoted to USER.md" } else { "promoted to IDENTITY.md" })).collect();
    for (k, f) in plan(&rest_entries, &rest_judged, cap()).into_iter().enumerate() {
        fates[rest[k]] = f;
    }
    let promoted = moved.iter().filter(|m| m.is_some()).count() + captured.iter().filter(|c| **c).count();
    let (mut kept, mut dropped) = (0, 0);
    // All fates and their decisions apply together: a failure midway must not leave memory half
    // tidied with only some decisions logged.
    let mut tx = db.begin().await?;
    for (i, e) in entries.iter().enumerate() {
        let scores = if raw[i].is_null() { None } else { Some(json!({ "answers": raw[i] })) };
        let (tier, reason, chosen) = match &fates[i] {
            Fate::Keep => {
                kept += 1;
                ("short", None, "keep")
            }
            Fate::Drop(why) if moved[i].is_some() => ("archived", Some(*why), "promote"),
            Fate::Drop(why) => {
                dropped += 1;
                ("archived", Some(*why), "drop")
            }
        };
        let proposal = captured[i].then_some("wiki");
        sqlx::query("UPDATE memories SET tier = $2, reason = COALESCE($3, reason), proposed = COALESCE($4, proposed), scores = COALESCE($5, scores), updated_at = CASE WHEN tier = $2 THEN updated_at ELSE now() END WHERE id = $1")
            .bind(e.id)
            .bind(tier)
            .bind(reason)
            .bind(proposal)
            .bind(&scores)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO decisions (point, model, input, answer, chosen, probability, acted) VALUES ('sleep', $1, $2, $3, $4, $5, true)")
            .bind(&scorer)
            .bind(json!({ "memory": e.id, "text": e.text, "source": e.source }))
            .bind(&raw[i])
            .bind(chosen)
            .bind(judged[i].as_ref().map(|j| j.needed))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let pruned = prune_outputs(&crate::zen_home().join("outputs"), OUTPUT_DAYS);
    if pruned > 0 {
        notes.push(format!("outputs: removed {pruned} saved tool output file(s) older than {OUTPUT_DAYS} days"));
    }
    // The wiki's nightly care: commit the agent's own edits since the last capture, report problems.
    let wiki = crate::wiki::root();
    if wiki.join(".git").exists() {
        crate::wiki::commit(&wiki, "sleep: edits since the last capture").await;
        for p in crate::wiki::lint(&wiki) {
            notes.push(format!("wiki: {p}"));
        }
    }
    notes.extend(crate::workshop::tend(db).await);
    notes.extend(crate::delegate::tune(db).await);
    let note = if notes.is_empty() { None } else { Some(notes.join("\n")) };
    sqlx::query("UPDATE sleep_runs SET ended_at = now(), entries = $2, kept = $3, dropped = $4, promoted = $5, proposed = 0, note = $6 WHERE id = $1")
        .bind(run)
        .bind(entries.len() as i32)
        .bind(kept)
        .bind(dropped)
        .bind(promoted as i32)
        .bind(&note)
        .execute(db)
        .await?;
    Ok(json!({ "run": run, "entries": entries.len(), "kept": kept, "dropped": dropped, "promoted": promoted, "scorer": scorer, "note": note }))
}

/// Where prompt-file backups go, under the zen home (its own folder: `backups/` holds other things).
pub(crate) const BACKUPS: &str = "backups/prompt-files";

/// Days a prompt file's backup is kept in `<zen home>/backups/prompt-files`.
const BACKUP_DAYS: u64 = 90;

/// Remove backups (`*.md` directly in `dir`) not modified for `days` days; returns how many.
fn prune_backups(dir: &std::path::Path, days: u64) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let limit = std::time::Duration::from_secs(days * 86_400);
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && e.file_name().to_string_lossy().ends_with(".md"))
        .filter(|e| e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > limit))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// Days a saved tool output (cut bash/MCP output, fetched PDFs) is kept in `<zen home>/outputs`.
const OUTPUT_DAYS: u64 = 30;

/// Remove regular files directly in `dir` not modified for `days` days; returns how many. Only
/// files the kernel saves (`*.log`, `mcp-*.txt`, `web-*.pdf`): anything else there is left alone.
fn prune_outputs(dir: &std::path::Path, days: u64) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let limit = std::time::Duration::from_secs(days * 86_400);
    let ours = |n: &str| n.ends_with(".log") || (n.starts_with("mcp-") && n.ends_with(".txt")) || (n.starts_with("web-") && n.ends_with(".pdf"));
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && ours(&e.file_name().to_string_lossy()))
        .filter(|e| e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > limit))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// A line about the last sleep for a session's instructions, when it ran in the last day (the
/// "morning note": what was kept, archived and promoted, and where).
pub async fn morning_note(db: &PgPool) -> Result<Option<String>, sqlx::Error> {
    let r = sqlx::query(
        "SELECT ended_at, entries, kept, dropped, promoted, note FROM sleep_runs
         WHERE ended_at > now() - interval '1 day' AND error IS NULL ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(r.map(|r| {
        let n = |k: &str| r.get::<Option<i32>, _>(k).unwrap_or(0);
        let when = r.get::<chrono::DateTime<chrono::Utc>, _>("ended_at").format("%Y-%m-%d %H:%M UTC");
        let mut line = format!(
            "Last sleep ({when}): {} entries, {} kept, {} archived, {} promoted.",
            n("entries"),
            n("kept"),
            n("dropped"),
            n("promoted")
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
    let rows = sqlx::query(
        "SELECT id, text, source, tier, proposed, reason, created_at, updated_at FROM memories
         WHERE $1 = 'all' OR tier = $1 OR ($1 = 'proposed' AND proposed = 'promote') ORDER BY id DESC LIMIT 500",
    )
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
        Entry { id, text: text.into(), source: source.into(), age_days: idle, idle_days: idle, uses: 0, captured: false }
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
    fn plan_drops_covered_and_keeps_by_need() {
        let all = vec![e(1, "duplicate of the profile", "owner", 1.0), e(2, "rarely needed detail", "inferred", 1.0), e(3, "often needed fact", "inferred", 1.0)];
        let j = |needed: f64, covered: f64| Some(Judged { needed, durable: 0.5, covered, ..Default::default() });
        let fates = plan(&all, &[j(0.9, 0.9), j(0.1, 0.0), j(0.9, 0.0)], line(&all[2]).chars().count());
        assert_eq!(fates, vec![Fate::Drop("already covered"), Fate::Drop("didn't fit"), Fate::Keep]);
    }

    #[test]
    fn lasting_entries_are_promoted_by_kind() {
        let j = |durable: f64, about_owner: f64, guidance: f64, knowledge: f64| Judged { durable, about_owner, guidance, knowledge, ..Default::default() };
        assert_eq!(belongs_in(&j(0.95, 0.95, 0.0, 0.0), 0.9), Some("USER.md"));
        assert_eq!(belongs_in(&j(0.95, 0.1, 0.95, 0.9), 0.9), Some("IDENTITY.md"));
        assert_eq!(belongs_in(&j(0.95, 0.1, 0.1, 0.95), 0.9), Some("wiki"));
        assert_eq!(belongs_in(&j(0.5, 0.95, 0.95, 0.95), 0.9), None, "passing state stays memory");
        assert_eq!(belongs_in(&j(0.95, 0.85, 0.1, 0.1), 0.9), None, "below the bar");
        assert!(may_steer("owner") && may_steer("verified") && !may_steer("inferred"));
    }

    #[test]
    fn learned_lines_go_at_the_end_of_their_section() {
        let line = "- 2026-10-08: x (from memory m1)";
        assert_eq!(with_learned("# U\n\n## Who\n\nme\n", line), format!("# U\n\n## Who\n\nme\n\n## Learned\n\n{line}\n"));
        assert_eq!(with_learned("# I\n\n## Learned\n\nDated.\n- old\n", line), format!("# I\n\n## Learned\n\nDated.\n- old\n{line}\n"));
        assert_eq!(with_learned("# I\n\n## Learned\n\n- old\n\n## Next\n\nz\n", line), format!("# I\n\n## Learned\n\n- old\n{line}\n\n## Next\n\nz\n"));
    }

    #[test]
    fn promoting_to_a_file_backs_it_up() {
        let home = crate::test_util::TestDir::new("promote");
        std::fs::write(home.join("USER.md"), "# USER.md\n\nJose.\n").unwrap();
        let backup = promote_to_file(&home, "USER.md", 8, "Jose needs stronger distribution.").unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "# USER.md\n\nJose.\n");
        let now = std::fs::read_to_string(home.join("USER.md")).unwrap();
        assert!(now.contains("## Learned\n\n- ") && now.contains("Jose needs stronger distribution. (from memory m8)"), "{now}");
        assert_ne!(promote_to_file(&home, "USER.md", 9, "again").unwrap(), backup, "each change gets its own backup");
    }

    #[test]
    fn a_compaction_answer_is_used_only_when_it_is_a_whole_file_that_fits() {
        let file = format!("# USER.md\n\n{}", "x".repeat(900));
        assert_eq!(compacted_text(&format!("Here it is:\n<<<FILE\n{file}\nFILE>>>\n"), 3000), Some(format!("{file}\n")));
        assert_eq!(compacted_text(&file, 3000), None, "no markers");
        assert_eq!(compacted_text("<<<FILE\nno heading\nFILE>>>", 3000), None);
        assert_eq!(compacted_text("<<<FILE\n# USER.md\n\nme\nFILE>>>", 3000), None, "gutted");
        assert_eq!(compacted_text(&format!("<<<FILE\n# U\n{}\nFILE>>>", "x".repeat(3001)), 3000), None, "too long");
    }

    #[test]
    fn without_judgments_recency_decides() {
        let all = vec![e(1, "stale", "inferred", 30.0), e(2, "fresh", "inferred", 0.0)];
        let fates = plan(&all, &[None, None], line(&all[1]).chars().count());
        assert_eq!(fates, vec![Fate::Drop("didn't fit"), Fate::Keep]);
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
    fn wilson_lower_needs_many_agreeing_outcomes() {
        assert_eq!(wilson_lower(0, 0, 1.645), 0.0);
        assert!(wilson_lower(10, 10, 1.645) < 0.95, "ten agreements aren't enough");
        assert!(wilson_lower(52, 52, 1.645) >= 0.95);
        assert!(wilson_lower(51, 52, 1.645) < 0.95, "one rejection costs a lot");
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
        let j = judged_from(&json!({ "needed": { "score": 3, "confidence": 0.9 }, "durable": { "probability": 0.7 }, "guidance": { "probability": 0.9 } })).unwrap();
        assert_eq!((j.needed, j.durable, j.covered, j.guidance), (1.0, 0.7, 0.0, 0.9));
        assert!(judged_from(&json!({})).is_none());
    }
}
