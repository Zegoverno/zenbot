//! The tape: each session's append-only chain of blocks (docs/context.md).
//!
//! A block has a number within its session (`seq`, the address the model sees as `#12`), a link to
//! the previous block, and a hash over the parent's hash, its kind and its payload, computed over the exact saved `hash_payload` bytes. Imported blocks retain PostgreSQL JSONB
//! bytes (hash version 1); new blocks use serde_json canonical object order (version 2).
//! D-062: risk/origin are kernel-owned columns beside (not inside) the historical payload hash.
//! Session risk is a conservative maximum, reset only by owner trust; derived blocks carry it.
//! Tool result provenance requires the kernel's output digest; only the owner append path assigns
//! risk 0. Committed changes queue private JSONL mirror exports, retried across restarts.

use serde_json::Value;
use sqlx::{SqlitePool, Row};
use uuid::Uuid;

/// One block as read back from the tape.
#[derive(Clone, Debug)]
pub struct Block {
    pub seq: i32,
    pub kind: String,
    pub payload: Value,
}

/// Append a block at the end of a session's chain. Appends to one session are serialized with an
/// immediate SQLite transaction, so concurrent writers cannot take the same number.
/// Returns the new block's number and hash.
pub async fn append(db: &SqlitePool, session: Uuid, kind: &str, payload: &Value) -> Result<(i32, String), sqlx::Error> {
    let mut tx = crate::storage::begin(db).await?;
    let result = append_in(&mut tx, session, kind, payload).await?;
    tx.commit().await?;
    crate::session_state::invalidate();
    Ok(result)
}

/// Only the authenticated owner turn path calls this. A worker cannot claim owner provenance.
#[cfg(test)]
pub async fn append_owner(db: &SqlitePool, session: Uuid, payload: &Value) -> Result<(i32, String), sqlx::Error> {
    append_owner_queued(db, session, payload, None).await
}

pub async fn append_owner_queued(db: &SqlitePool, session: Uuid, payload: &Value, queued: Option<i64>) -> Result<(i32, String), sqlx::Error> {
    let mut tx = crate::storage::begin(db).await?;
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
pub async fn append_in(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, session: Uuid, kind: &str, payload: &Value) -> Result<(i32, String), sqlx::Error> {
    // The kernel, not worker payload fields, selects provenance under the chain lock.
    let state = sqlx::query("SELECT risk,kind FROM sessions WHERE id=$1")
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
        let text = zen_proto::digest_text(&payload["content"]);
        let rated = sqlx::query("SELECT id,risk,origin FROM tool_calls WHERE session_id=$1 AND call_id=$2 AND name=$3 AND is_error=$5 AND result_sha256=sha256_hex($4) ORDER BY id DESC LIMIT 1")
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
         INSERT INTO tape_events (session_id, kind, payload, seq, parent, hash, risk, origin, tool_call_record, hash_payload, hash_version)
         SELECT $1, $2, $3, COALESCE((SELECT seq FROM last), 0) + 1, (SELECT id FROM last), sha256_hex(COALESCE((SELECT hash FROM last),'') || $2 || $3), $4, $5, $6, $3, 2
         RETURNING seq, hash",
    )
    .bind(session)
    .bind(kind)
    .bind(payload.to_string())
    .bind(risk)
    .bind(origin)
    .bind(tool_call_record)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query("UPDATE tape_events SET tainted=EXISTS(SELECT 1 FROM zen_contaminated_sessions c WHERE c.id=tape_events.session_id) WHERE session_id=$1 AND seq=$2")
        .bind(session).bind(row.get::<i32,_>("seq")).execute(&mut **tx).await?;
    sqlx::query("UPDATE sessions SET updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), risk=max(risk,$2) WHERE id = $1").bind(session).bind(risk).execute(&mut **tx).await?;
    Ok((row.get("seq"), row.get("hash")))
}

