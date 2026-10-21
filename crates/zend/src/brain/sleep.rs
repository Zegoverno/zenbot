//! The sleep (nightly, at once when short-term memory passes its size, or by hand): each
//! short-term memory is promoted to long-term memory (a record file of its own) when System One
//! judges it lasting (`memory_durable`), archived when it is clearly finished
//! (`memory_disposition`), and otherwise kept for the next sleep. A memory whose writer was denied
//! is archived without a model. Nothing is deleted, no prompt file is touched, and a memory changed
//! while the sleep ran is left as it is now. A promoted record cites its memory, so it carries the
//! memory's rating and writers.
use super::*;

fn basis(source: &str) -> &'static str {
    match source {
        "owner" => "owner_statement",
        "verified" => "observed",
        _ => "inference",
    }
}

/// Write a long-term memory record from a memory's text. `session` writes it (`remember` past the
/// high bar), or the kernel does (the sleep, citing the memory in `evidence`). Returns its id.
pub async fn long_term(
    db: &PgPool,
    session: Option<Uuid>,
    text: &str,
    kind: &str,
    source: &str,
    cite: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    let words: String = crate::wiki::slug(&zen_proto::head(text, 60)).chars().take(48).collect();
    let base = format!("{}-{}", at.format("%Y-%m-%d"), words.trim_end_matches('-'));
    let mut id = base.clone();
    for n in 2.. {
        let taken: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM brain_records WHERE id=$1)")
            .bind(&id)
            .fetch_one(db)
            .await?;
        if !taken && !records::path("memory", kind, &id).exists() {
            break;
        }
        id = format!("{base}-{n}");
    }
    let title = zen_proto::head(&text.replace('\n', " "), 80);
    let record = json!({"id": id, "store": "memory", "kind": kind, "title": title, "body": text,
        "metadata": {"evidence": [{"source": cite, "locator": format!("short-term memory, {}", at.format("%Y-%m-%d")), "basis": basis(source)}]}});
    put(db, session, &record, None, true).await?;
    Ok(id)
}

pub async fn consolidate(app: &crate::AppState, trigger: &str) -> Result<Value> {
    let db = &app.db;
    let mut lock = db.begin().await?;
    let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(704,6)")
        .fetch_one(&mut *lock)
        .await?;
    ensure!(free, "a sleep is already running");
    let run: i64 = sqlx::query_scalar("INSERT INTO sleep_runs(trigger,scorer) VALUES($1,$2) RETURNING id")
        .bind(trigger)
        .bind(crate::s1::default_model())
        .fetch_one(db)
        .await?;
    let result = sleep(db, run).await;
    if let Err(e) = &result {
        sqlx::query("UPDATE sleep_runs SET ended_at=now(),error=$2 WHERE id=$1")
            .bind(run)
            .bind(format!("{e:#}"))
            .execute(db)
            .await?;
    }
    lock.rollback().await?;
    result
}

type Row = (
    i64,
    String,
    String,
    String,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
    bool,
);

async fn sleep(db: &PgPool, run: i64) -> Result<Value> {
    crate::memory::import(db).await?;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, text, source, record_type, created_at, updated_at, zen_memory_contaminated(id) FROM memories WHERE tier='short' ORDER BY id",
    )
    .fetch_all(db)
    .await?;
    let archive = |id: i64, at: chrono::DateTime<chrono::Utc>, reason: String| {
        sqlx::query("UPDATE memories SET tier='archived',reason=$3,updated_at=now() WHERE id=$1 AND updated_at=$2 AND tier='short'").bind(id).bind(at).bind(reason).execute(db)
    };
    let (mut promoted, mut archived, mut kept, mut changed) = (0, 0, 0, 0);
    let (denied, rows): (Vec<Row>, Vec<Row>) = rows.into_iter().partition(|r| r.6);
    for r in &denied {
        archived += archive(r.0, r.5, "a session that wrote it was denied".into())
            .await?
            .rows_affected();
    }
    let now = chrono::Utc::now();
    for batch in rows.chunks(12) {
        let items: Vec<Value> = batch
            .iter()
            .map(|r| json!({"memory": r.1, "kind": r.3, "source": r.2, "age_days": (now - r.4).num_days()}))
            .collect();
        let (durable, disposition) = tokio::join!(
            crate::s1::decide_items(db, "memory_durable", crate::s1::Facts::new(None), items.clone()),
            crate::s1::decide_items(db, "memory_disposition", crate::s1::Facts::new(None), items)
        );
        for (i, r) in batch.iter().enumerate() {
            let fresh: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM memories WHERE id=$1 AND updated_at=$2 AND tier='short')",
            )
            .bind(r.0)
            .bind(r.5)
            .fetch_one(db)
            .await?;
            if !fresh {
                changed += 1;
            } else if durable.get(i).is_some_and(|d| d.is("promote")) {
                match long_term(db, None, &r.1, &r.3, &r.2, &format!("m{}", r.0), r.4).await {
                    Ok(id) => {
                        archive(r.0, r.5, format!("promoted to long-term memory {id}")).await?;
                        promoted += 1;
                    }
                    Err(e) => {
                        tracing::warn!("promoting m{}: {e:#}", r.0);
                        kept += 1;
                    }
                }
            } else if disposition.get(i).is_some_and(|d| d.is("archive")) {
                archived += archive(r.0, r.5, "the sleep: finished".into()).await?.rows_affected();
            } else {
                kept += 1;
            }
        }
    }
    let note = format!(
        "{promoted} promoted to long-term memory, {archived} archived, {kept} kept, {changed} changed during the sleep and left as they are."
    );
    sqlx::query("UPDATE sleep_runs SET ended_at=now(),entries=$2,kept=$3,promoted=$4,dropped=$5,note=$6 WHERE id=$1")
        .bind(run)
        .bind((denied.len() + rows.len()) as i32)
        .bind(kept)
        .bind(promoted)
        .bind(archived as i32)
        .bind(&note)
        .execute(db)
        .await?;
    crate::memory::export(db).await?;
    Ok(
        json!({"run": run, "entries": denied.len() + rows.len(), "promoted": promoted, "archived": archived, "kept": kept, "changed": changed, "note": note}),
    )
}
