//! The tape: each session's append-only chain of blocks (docs/context.md).
//!
//! A block has a number within its session (`seq`, the address the model sees as `#12`), a link to
//! the previous block, and a hash over the parent's hash, its kind and its payload, computed by the
//! database (`zen_block_hash`, migrations/0007) so every writer hashes the same canonical JSON.
//! D-062: risk/origin are kernel-owned columns beside (not inside) the historical payload hash.
//! Session risk is a conservative maximum, reset only by owner trust; derived blocks carry it.
//! Tool result provenance requires the kernel's output digest; only append_owner assigns risk 0.

use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// One block as read back from the tape.
#[derive(Clone, Debug)]
pub struct Block {
    pub seq: i32,
    pub kind: String,
    pub payload: Value,
}

/// Append a block at the end of a session's chain. Appends to one session are serialized with an
/// advisory lock, so concurrent writers (tool calls, notifications) can't take the same number.
/// Returns the new block's number and hash.
pub async fn append(db: &PgPool, session: Uuid, kind: &str, payload: &Value) -> Result<(i32, String), sqlx::Error> {
    let mut tx = db.begin().await?;
    let result = append_in(&mut tx, session, kind, payload).await?;
    tx.commit().await?;
    crate::session_state::invalidate();
    Ok(result)
}

/// Only the authenticated owner turn path calls this. A worker cannot claim owner provenance.
#[cfg(test)]
pub async fn append_owner(db: &PgPool, session: Uuid, payload: &Value) -> Result<(i32, String), sqlx::Error> {
    append_owner_queued(db, session, payload, None).await
}

pub async fn append_owner_queued(db: &PgPool, session: Uuid, payload: &Value, queued: Option<i64>) -> Result<(i32, String), sqlx::Error> {
    let mut tx = db.begin().await?;
    let (seq, hash) = append_in(&mut tx, session, "message", payload).await?;
    if let Some(queued) = queued {
        let deleted = sqlx::query("DELETE FROM pending_prompts WHERE id=$1 AND session_id=$2 AND error IS NULL")
            .bind(queued).bind(session).execute(&mut *tx).await?.rows_affected();
        if deleted != 1 { return Err(sqlx::Error::RowNotFound); }
    }
    sqlx::query("UPDATE tape_events SET risk=0, origin='owner', tainted=false WHERE session_id=$1 AND seq=$2")
        .bind(session).bind(seq).execute(&mut *tx).await?;
    tx.commit().await?;
    crate::session_state::invalidate();
    Ok((seq,hash))
}

/// Append in the caller's transaction, so an approval and its audit become visible together.
pub async fn append_in(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, session: Uuid, kind: &str, payload: &Value) -> Result<(i32, String), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 7))").bind(session).execute(&mut **tx).await?;
    // The kernel, not worker payload fields, selects provenance under the chain lock.
    let state = sqlx::query("SELECT risk,kind FROM sessions WHERE id=$1 FOR UPDATE")
        .bind(session).fetch_one(&mut **tx).await?;
    let mut risk: i16 = state.get("risk");
    let mut origin = "derived-session".to_string();
    let mut tool_call_record: Option<i64> = None;
    if kind == "message" && payload["role"] == "toolResult" {
        // The worker cannot lower a rating by inserting risk/origin fields into its message. An
        // error the engine wrote itself (an argument it refused, a call cut off at the turn's
        // end) carries no outside content: it keeps the session's level instead of raising it to
        // 4 (D-067); any other result the kernel didn't produce counts as unknown.
        let engine_error = payload["isError"].as_bool().unwrap_or(false);
        let session_risk = risk;
        risk = if engine_error { session_risk } else { 4 };
        origin = if engine_error { "engine-error".into() } else { "unverified-tool-result".into() };
        let text = zen_proto::text_of(&payload["content"]);
        let rated = sqlx::query("SELECT id,risk,origin FROM tool_calls WHERE session_id=$1 AND call_id=$2 AND name=$3 AND is_error=$5 AND result_sha256=encode(sha256(convert_to($4,'UTF8')),'hex') ORDER BY id DESC LIMIT 1")
            .bind(session).bind(payload["toolCallId"].as_str().unwrap_or("")).bind(payload["toolName"].as_str().unwrap_or(""))
            .bind(text).bind(payload["isError"].as_bool().unwrap_or(false)).fetch_optional(&mut **tx).await?;
        if let Some(r) = rated {
            tool_call_record = Some(r.get("id"));
            risk = r.get::<Option<i16>,_>("risk").unwrap_or(risk);
            origin = r.get::<Option<String>,_>("origin").unwrap_or(origin);
        }
    }
    let row = sqlx::query(
        "WITH last AS (SELECT id, seq, hash FROM tape_events WHERE session_id = $1 AND seq IS NOT NULL ORDER BY seq DESC LIMIT 1)
         INSERT INTO tape_events (session_id, kind, payload, seq, parent, hash, risk, origin, tool_call_record)
         SELECT $1, $2, $3, COALESCE((SELECT seq FROM last), 0) + 1, (SELECT id FROM last), zen_block_hash((SELECT hash FROM last), $2, $3), $4, $5, $6
         RETURNING seq, hash",
    )
    .bind(session)
    .bind(kind)
    .bind(payload)
    .bind(risk)
    .bind(origin)
    .bind(tool_call_record)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query("UPDATE tape_events SET tainted=zen_session_contaminated(session_id) WHERE session_id=$1 AND seq=$2")
        .bind(session).bind(row.get::<i32,_>("seq")).execute(&mut **tx).await?;
    sqlx::query("UPDATE sessions SET updated_at = now(), risk=GREATEST(risk,$2) WHERE id = $1").bind(session).bind(risk).execute(&mut **tx).await?;
    Ok((row.get("seq"), row.get("hash")))
}

