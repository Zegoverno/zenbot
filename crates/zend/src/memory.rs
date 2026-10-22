//! Short-term memory (DESIGN.md, "Memory and knowledge"; D-069).
//!
//! One capped file, `global/MEMORY.md`, in every session's fixed instructions (rendered at the
//! session's start, so a write shows from the next session and the prompt cache holds). The agent
//! saves to it freely with `remember`; after each turn with the owner, System One picks sentences
//! of the owner's own message worth keeping (`memory_capture`). Each entry is a row of `memories`
//! carrying its rating and every session that wrote it. The owner may edit `MEMORY.md` in any
//! editor: changed, added and removed lines are read back before the next use (`import`).
//!
//! The space is fixed (`ZEN_MEMORY_CHARS`): writes are never refused, and passing the size starts
//! a sleep at once (brain/sleep.rs) that moves lasting entries to long-term memory, one record file
//! each, and archives finished ones. `remember` with `long: true` writes a long-term memory on the
//! spot when System One judges it clearly lasting (`memory_long`, a high bar); otherwise it is
//! saved short-term. Nothing is ever deleted.
//!
//! Taint (D-053, D-062): an entry rated 4 or more, or whose writer (any of them) was denied, is
//! left out of the instructions and the file (only its id is listed); 2–3 go in labelled. The
//! reader depends on the writers of what it is shown, recorded before it is shown; an error fails
//! the render, never trusts the text. Replacing an entry adds the replacing session as a writer,
//! and the entry's rating never drops.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::AppState;

/// The size the sleep tidies short-term memory to, in characters (ZEN_MEMORY_CHARS, default 4000).
pub fn cap() -> usize {
    crate::config::get().memory_chars
}

/// What the instructions show at most (twice the size); a sleep starts long before.
fn ceiling() -> usize {
    cap() * 2
}

/// One change to short-term memory at a time: `remember`, the owner's file edits and the export.
static WRITING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub text: String,
    pub source: String,
    /// Rated 4 or more by origin, or a writer of it is contaminated now (D-062).
    pub tainted: bool,
    /// Every session that wrote it: a reader of the entry depends on them.
    pub writers: Vec<Uuid>,
    pub risk: i16,
}

fn line(e: &Entry) -> String {
    format!(
        "- [m{}] {}\n",
        e.id,
        crate::context::label(&format!("memory m{} ({})", e.id, e.source), e.risk, &e.text).replace('\n', " ")
    )
}

async fn short_entries(db: &PgPool) -> Result<Vec<Entry>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, text, source, risk, (risk >= 4 OR zen_memory_contaminated(id)) AS tainted,
                ARRAY(SELECT writer FROM brain_memory_inputs WHERE memory_id = m.id) AS writers
         FROM memories m WHERE tier = 'short' ORDER BY created_at, id",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Entry {
            id: r.get("id"),
            text: r.get("text"),
            source: r.get("source"),
            tainted: r.get("tainted"),
            writers: r.get("writers"),
            risk: r.get("risk"),
        })
        .collect())
}

/// Short-term memory as it goes into `reader`'s instructions: every entry rated 0–3, newest first
/// past the hard limit; entries rated 4 or more, or with a denied writer, only by id (D-062).
pub async fn render(db: &PgPool, reader: Uuid) -> Result<String> {
    import(db).await?;
    let entries = short_entries(db).await?;
    let (marked, clean): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|e| e.tainted);
    let writers: Vec<Uuid> = clean.iter().flat_map(|e| e.writers.clone()).collect();
    crate::sources::depend(db, reader, &writers).await?;
    crate::sources::raise(db, reader, clean.iter().map(|e| e.risk).max().unwrap_or(1)).await?;
    ensure!(
        !crate::taint::stopped(db, reader).await?,
        "memories withheld: a writer was stopped"
    );
    let mut out = render_entries(&clean, ceiling());
    out.push_str(&left_out(&marked));
    Ok(out)
}

