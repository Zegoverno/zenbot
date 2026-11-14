//! Short-term memory.
//!
//! Each agent has its own: one capped file, `MEMORY.md` at the root of its vault (zenbot's is
//! `global/MEMORY.md`), in its sessions' fixed instructions (rendered at the session's start, so a
//! write shows from the next session and the prompt cache holds). Rows of `memories` carry their
//! agent (the writing session's); an agent sees and changes only its own. The agent
//! saves to it freely with `remember`; after each turn with the owner, System One picks sentences
//! of the owner's own message worth keeping (`memory_capture`). Each entry is a row of `memories`.
//! The owner may edit `MEMORY.md` in any
//! editor: changed, added and removed lines are read back before the next use (`import`).
//!
//! The space is fixed (`ZEN_MEMORY_CHARS`): writes are never refused, and passing the size starts
//! a sleep at once (brain/sleep.rs) that moves lasting entries to long-term memory, one record file
//! each, and archives finished ones. `remember` with `long: true` writes a long-term memory on the
//! spot when System One judges it clearly lasting (`memory_long`, a high bar); otherwise it is
//! saved short-term. Nothing is ever deleted.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sqlx::{SqlitePool, Row};
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

/// An owner's message on the tape: a user message the kernel didn't write.
pub(crate) const OWNER_MESSAGE: &str = "kind='message' AND payload->>'role'='user' AND COALESCE(payload->>'kernel',0) != 1";

/// One change to short-term memory at a time: `remember`, the owner's file edits and the export.
static WRITING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub text: String,
}

fn line(e: &Entry) -> String {
    format!("- [m{}] {}\n", e.id, e.text.replace('\n', " "))
}

async fn short_entries(db: &SqlitePool, agent: &str) -> Result<Vec<Entry>, sqlx::Error> {
    let rows = sqlx::query("SELECT id, text FROM memories WHERE tier = 'short' AND agent = $1 ORDER BY created_at, id").bind(agent).fetch_all(db).await?;
    Ok(rows.iter().map(|r| Entry { id: r.get("id"), text: r.get("text") }).collect())
}