/// A session's blocks of the given kinds, in order.
pub async fn load(db: &PgPool, session: Uuid, kinds: &[&str]) -> Result<Vec<Block>, sqlx::Error> {
    let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
    let rows = sqlx::query("SELECT seq, kind, payload FROM tape_events WHERE session_id = $1 AND kind = ANY($2) ORDER BY seq")
        .bind(session)
        .bind(&kinds)
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Block { seq: r.get::<Option<i32>, _>("seq").unwrap_or(0), kind: r.get("kind"), payload: r.get("payload") })
        .collect())
}

/// All of a session's blocks, in order: read once per turn start and derived from (compile.rs, turns.rs).
pub async fn load_all(db: &PgPool, session: Uuid) -> Result<Vec<Block>, sqlx::Error> {
    let rows = sqlx::query("SELECT seq, kind, payload FROM tape_events WHERE session_id = $1 AND seq IS NOT NULL ORDER BY seq")
        .bind(session)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(|r| Block { seq: r.get("seq"), kind: r.get("kind"), payload: r.get("payload") }).collect())
}

/// Number and link blocks written without a number (by an older build after a rollback).
pub async fn repair(db: &PgPool) -> Result<u64, sqlx::Error> {
    let r = sqlx::query("SELECT zen_rechain(session_id) FROM (SELECT DISTINCT session_id FROM tape_events WHERE seq IS NULL) s")
        .execute(db)
        .await?;
    Ok(r.rows_affected())
}

/// Maintain private JSONL history files from the committed tape. The database remains authoritative;
/// a failed export is retried, and every restart repairs the whole mirror. Atomic replacement means
/// readers see a complete session, never a partial JSON line or an out-of-order concurrent append.
pub async fn mirror_loop(db: PgPool) {
    // Rebuild missing or manually damaged exports on restart as well as resuming dirty rows.
    let _ = sqlx::query("INSERT INTO tape_exports(session_id) SELECT DISTINCT session_id FROM tape_events ON CONFLICT DO NOTHING").execute(&db).await;
    loop {
        if let Err(e) = mirror_pending(&db).await {
            tracing::warn!("exporting session histories: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn mirror_pending(db: &PgPool) -> anyhow::Result<()> {
    let pending: Vec<(Uuid, i64)> = sqlx::query_as("SELECT session_id,version FROM tape_exports ORDER BY session_id LIMIT 100").fetch_all(db).await?;
    let dir = crate::zen_home().join("sessions");
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    for (session, version) in pending {
        let rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(e) FROM tape_events e WHERE session_id=$1 ORDER BY seq")
            .bind(session).fetch_all(db).await?;
        let path = dir.join(format!("{session}.jsonl"));
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let tmp = path.with_extension("jsonl.tmp");
            let mut file = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&tmp)?;
            for row in rows {
                serde_json::to_writer(&mut file, &row)?;
                file.write_all(b"\n")?;
            }
            file.sync_all()?;
            std::fs::rename(tmp, path)
        }).await??;
        sqlx::query("DELETE FROM tape_exports WHERE session_id=$1 AND version=$2").bind(session).bind(version).execute(db).await?;
    }
    Ok(())
}