/// The line that names the entries left out as untrusted, if any.
fn left_out(marked: &[Entry]) -> String {
    if marked.is_empty() {
        return String::new();
    }
    let ids: Vec<String> = marked.iter().map(|e| format!("m{}", e.id)).collect();
    format!(
        "({} saved by sessions that read untrusted content, so not shown here: search memories to read them, as untrusted content.)\n",
        ids.join(", ")
    )
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
    out.push_str(&format!(
        "({} older entries are left out until the sleep tidies memory.)\n",
        entries.len() - kept.len()
    ));
    out
}

fn file() -> std::path::PathBuf {
    crate::layout::global_dir().join("MEMORY.md")
}

const HEADER: &str = "# MEMORY.md\n\nzenbot's short-term memory. Edit, add (`- text`) or remove lines here, or ask zenbot; lasting entries move to long-term memory (memory/) at the sleep.\n\n";

/// Write `MEMORY.md` from the entries (marked ones by id only) and rate its bytes with every
/// writer of what it shows, so reading the file is reading them.
pub async fn export(db: &PgPool) -> Result<()> {
    let _one = WRITING.lock().await;
    write_file(db).await
}

async fn write_file(db: &PgPool) -> Result<()> {
    let entries = short_entries(db).await?;
    let (marked, clean): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|e| e.tainted);
    let text = format!(
        "{HEADER}{}{}",
        clean.iter().map(line).collect::<String>(),
        left_out(&marked)
    );
    let path = file();
    for e in &clean {
        for w in &e.writers {
            crate::sources::register_bytes(db, &path, &text, Some(*w), e.risk).await?;
        }
    }
    crate::sources::register_bytes(db, &path, &text, None, clean.iter().map(|e| e.risk).max().unwrap_or(1)).await?;
    crate::brain::write_private(&path, &text).await?;
    // What was written, and which entries it showed: only those can have been removed by hand.
    let shown: Vec<String> = clean.iter().map(|e| e.id.to_string()).collect();
    sqlx::query("INSERT INTO search_state(key,value) VALUES('memory_md',$1) ON CONFLICT(key) DO UPDATE SET value=$1")
        .bind(format!("{} {}", crate::brain::hash(&text), shown.join(",")))
        .execute(db)
        .await?;
    Ok(())
}

/// `- [m12] text` → (12, text without a rating label).
fn parse_line(l: &str) -> Option<(i64, String)> {
    let rest = l.strip_prefix("- [m")?;
    let (id, text) = rest.split_once("] ")?;
    let text = match text.strip_prefix("[source: ").and_then(|t| t.split_once("] ")) {
        Some((_, t)) => t,
        None => text,
    };
    Some((id.parse().ok()?, text.trim().to_string()))
}