/// An agent's short-term memory as it goes into its sessions' instructions: newest first past the
/// hard limit.
pub async fn render(db: &SqlitePool, agent: &str) -> Result<String> {
    import(db, agent).await?;
    Ok(render_entries(&short_entries(db, agent).await?, ceiling()))
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

/// An agent's `MEMORY.md`, at the root of its vault.
fn file(agent: &str) -> std::path::PathBuf {
    crate::agents::vault(agent).join("MEMORY.md")
}

fn header(agent: &str) -> String {
    format!("# MEMORY.md\n\n{agent}'s short-term memory. Edit, add (`- text`) or remove lines here, or ask {agent}; lasting entries move to long-term memory (memory/) at the sleep.\n\n")
}

/// Where what the kernel last wrote of an agent's `MEMORY.md` is kept (zenbot's key from before agents).
fn state_key(agent: &str) -> String {
    if agent == crate::agents::DEFAULT { "memory_md".into() } else { format!("memory_md:{agent}") }
}

/// Write an agent's `MEMORY.md` from its entries.
pub async fn export(db: &SqlitePool, agent: &str) -> Result<()> {
    let _one = WRITING.lock().await;
    write_file(db, agent, &file(agent)).await
}

async fn write_file(db: &SqlitePool, agent: &str, path: &std::path::Path) -> Result<()> {
    let entries = short_entries(db, agent).await?;
    let text = format!("{}{}", header(agent), entries.iter().map(line).collect::<String>());
    crate::brain::write_private(path, &text).await?;
    // What was written, and which entries it showed: only those can have been removed by hand.
    let shown: Vec<String> = entries.iter().map(|e| e.id.to_string()).collect();
    sqlx::query("INSERT INTO search_state(key,value) VALUES($2,$1) ON CONFLICT(key) DO UPDATE SET value=$1")
        .bind(format!("{} {}", crate::brain::hash(&text), shown.join(",")))
        .bind(state_key(agent))
        .execute(db)
        .await?;
    Ok(())
}

/// `- [m12] text` → (12, text).
fn parse_line(l: &str) -> Option<(i64, String)> {
    let rest = l.strip_prefix("- [m")?;
    let (id, text) = rest.split_once("] ")?;
    Some((id.parse().ok()?, text.trim().to_string()))
}

/// Read the owner's edits of `MEMORY.md` back: a changed line changes its entry, a new `- text`
/// line is a new entry, a removed line archives its entry. Nothing happens until the kernel has
/// written the file once. Each agent's file changes only its own entries.
pub async fn import(db: &SqlitePool, agent: &str) -> Result<()> {
    let _one = WRITING.lock().await;
    import_file(db, agent, &file(agent)).await
}

async fn import_file(db: &SqlitePool, agent: &str, path: &std::path::Path) -> Result<()> {
    let Ok(text) = crate::sandbox::read_vault_file(path) else {
        return Ok(());
    };
    let written: Option<String> = sqlx::query_scalar("SELECT value FROM search_state WHERE key=$1")
        .bind(state_key(agent))
        .fetch_optional(db)
        .await?;
    let Some((hash, shown)) = written.as_deref().and_then(|w| w.split_once(' ')) else {
        return Ok(());
    };
    if hash == crate::brain::hash(&text) {
        return Ok(());
    }
    let shown: Vec<i64> = shown.split(',').filter_map(|id| id.parse().ok()).collect();
    let entries = short_entries(db, agent).await?;
    let mut seen = vec![];
    for l in text.lines() {
        if let Some((id, new)) = parse_line(l) {
            let Some(e) = entries.iter().find(|e| e.id == id) else {
                continue;
            };
            seen.push(id);
            if new != e.text && !new.is_empty() {
                sqlx::query("UPDATE memories SET text=$2,updated_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id=$1")
                    .bind(id)
                    .bind(crate::secrets::mask(&new))
                    .execute(db)
                    .await?;
            }
        } else if let Some(new) = l
            .strip_prefix("- ")
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.starts_with('('))
        {
            sqlx::query("INSERT INTO memories(text,source,agent) VALUES($1,'owner',$2)")
                .bind(crate::secrets::mask(new))
                .bind(agent)
                .execute(db)
                .await?;
        }
    }
    for e in entries.iter().filter(|e| shown.contains(&e.id) && !seen.contains(&e.id)) {
        sqlx::query("UPDATE memories SET tier='archived',reason='removed from MEMORY.md',updated_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id=$1 AND tier='short'").bind(e.id).execute(db).await?;
    }
    write_file(db, agent, path).await
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

async fn used_chars(db: &SqlitePool, agent: &str) -> usize {
    sqlx::query_scalar::<_, Option<i64>>("SELECT SUM(length(text) + 10) FROM memories WHERE tier = 'short' AND agent = $1")
        .bind(agent)
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
    let agent = crate::agents::of_session(db, session).await?;
    let agent = agent.as_str();
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
    // Only the owner's exact words count as theirs.
    let mut note = "";
    if source == "owner" && !text.is_empty() {
        let said: bool = sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM tape_events WHERE session_id=$1 AND {OWNER_MESSAGE} AND instr(payload->>'content',$2)>0)"))
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
                        agent,
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
            let id: i64 = sqlx::query_scalar("INSERT INTO memories (text, source, session_id, record_type, agent) VALUES ($1, $2, $3, $4, $5) RETURNING id")
                .bind(&text)
                .bind(source)
                .bind(session)
                .bind(kind)
                .bind(agent)
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
            let n = sqlx::query("UPDATE memories SET text = $2, source = $3, session_id = $4, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), used_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), uses = uses + 1 WHERE id = $1 AND tier = 'short' AND agent = $5")
                .bind(id)
                .bind(&text)
                .bind(source)
                .bind(session)
                .bind(agent)
                .execute(db)
                .await?
                .rows_affected();
            ensure!(n == 1, "no short-term memory m{id}");
            format!("Replaced m{id}{note}.")
        }
        "remove" => {
            let id = parse_id(&args["id"]).ok_or_else(|| anyhow::anyhow!("remove needs the entry's `id`, e.g. m12"))?;
            let _one = WRITING.lock().await;
            let n = sqlx::query("UPDATE memories SET tier = 'archived', reason = 'removed by the agent', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') WHERE id = $1 AND tier = 'short' AND agent = $2")
                .bind(id)
                .bind(agent)
                .execute(db)
                .await?
                .rows_affected();
            ensure!(n == 1, "no short-term memory m{id}");
            format!("Removed m{id} (archived, not deleted).")
        }
        other => anyhow::bail!("action must be add, replace or remove, not `{other}`"),
    };
    export(db, agent).await?;
    let used = used_chars(db, agent).await;
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
    let agent = crate::agents::of_session(db, session).await?;
    let said: Option<String> = sqlx::query_scalar(&format!("SELECT payload->>'content' FROM tape_events WHERE session_id=$1 AND {OWNER_MESSAGE} ORDER BY seq DESC LIMIT 1"))
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
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memories WHERE text=$1 AND tier<>'archived' AND agent=$2)")
                .bind(&text)
                .bind(&agent)
                .fetch_one(db)
                .await?;
        if !known {
            sqlx::query("INSERT INTO memories (text, source, session_id, agent) VALUES ($1, 'owner', $2, $3)")
                .bind(&text)
                .bind(session)
                .bind(&agent)
                .execute(db)
                .await?;
            saved += 1;
        }
    }
    if saved > 0 {
        export(db, &agent).await?;
        if used_chars(db, &agent).await > cap() {
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
pub async fn morning_note(db: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    let r = sqlx::query("SELECT ended_at, note FROM sleep_runs WHERE ended_at > strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 day') AND error IS NULL ORDER BY id DESC LIMIT 1")
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
pub async fn last_run(db: &SqlitePool) -> Result<Value, sqlx::Error> {
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
pub async fn list(db: &SqlitePool, tier: &str) -> Result<Value, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, text, source, tier, reason, created_at, updated_at FROM memories
         WHERE $1 = 'all' OR tier = $1 ORDER BY id DESC LIMIT 500",
    )
    .bind(tier)
    .fetch_all(db)
    .await?;
    Ok(Value::Array(
        rows.iter()
            .map(|r| {
                json!({ "id": format!("m{}", r.get::<i64, _>("id")), "text": r.get::<String, _>("text"), "source": r.get::<String, _>("source"),
                        "tier": r.get::<String, _>("tier"), "reason": r.get::<Option<String>, _>("reason"),
                        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at") })
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: i64, text: &str) -> Entry {
        Entry { id, text: text.into() }
    }

    #[test]
    fn render_keeps_newest_entries_when_over_the_size() {
        let entries: Vec<Entry> = (1..=10).map(|i| e(i, &"x".repeat(40))).collect();
        let out = render_entries(&entries, 200);
        assert!(out.contains("[m10]") && !out.contains("[m1]") && out.contains("older entries are left out"));
        assert_eq!(render_entries(&entries[..2], 200).lines().count(), 2);
    }

    #[tokio::test]
    async fn each_agent_has_its_own_short_term_memory() {
        let (dir, db) = crate::test_util::database().await;
        for (text, agent) in [("zen fact", "zenbot"), ("garry fact", "garry")] {
            sqlx::query("INSERT INTO memories (text, source, agent) VALUES ($1, 'owner', $2)").bind(text).bind(agent).execute(&db).await.unwrap();
        }
        let texts = |v: Vec<Entry>| v.into_iter().map(|e| e.text).collect::<Vec<_>>();
        assert_eq!(texts(short_entries(&db, "zenbot").await.unwrap()), ["zen fact"]);
        assert_eq!(texts(short_entries(&db, "garry").await.unwrap()), ["garry fact"]);
        // Each file holds its agent's rows, and a hand edit changes only that agent's memory.
        let (zf, gf) = (dir.join("zen/MEMORY.md"), dir.join("garry/MEMORY.md"));
        write_file(&db, "zenbot", &zf).await.unwrap();
        write_file(&db, "garry", &gf).await.unwrap();
        let g = std::fs::read_to_string(&gf).unwrap();
        assert!(g.contains("garry's short-term memory") && g.contains("garry fact") && !g.contains("zen fact"), "{g}");
        assert!(std::fs::read_to_string(&zf).unwrap().starts_with("# MEMORY.md\n\nzenbot's"));
        std::fs::write(&gf, "- a new garry line\n").unwrap();
        import_file(&db, "garry", &gf).await.unwrap();
        assert_eq!(texts(short_entries(&db, "garry").await.unwrap()), ["a new garry line"], "the removed line is archived");
        assert_eq!(texts(short_entries(&db, "zenbot").await.unwrap()), ["zen fact"], "zenbot's untouched");
        import_file(&db, "zenbot", &zf).await.unwrap();
        assert_eq!(texts(short_entries(&db, "zenbot").await.unwrap()), ["zen fact"]);
        assert_eq!((used_chars(&db, "zenbot").await, used_chars(&db, "nobody").await), (18, 0));
    }

    #[test]
    fn lines_read_back() {
        assert_eq!(parse_line(&line(&e(12, "plain"))), Some((12, "plain".into())));
        assert_eq!(parse_line("- a new line"), None);
    }
}
