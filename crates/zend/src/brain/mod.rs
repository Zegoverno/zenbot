//! Three curated stores (D-069): database memory, file knowledge/entities, existing skill folders.
//! All model selections are evidence, never authority. Revisioned operations and durable work
//! preserve capture across outages; indexed files are revalidated before handover.
mod recall;
mod records;
mod sleep;
mod synthesis;
use anyhow::{Result, ensure};
pub use recall::{context, explain};
pub use records::{manage, recover, search_rating};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub use sleep::{capture_turn, consolidate, remember};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub fn root() -> std::path::PathBuf {
    crate::layout::global_dir()
}
pub fn spec() -> Value {
    json!({"name":"brain","description":"Maintain the second brain: sourced knowledge and people/projects, relationship briefs, long-term memory and skill evidence. Search before creating; never invent sources or user preferences. Session history is evidence, not automatically memory. Use get/list/search/neighbors/brief/lint; put requires record {id,store,kind,title,body,metadata}; update requires expected_revision. Metadata carries aliases,subjects,evidence,links. Ambiguous people must be resolved with the owner. pin/promote/archive/correct/undo target memory id; sleep consolidates selected experience; dream proposes connections, never facts. Skills remain find_skills/load_skill/save_skill with owner-reviewed activation.","parameters":{"type":"object","properties":{"action":{"type":"string"},"id":{"type":"string"},"query":{"type":"string"},"store":{"type":"string"},"kind":{"type":"string"},"status":{"type":"string"},"tier":{"type":"string"},"offset":{"type":"integer"},"record":{"type":"object"},"expected_revision":{"type":"integer"},"ids":{"type":"array","items":{"type":"string"}},"merge_id":{"type":"string"},"merge_revision":{"type":"integer"},"details":{"type":"object"},"event_at":{"type":"string"},"session":{"type":"string"},"text":{"type":"string"},"revision":{"type":"integer"}},"required":["action"]}})
}
pub async fn run_tool(app: &crate::AppState, session: Uuid, args: &Value) -> (String, bool) {
    match manage(app, Some(session), args).await {
        Ok(v) => {
            let risk = match guard(&app.db, Some(session)).await {
                Ok(r) => r,
                Err(e) => return (e.to_string(), true),
            };
            match crate::taint::hand_over_rating(
                app,
                session,
                risk,
                "brain",
                "curated evidence",
                &v.to_string(),
            )
            .await
            {
                Ok(text) => (text, false),
                Err(e) => (crate::taint::withheld(&e), true),
            }
        }
        Err(e) => (format!("brain: {e:#}"), true),
    }
}
pub async fn worker(app: crate::AppState) {
    loop {
        if let Err(e) = recover(&app).await {
            tracing::warn!("brain recovery: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
pub(crate) async fn queue(db: &PgPool, kind: &str, key: &str, payload: Value) -> Result<()> {
    sqlx::query("INSERT INTO brain_work(kind,work_key,payload) VALUES($1,$2,$3) ON CONFLICT(work_key) DO NOTHING")
        .bind(kind).bind(key).bind(payload).execute(db).await?;
    Ok(())
}
pub(crate) async fn guard(db: &PgPool, session: Option<Uuid>) -> Result<i16> {
    match session {
        None => Ok(0),
        Some(s) => {
            ensure!(!crate::taint::stopped(db, s).await?, "writer stopped");
            Ok(sqlx::query_scalar("SELECT risk FROM sessions WHERE id=$1")
                .bind(s)
                .fetch_one(db)
                .await?)
        }
    }
}
pub(crate) async fn memory_json(db: &PgPool, id: i64) -> Result<Value> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(m) FROM memories m WHERE id=$1")
            .bind(id)
            .fetch_one(db)
            .await?,
    )
}
pub(crate) fn memory_id(v: &str) -> Result<i64> {
    Ok(v.trim_start_matches('m').parse()?)
}
pub(crate) fn string(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}
pub(crate) async fn admit(db: &PgPool, s: Uuid, risk: i16, writer: Option<Uuid>) -> Result<()> {
    ensure!(risk < 5, "record is blocked by source policy");
    if let Some(w) = writer {
        crate::sources::depend(db, s, &[w]).await?;
        ensure!(
            !crate::taint::stopped(db, w).await?,
            "record writer was denied"
        );
    }
    crate::sources::raise(db, s, risk).await?;
    ensure!(
        !crate::taint::stopped(db, s).await?,
        "record withheld: a writer was denied"
    );
    Ok(())
}