/// Read the owner's edits of `MEMORY.md` back: a changed line changes its entry (its rating never
/// drops), a new `- text` line is a new entry rated as the file's bytes are (D-067: bytes no session
/// wrote take the highest rating its writers had), a removed line archives its entry. Entries the
/// file doesn't show (marked ones) are left alone. Nothing happens until the kernel has written the
/// file once.
pub async fn import(db: &PgPool) -> Result<()> {
    let _one = WRITING.lock().await;
    let path = file();
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return Ok(());
    };
    let written: Option<String> = sqlx::query_scalar("SELECT value FROM search_state WHERE key='memory_md'")
        .fetch_optional(db)
        .await?;
    let Some((hash, shown)) = written.as_deref().and_then(|w| w.split_once(' ')) else {
        return Ok(());
    };
    if hash == crate::brain::hash(&text) {
        return Ok(());
    }
    let shown: Vec<i64> = shown.split(',').filter_map(|id| id.parse().ok()).collect();
    let rating = crate::sources::file_rating(db, &path, &text).await?.max(1);
    let entries = short_entries(db).await?;
    let mut seen = vec![];
    for l in text.lines() {
        if let Some((id, new)) = parse_line(l) {
            let Some(e) = entries.iter().find(|e| e.id == id && !e.tainted) else {
                continue;
            };
            seen.push(id);
            if new != e.text && !new.is_empty() {
                sqlx::query("UPDATE memories SET text=$2,updated_at=now() WHERE id=$1")
                    .bind(id)
                    .bind(crate::secrets::mask(&new))
                    .execute(db)
                    .await?;
                sqlx::query("UPDATE memories SET risk=GREATEST($2,$3) WHERE id=$1")
                    .bind(id)
                    .bind(e.risk)
                    .bind(rating)
                    .execute(db)
                    .await?;
            }
        } else if let Some(new) = l
            .strip_prefix("- ")
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.starts_with('('))
        {
            let id: i64 = sqlx::query_scalar("INSERT INTO memories(text,source) VALUES($1,'owner') RETURNING id")
                .bind(crate::secrets::mask(new))
                .fetch_one(db)
                .await?;
            sqlx::query("UPDATE memories SET risk=$2 WHERE id=$1")
                .bind(id)
                .bind(rating)
                .execute(db)
                .await?;
        }
    }
    for e in entries.iter().filter(|e| shown.contains(&e.id) && !e.tainted && !seen.contains(&e.id)) {
        sqlx::query("UPDATE memories SET tier='archived',reason='removed from MEMORY.md',updated_at=now() WHERE id=$1 AND tier='short'").bind(e.id).execute(db).await?;
    }
    write_file(db).await
}

pub fn spec() -> Value {
    json!({
        "name": "remember",
        "description": "Save facts about owner decisions, commitments, preferences, questions and projects, not code/docs/git or self-instructions. Entries appear from the next session. Replace near-duplicates by id; remove wrong entries. Sleep archives finished entries and moves lasting ones to searchable long-term memory. long=true requests immediate retention only for clearly lasting facts. source=owner means exact owner words; verified means checked; otherwise inferred.",
        "parameters": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["add", "replace", "remove"], "description": "add (default), replace or remove" },
                "text": { "type": "string", "description": "The memory, one or two sentences (add, replace)" },
                "id": { "type": "string", "description": "The entry to replace or remove, e.g. m12" },
                "kind": { "type": "string", "enum": ["experience", "decision", "preference", "commitment", "question"], "description": "What it is (default experience)" },
                "source": { "type": "string", "enum": ["owner", "verified", "inferred"], "description": "Where it comes from (default inferred)" },
                "long": { "type": "boolean", "description": "Keep it in long-term memory at once (rare)" }
            }
        }
    })
}

fn parse_id(v: &Value) -> Option<i64> {
    v.as_str()
        .map(|s| s.trim().trim_start_matches(['m', 'M']))
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_i64())
}

async fn used_chars(db: &PgPool) -> usize {
    sqlx::query_scalar::<_, Option<i64>>("SELECT SUM(length(text) + 10)::bigint FROM memories WHERE tier = 'short'")
        .fetch_one(db)
        .await
        .ok()
        .flatten()
        .unwrap_or(0) as usize
}

/// Run the `remember` tool.
pub async fn run_tool(app: &AppState, session: Uuid, args: &Value) -> (String, bool) {
    match remember(app, session, args).await {
        Ok(text) => (text, false),
        Err(e) => (e.to_string(), true),
    }
}

