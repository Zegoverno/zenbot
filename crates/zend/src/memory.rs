//! Short-term memory admission and owner views. Lifecycle and sleep live in brain/sleep.rs
//! (D-069); no automatic promotion writes prompt files. Historical fixed bases replay unchanged.
use anyhow::Result;
use serde_json::{json,Value};
use sqlx::{PgPool,Row};
use uuid::Uuid;
use crate::AppState;
pub fn cap()->usize{crate::config::get().memory_chars}
fn ceiling()->usize{cap()*2}
struct Entry {id:i64,text:String,source:String,tainted:bool,writer:Option<Uuid>,risk:i16}
fn line(e:&Entry)->String{format!("- [m{}] {}\n",e.id,crate::context::label(&format!("memory m{} ({})",e.id,e.source),e.risk,&e.text).replace('\n'," "))}
async fn short_entries(db:&PgPool)->Result<Vec<Entry>>{
 let rows=sqlx::query("SELECT id,text,source,session_id,risk,(risk>=4 OR zen_memory_contaminated(id)) AS tainted FROM memories WHERE tier='short' ORDER BY id").fetch_all(db).await?;
 Ok(rows.iter().map(|r|Entry{id:r.get("id"),text:r.get("text"),source:r.get("source"),tainted:r.get("tainted"),writer:r.get("session_id"),risk:r.get("risk")}).collect())
}
fn render_entries(entries:&[Entry],cap:usize)->String{let mut out=String::new();for e in entries{let l=line(e);if out.chars().count()+l.chars().count()<=cap{out.push_str(&l);}}out}
pub async fn render(db: &PgPool, reader: Uuid) -> Result<String> {
    let entries = short_entries(db).await?;
    let (marked, clean): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|e| e.tainted);
    let writers: Vec<Uuid> = clean.iter().filter_map(|e| e.writer).collect();
    crate::sources::depend(db, reader, &writers).await?;
    let ids:Vec<i64>=clean.iter().map(|e|e.id).collect();
    let all_writers:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT writer FROM brain_memory_inputs WHERE memory_id=ANY($1)").bind(&ids).fetch_all(db).await?;
    crate::sources::depend(db,reader,&all_writers).await?;
    crate::sources::raise(db, reader, clean.iter().map(|e|e.risk).max().unwrap_or(1)).await?;
    anyhow::ensure!(!crate::taint::stopped(db, reader).await?, "memories withheld: a writer was stopped");
    let mut out = render_entries(&clean, ceiling());
    out.push_str(&left_out(&marked));
    Ok(out)
}

/// The line that names the entries left out of the instructions as untrusted, if any.
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

pub fn spec()->Value{json!({"name":"remember","description":"Save selected experience, decisions, commitments and open questions to bounded short-term memory. Explicit owner facts require exact owner evidence; otherwise use inferred. Sleep consolidates durable experience into long-term memory; nothing is silently discarded. Use brain for correction/promotion/archive with expected_revision. Session history is separately searchable evidence.","parameters":{"type":"object","properties":{"action":{"type":"string","enum":["add","replace","remove"]},"text":{"type":"string"},"id":{"type":"string"},"source":{"type":"string","enum":["owner","verified","inferred"]},"kind":{"type":"string","enum":["experience","decision","preference","commitment","project","meeting","question"]},"entities":{"type":"array","items":{"type":"string"}},"pinned":{"type":"boolean"},"expected_revision":{"type":"integer"}},"required":["action"]}})}
pub async fn run_tool(app:&AppState,s:Uuid,a:&Value)->(String,bool){match crate::brain::remember(app,s,a).await{Ok(v)=>(v,false),Err(e)=>(e.to_string(),true)}}
pub async fn sleep(app:&AppState,trigger:&str)->Result<Value>{crate::brain::consolidate(app,trigger).await}
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

pub(crate) const BACKUPS: &str = "backups/prompt-files";

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
        "SELECT id, text, source, tier, proposed, reason, created_at, updated_at, (risk>=4 OR zen_memory_contaminated(id)) AS tainted FROM memories
         WHERE $1 = 'all' OR tier = $1 OR ($1 = 'proposed' AND proposed = 'promote') ORDER BY id DESC LIMIT 500",
    )
        .bind(tier)
        .fetch_all(db)
        .await?;
    Ok(Value::Array(
        rows.iter()
            .map(|r| {
                json!({ "id": format!("m{}", r.get::<i64, _>("id")), "text": r.get::<String, _>("text"), "source": r.get::<String, _>("source"), "untrusted": r.get::<bool, _>("tainted"),
                        "tier": r.get::<String, _>("tier"), "proposed": r.get::<Option<String>, _>("proposed"), "reason": r.get::<Option<String>, _>("reason"),
                        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at") })
            })
            .collect(),
    ))
}
