//! The tape: each session's append-only chain of blocks (docs/context.md).
//!
//! A block has a number within its session (`seq`, the address the model sees as `#12`), a link to
//! the previous block, and a hash over the parent's hash, its kind and its payload, computed by the
//! database (`zen_block_hash`, migrations/0007) so every writer hashes the same canonical JSON.

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
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 7))").bind(session).execute(&mut *tx).await?;
    let row = sqlx::query(
        "WITH last AS (SELECT id, seq, hash FROM tape_events WHERE session_id = $1 AND seq IS NOT NULL ORDER BY seq DESC LIMIT 1)
         INSERT INTO tape_events (session_id, kind, payload, seq, parent, hash)
         SELECT $1, $2, $3, COALESCE((SELECT seq FROM last), 0) + 1, (SELECT id FROM last), zen_block_hash((SELECT hash FROM last), $2, $3)
         RETURNING seq, hash",
    )
    .bind(session)
    .bind(kind)
    .bind(payload)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE sessions SET updated_at = now() WHERE id = $1").bind(session).execute(&mut *tx).await?;
    tx.commit().await?;
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

/// A session's last block, if any.
pub async fn last(db: &PgPool, session: Uuid) -> Result<Option<Block>, sqlx::Error> {
    let row = sqlx::query("SELECT seq, kind, payload FROM tape_events WHERE session_id = $1 AND seq IS NOT NULL ORDER BY seq DESC LIMIT 1")
        .bind(session)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| Block { seq: r.get("seq"), kind: r.get("kind"), payload: r.get("payload") }))
}

/// Number and link blocks written without a number (by an older build after a rollback).
pub async fn repair(db: &PgPool) -> Result<u64, sqlx::Error> {
    let r = sqlx::query("SELECT zen_rechain(session_id) FROM (SELECT DISTINCT session_id FROM tape_events WHERE seq IS NULL) s")
        .execute(db)
        .await?;
    Ok(r.rows_affected())
}