async fn remember(app: &AppState, session: Uuid, args: &Value) -> Result<String> {
    let db = &app.db;
    let action = args["action"].as_str().unwrap_or("add");
    let mut source = match args["source"].as_str().unwrap_or("inferred") {
        s @ ("owner" | "verified" | "inferred") => s,
        other => anyhow::bail!("source must be owner, verified or inferred, not `{other}`"),
    };
    let kind = match args["kind"].as_str().unwrap_or("experience") {
        k @ ("experience" | "decision" | "preference" | "commitment" | "question") => k,
        other => anyhow::bail!("kind must be experience, decision, preference, commitment or question, not `{other}`"),
    };
    let text = crate::secrets::mask(args["text"].as_str().map(str::trim).unwrap_or(""));
    // A session that read untrusted content can't vouch for what it saves, and only the owner's
    // exact words count as theirs.
    let mut note = "";
    let tainted = crate::taint::limited(db, session, crate::taint::Limit::Vouch).await;
    if tainted {
        source = "inferred";
    }
    if source == "owner" && !text.is_empty() {
        let said: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tape_events WHERE session_id=$1 AND origin='owner' AND kind='message' AND payload->>'role'='user' AND strpos(payload->>'content',$2)>0)")
            .bind(session)
            .bind(&text)
            .fetch_one(db)
            .await?;
        if !said {
            source = "inferred";
            note = " (saved as inferred: `owner` is for the owner's exact words)";
        }
    }
    let done = match action {
        "add" => {
            ensure!(!text.is_empty(), "add needs `text`");
            ensure!(
                text.chars().count() <= 600,
                "keep a memory under 600 characters; split it or say it shorter"
            );
            if args["long"].as_bool().unwrap_or(false) {
                let item = json!({"memory": text, "kind": kind, "source": source});
                let d =
                    crate::s1::decide_items(db, "memory_long", crate::s1::Facts::new(Some(session)), vec![item]).await;
                if d.first().is_some_and(|d| d.is("long")) {
                    let id = crate::brain::long_term(
                        db,
                        Some(session),
                        &text,
                        kind,
                        source,
                        &format!("session:{session}"),
                        chrono::Utc::now(),
                    )
                    .await?;
                    return Ok(format!("Saved to long-term memory as `{id}`{note}: search finds it."));
                }
                note = " (short-term: not clearly lasting enough for long-term memory on its own; the sleep reconsiders it)";
            }
            let _one = WRITING.lock().await;
            let id: i64 = sqlx::query_scalar("INSERT INTO memories (text, source, session_id, record_type, tainted) VALUES ($1, $2, $3, $4, $5) RETURNING id")
                .bind(&text)
                .bind(source)
                .bind(session)
                .bind(kind)
                .bind(tainted)
                .fetch_one(db)
                .await?;
            format!("Saved as m{id}{note}.")
        }
        "replace" => {
            let id =
                parse_id(&args["id"]).ok_or_else(|| anyhow::anyhow!("replace needs the entry's `id`, e.g. m12"))?;
            ensure!(!text.is_empty(), "replace needs the new `text`");
            ensure!(text.chars().count() <= 600, "keep a memory under 600 characters");
            let _one = WRITING.lock().await;
            let old: Option<i16> = sqlx::query_scalar("SELECT risk FROM memories WHERE id = $1 AND tier = 'short'")
                .bind(id)
                .fetch_optional(db)
                .await?;
            let old = old.ok_or_else(|| anyhow::anyhow!("no short-term memory m{id}"))?;
            // The new text is the replacing session's too: it becomes a writer, and the rating
            // keeps the highest of the old entry's and the new writer's.
            sqlx::query("UPDATE memories SET text = $2, source = $3, session_id = $4, tainted = tainted OR $5, updated_at = now(), used_at = now(), uses = uses + 1 WHERE id = $1")
                .bind(id)
                .bind(&text)
                .bind(source)
                .bind(session)
                .bind(tainted)
                .execute(db)
                .await?;
            sqlx::query("UPDATE memories SET risk = GREATEST(risk, $2) WHERE id = $1")
                .bind(id)
                .bind(old)
                .execute(db)
                .await?;
            format!("Replaced m{id}{note}.")
        }
        "remove" => {
            let id = parse_id(&args["id"]).ok_or_else(|| anyhow::anyhow!("remove needs the entry's `id`, e.g. m12"))?;
            let _one = WRITING.lock().await;
            let n = sqlx::query("UPDATE memories SET tier = 'archived', reason = 'removed by the agent', updated_at = now() WHERE id = $1 AND tier = 'short'")
                .bind(id)
                .execute(db)
                .await?
                .rows_affected();
            ensure!(n == 1, "no short-term memory m{id}");
            format!("Removed m{id} (archived, not deleted).")
        }
        other => anyhow::bail!("action must be add, replace or remove, not `{other}`"),
    };
    export(db).await?;
    let used = used_chars(db).await;
    if used > cap() {
        start_sleep(app, "size");
    }
    Ok(format!(
        "{done} Memory: {used}/{} characters; it shows in your instructions from the next session.",
        cap()
    ))
}