/// A session's blocks of the given kinds, in order.
pub async fn load(db: &SqlitePool, session: Uuid, kinds: &[&str]) -> Result<Vec<Block>, sqlx::Error> {
    let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
    let rows = sqlx::query("SELECT seq, kind, payload FROM tape_events WHERE session_id = $1 AND kind IN (SELECT value FROM json_each($2)) ORDER BY seq")
        .bind(session)
        .bind(serde_json::json!(kinds))
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Block { seq: r.get::<Option<i32>, _>("seq").unwrap_or(0), kind: r.get("kind"), payload: r.get("payload") })
        .collect())
}

/// All of a session's blocks, in order: read once per turn start and derived from (compile.rs, turns.rs).
pub async fn load_all(db: &SqlitePool, session: Uuid) -> Result<Vec<Block>, sqlx::Error> {
    let rows = sqlx::query("SELECT seq, kind, payload FROM tape_events WHERE session_id = $1 AND seq IS NOT NULL ORDER BY seq")
        .bind(session)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(|r| Block { seq: r.get("seq"), kind: r.get("kind"), payload: r.get("payload") }).collect())
}

/// Number and link blocks written without a number (by an older build after a rollback).
pub async fn repair(db: &SqlitePool) -> Result<u64, sqlx::Error> {
    // Migrated chains are immutable. Only a missing suffix may be numbered; changing a
    // historical parent/hash would destroy the evidence we verified during migration.
    let mut tx = crate::storage::begin(db).await?;
    let rows = sqlx::query("SELECT id,session_id,kind,payload FROM tape_events WHERE seq IS NULL ORDER BY id")
        .fetch_all(&mut *tx).await?;
    for row in &rows {
        let id: i64 = row.get("id");
        let session: Uuid = row.get("session_id");
        let last: Option<(i64,i32,String)> = sqlx::query_as("SELECT id,seq,hash FROM tape_events WHERE session_id=$1 AND seq IS NOT NULL ORDER BY seq DESC LIMIT 1")
            .bind(session).fetch_optional(&mut *tx).await?;
        if last.as_ref().is_some_and(|last| last.0 > id) {
            return Err(sqlx::Error::Protocol("cannot repair a hole inside a historical tape chain".into()));
        }
        let payload: Value = row.get("payload");
        let payload = payload.to_string();
        let kind: String = row.get("kind");
        let (parent,seq,parent_hash) = last.map(|(id,seq,hash)|(Some(id),seq,hash)).unwrap_or((None,0,String::new()));
        let hash = block_hash(&parent_hash,&kind,&payload);
        sqlx::query("UPDATE tape_events SET seq=$2,parent=$3,hash=$4,hash_payload=$5,hash_version=2 WHERE id=$1")
            .bind(id).bind(seq+1).bind(parent).bind(hash).bind(payload).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

fn block_hash(parent: &str, kind: &str, payload: &str) -> String {
    use sha2::{Digest,Sha256};
    let mut digest=Sha256::new();
    digest.update(parent.as_bytes());
    digest.update(kind.as_bytes());
    digest.update(payload.as_bytes());
    format!("{:x}",digest.finalize())
}

/// Maintain private JSONL history files from the committed tape. The database remains authoritative;
/// a failed export is retried, and every restart repairs the whole mirror. Atomic replacement means
/// readers see a complete session, never a partial JSON line or an out-of-order concurrent append.
pub async fn mirror_loop(db: SqlitePool) {
    // Rebuild missing or manually damaged exports on restart as well as resuming dirty rows.
    let _ = sqlx::query("INSERT INTO tape_exports(session_id) SELECT DISTINCT session_id FROM tape_events WHERE true ON CONFLICT DO NOTHING").execute(&db).await;
    loop {
        if let Err(e) = mirror_pending(&db).await {
            tracing::warn!("exporting session histories: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn mirror_pending(db: &SqlitePool) -> anyhow::Result<()> {
    let pending: Vec<(Uuid, i64)> = sqlx::query_as("SELECT session_id,version FROM tape_exports ORDER BY session_id LIMIT 100").fetch_all(db).await?;
    let dir = crate::zen_home().join("sessions");
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    for (session, version) in pending {
        let events = sqlx::query("SELECT * FROM tape_events WHERE session_id=$1 ORDER BY seq")
            .bind(session).fetch_all(db).await?;
        let rows: Vec<Value> = events.iter().map(|r| serde_json::json!({
            "id":r.get::<i64,_>("id"),"session_id":session,"kind":r.get::<String,_>("kind"),
            "payload":r.get::<Value,_>("payload"),"created_at":r.get::<String,_>("created_at"),
            "seq":r.get::<Option<i32>,_>("seq"),"parent":r.get::<Option<i64>,_>("parent"),
            "hash":r.get::<Option<String>,_>("hash"),"risk":r.get::<i16,_>("risk"),
            "origin":r.get::<String,_>("origin"),"tainted":r.get::<bool,_>("tainted"),
            "tool_call_record":r.get::<Option<i64>,_>("tool_call_record"),
            "legacy_risk":r.get::<Option<i16>,_>("legacy_risk"),
            "hash_payload":r.get::<Option<String>,_>("hash_payload"),"hash_version":r.get::<i32,_>("hash_version")
        })).collect();
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

#[cfg(test)]
mod sqlite_tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_append_preserves_chain_and_imported_hashes() -> anyhow::Result<()> {
        let (_dir, db) = crate::test_util::database().await;
        let session = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model) VALUES($1,'faux/smoke')")
            .bind(session).execute(&db).await?;
        // PostgreSQL's JSONB canonical text differs from serde_json. Retain its exact
        // bytes, including spaces, and continue from its hash without rechaining it.
        let legacy = r#"{"role": "user", "content": "old message"}"#;
        let legacy_hash = block_hash("", "message", legacy);
        sqlx::query("INSERT INTO tape_events(session_id,kind,payload,seq,hash,hash_payload,hash_version) VALUES($1,'message',$2,1,$3,$2,1)")
            .bind(session).bind(legacy).bind(&legacy_hash).execute(&db).await?;
        let mut tasks = tokio::task::JoinSet::new();
        for n in 0..32 {
            let db = db.clone();
            tasks.spawn(async move { append(&db, session, "message", &serde_json::json!({"n":n})).await });
        }
        while let Some(task) = tasks.join_next().await { task??; }
        let rows = sqlx::query("SELECT id,seq,parent,kind,hash,hash_payload,hash_version FROM tape_events WHERE session_id=$1 ORDER BY seq")
            .bind(session).fetch_all(&db).await?;
        assert_eq!(rows.len(), 33);
        let mut previous_id = None;
        let mut previous_hash = String::new();
        for (n, row) in rows.iter().enumerate() {
            assert_eq!(row.get::<i32,_>("seq"), n as i32+1);
            assert_eq!(row.get::<Option<i64>,_>("parent"), previous_id);
            let hash: String = row.get("hash");
            assert_eq!(hash, block_hash(&previous_hash,&row.get::<String,_>("kind"),&row.get::<String,_>("hash_payload")));
            assert_eq!(row.get::<i32,_>("hash_version"), if n==0 {1} else {2});
            previous_id=Some(row.get::<i64,_>("id"));
            previous_hash=hash;
        }
        assert_eq!(rows[0].get::<String,_>("hash"),legacy_hash);
        assert_eq!(repair(&db).await?,0);
        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn failed_owner_queue_claim_rolls_back_block_and_export() -> anyhow::Result<()> {
        let (_dir, db) = crate::test_util::database().await;
        let session = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model) VALUES($1,'faux/smoke')")
            .bind(session).execute(&db).await?;
        assert!(append_owner_queued(&db,session,&serde_json::json!({"role":"user","content":"hello"}),Some(-1)).await.is_err());
        let count: i64=sqlx::query_scalar("SELECT count(*) FROM tape_events").fetch_one(&db).await?;
        assert_eq!(count,0);
        let count: i64=sqlx::query_scalar("SELECT count(*) FROM tape_exports").fetch_one(&db).await?;
        assert_eq!(count,0);
        db.close().await;
        Ok(())
    }
}
