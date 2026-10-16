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
    Ok(result)
}

/// Only the authenticated owner turn path calls this. A worker cannot claim owner provenance.
pub async fn append_owner(db: &PgPool, session: Uuid, payload: &Value) -> Result<(i32, String), sqlx::Error> {
    let mut tx = db.begin().await?;
    let (seq, hash) = append_in(&mut tx, session, "message", payload).await?;
    sqlx::query("UPDATE tape_events SET risk=0, origin='owner', tainted=false WHERE session_id=$1 AND seq=$2")
        .bind(session).bind(seq).execute(&mut *tx).await?;
    tx.commit().await?;
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
        // The worker cannot lower a rating by inserting risk/origin fields into its message.
        risk = 4;
        origin = "unverified-tool-result".into();
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