/// After a turn with the owner, in the background: sentences of the owner's last message System
/// One judges worth keeping become short-term entries in the owner's words.
pub fn capture_turn(app: &AppState, session: Uuid) {
    let app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = capture(&app, session).await {
            tracing::warn!("capturing memory from session {session}: {e:#}");
        }
    });
}

async fn capture(app: &AppState, session: Uuid) -> Result<()> {
    let db = &app.db;
    let said: Option<String> = sqlx::query_scalar("SELECT payload->>'content' FROM tape_events WHERE session_id=$1 AND origin='owner' AND kind='message' AND payload->>'role'='user' ORDER BY seq DESC LIMIT 1")
        .bind(session)
        .fetch_optional(db)
        .await?
        .flatten();
    let Some(said) = said else { return Ok(()) };
    let spans: Vec<String> = said
        .split(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|s| (12..=600).contains(&s.chars().count()))
        .take(20)
        .map(str::to_owned)
        .collect();
    if spans.is_empty() {
        return Ok(());
    }
    let items = spans.iter().map(|s| json!({"owner_statement": s})).collect();
    let decisions = crate::s1::decide_items(db, "memory_capture", crate::s1::Facts::new(Some(session)), items).await;
    let mut saved = 0;
    for (span, d) in spans.iter().zip(decisions) {
        if !d.is("save") {
            continue;
        }
        let _one = WRITING.lock().await;
        let text = crate::secrets::mask(span);
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memories WHERE text=$1 AND tier<>'archived')")
                .bind(&text)
                .fetch_one(db)
                .await?;
        if !known {
            sqlx::query("INSERT INTO memories (text, source, session_id) VALUES ($1, 'owner', $2)")
                .bind(&text)
                .bind(session)
                .execute(db)
                .await?;
            saved += 1;
        }
    }
    if saved > 0 {
        export(db).await?;
        if used_chars(db).await > cap() {
            start_sleep(app, "size");
        }
    }
    Ok(())
}

/// Start a sleep in the background, unless one is running.
fn start_sleep(app: &AppState, trigger: &'static str) {
    let app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = sleep(&app, trigger).await {
            tracing::warn!("memory sleep ({trigger}): {e:#}");
        }
    });
}

/// The sleep (brain/sleep.rs).
pub async fn sleep(app: &AppState, trigger: &str) -> Result<Value> {
    crate::brain::consolidate(app, trigger).await
}

/// Back up a file the agent changes (prompt files) under the zen home, never over an older backup.
pub(crate) fn backup_file(home: &std::path::Path, path: &std::path::Path) -> Result<std::path::PathBuf> {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let dir = home.join(BACKUPS);
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let mut backup = dir.join(format!("{name}-{stamp}.md"));
    let mut n = 2;
    while backup.exists() {
        backup = dir.join(format!("{name}-{stamp}-{n}.md"));
        n += 1;
    }
    std::fs::write(&backup, std::fs::read(path).unwrap_or_default())?;
    Ok(backup)
}

/// Where prompt-file backups go, under the zen home.
pub(crate) const BACKUPS: &str = "backups/prompt-files";

/// A line about the last sleep for a session's instructions, when it ran in the last day.
pub async fn morning_note(db: &PgPool) -> Result<Option<String>, sqlx::Error> {
    let r = sqlx::query("SELECT ended_at, note FROM sleep_runs WHERE ended_at > now() - interval '1 day' AND error IS NULL ORDER BY id DESC LIMIT 1")
        .fetch_optional(db)
        .await?;
    Ok(r.map(|r| {
        let when = r
            .get::<chrono::DateTime<chrono::Utc>, _>("ended_at")
            .format("%Y-%m-%d %H:%M UTC");
        format!(
            "Last sleep ({when}): {}",
            zen_proto::head(&r.get::<Option<String>, _>("note").unwrap_or_default(), 1500)
        )
    }))
}

/// The latest sleep, for `zen status` and `zen memory`.
pub async fn last_run(db: &PgPool) -> Result<Value, sqlx::Error> {
    let r = sqlx::query("SELECT id, started_at, ended_at, trigger, scorer, entries, kept, dropped, promoted, note, error FROM sleep_runs ORDER BY id DESC LIMIT 1")
        .fetch_optional(db)
        .await?;
    Ok(r.map(|r| {
        json!({
            "id": r.get::<i64, _>("id"), "started_at": r.get::<chrono::DateTime<chrono::Utc>, _>("started_at"),
            "ended_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("ended_at"), "trigger": r.get::<String, _>("trigger"),
            "scorer": r.get::<Option<String>, _>("scorer"), "entries": r.get::<Option<i32>, _>("entries"), "kept": r.get::<Option<i32>, _>("kept"),
            "dropped": r.get::<Option<i32>, _>("dropped"), "promoted": r.get::<Option<i32>, _>("promoted"),
            "note": r.get::<Option<String>, _>("note"), "error": r.get::<Option<String>, _>("error"),
        })
    })
    .unwrap_or(Value::Null))
}

/// Memories by tier, newest first, for `zen memory`.
pub async fn list(db: &PgPool, tier: &str) -> Result<Value, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, text, source, tier, reason, created_at, updated_at, (risk>=4 OR zen_memory_contaminated(id)) AS tainted FROM memories
         WHERE $1 = 'all' OR tier = $1 ORDER BY id DESC LIMIT 500",
    )
    .bind(tier)
    .fetch_all(db)
    .await?;
    Ok(Value::Array(
        rows.iter()
            .map(|r| {
                json!({ "id": format!("m{}", r.get::<i64, _>("id")), "text": r.get::<String, _>("text"), "source": r.get::<String, _>("source"), "untrusted": r.get::<bool, _>("tainted"),
                        "tier": r.get::<String, _>("tier"), "reason": r.get::<Option<String>, _>("reason"),
                        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at") })
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: i64, text: &str, risk: i16) -> Entry {
        Entry {
            id,
            text: text.into(),
            source: "inferred".into(),
            tainted: false,
            writers: vec![],
            risk,
        }
    }

    #[test]
    fn render_keeps_newest_entries_when_over_the_size() {
        let entries: Vec<Entry> = (1..=10).map(|i| e(i, &"x".repeat(40), 1)).collect();
        let out = render_entries(&entries, 200);
        assert!(out.contains("[m10]") && !out.contains("[m1]") && out.contains("older entries are left out"));
        assert_eq!(render_entries(&entries[..2], 200).lines().count(), 2);
    }

    #[test]
    fn lines_read_back_without_their_label() {
        assert_eq!(parse_line(&line(&e(12, "plain", 1))), Some((12, "plain".into())));
        assert_eq!(
            parse_line(line(&e(7, "labelled", 3)).trim_end()),
            Some((7, "labelled".into()))
        );
        assert_eq!(parse_line("- a new line"), None);
    }

    #[test]
    fn marked_entries_are_named_not_shown() {
        let marked = vec![Entry {
            tainted: true,
            ..e(4, "web text", 4)
        }];
        let out = left_out(&marked);
        assert!(out.contains("m4") && !out.contains("web text"));
    }
}
