//! The second brain's curated stores (D-069): short-term memory (`memory.rs`, one capped
//! `MEMORY.md` in every session's instructions), and record files: long-term memory, knowledge
//! (with how we know it) and entities (people, organizations, projects), one Markdown file each,
//! linked by typed relationships (`records.rs`). The sleep moves lasting short-term memories into
//! long-term ones (`sleep.rs`). Nothing is pushed into a turn: the brain finds records with
//! `search` (which follows their links) and reads or changes them with the `brain` tool. Session
//! history stays a searchable archive, not memory. Owner-only legacy migration preserves exact
//! originals and provenance (`legacy.rs`); the historical wiki job indexes and lints records.
mod legacy;
mod records;
mod sleep;
use anyhow::{Result, ensure};
pub use records::{index, put};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub use sleep::{consolidate, long_term};
use sqlx::PgPool;
use uuid::Uuid;

pub fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub fn root() -> std::path::PathBuf {
    crate::layout::global_dir()
}
/// Write a private file atomically (0600 in a 0700 folder), never through a symlink.
pub(crate) async fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    records::private_write(path, text).await
}
pub(crate) fn string(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

pub fn spec() -> Value {
    json!({
        "name": "brain",
        "description": "Your curated records, one Markdown file each: knowledge (concepts, claims, arguments, questions, sources, events, subjects, each with how we know it: its evidence and their basis), people, organizations and projects, and long-term memory. Find them with `search`, which follows their links. `get` reads one by id with its links; `list` by store, kind or tag; `put` creates or updates one (an update passes the expected_revision you read; a new long-term memory comes from `remember` instead); `schema` explains the format. Search before creating; never invent a source; keep disagreements and uncertainty; resolve an ambiguous person with the owner.",
        "parameters": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["get", "list", "put", "schema"] },
                "id": { "type": "string", "description": "A record's id (get)" },
                "record": { "type": "object", "description": "put: {id, store, kind, title, body, status, metadata: {tags, aliases, subjects, evidence, links}}" },
                "expected_revision": { "type": "integer", "description": "put on an existing record: the revision you read" },
                "store": { "type": "string", "enum": ["knowledge", "entity", "memory"] },
                "kind": { "type": "string" },
                "tag": { "type": "string" },
                "offset": { "type": "integer" }
            },
            "required": ["action"]
        }
    })
}

/// A `brain` action for the agent (`session`) or the owner (none): the result and the highest
/// rating of the record text in it.
pub async fn manage(db: &PgPool, session: Option<Uuid>, a: &Value) -> Result<(Value, i16)> {
    let id = string(a, "id");
    match string(a, "action").as_str() {
        "get" => {
            let (mut r, text, risk) = records::read(db, &id).await?;
            records::hand(db, session, &text, risk).await?;
            let links: Vec<Value> = records::links(db, std::slice::from_ref(&id))
                .await?
                .into_iter()
                .map(|(from, relation, to)| json!({"from": from, "relation": relation, "to": to}))
                .collect();
            r["links"] = json!(links);
            Ok((r, risk))
        }
        "list" => {
            let mut out = vec![];
            let mut top = 0;
            let rows = records::list(db, a).await?;
            let more = rows.len() == 50;
            for (id, store, kind, title, path) in rows {
                let Ok(text) = tokio::fs::read_to_string(&path).await else {
                    continue;
                };
                let risk = crate::sources::file_rating(db, std::path::Path::new(&path), &text).await?;
                // An id and title are made from the record's text: from a session that read
                // untrusted content, only where to look is listed, as `search` does (D-056).
                if risk >= 4 {
                    out.push(json!({"store": store, "kind": kind, "withheld": "written after reading untrusted content; search for it by name to read it"}));
                    continue;
                }
                records::hand(db, session, &text, risk).await?;
                top = top.max(risk);
                out.push(json!({"id": id, "store": store, "kind": kind, "title": title, "path": path}));
            }
            Ok((json!({"records": out, "more": more}), top))
        }
        "put" => {
            // The owner may write a new long-term memory; the agent asks `remember` (long: true).
            let r = records::put(
                db,
                session,
                &a["record"],
                a["expected_revision"].as_i64(),
                session.is_none(),
            )
            .await?;
            Ok((r, 0))
        }
        "schema" => Ok((json!({"schema": records::SCHEMA, "relations": records::RELATIONS}), 0)),
        "migrate_legacy" => {
            ensure!(session.is_none(), "migration is the owner's");
            Ok((legacy::migrate(db, a["apply"].as_bool().unwrap_or(false)).await?, 0))
        }
        "lint" => {
            ensure!(session.is_none(), "lint is the owner's");
            Ok((json!({"issues": records::lint(db).await?}), 0))
        }
        "index" => {
            ensure!(session.is_none(), "index is the owner's");
            Ok((json!({"changed": records::index(db).await?}), 0))
        }
        other => anyhow::bail!("unknown brain action `{other}`: get, list, put or schema"),
    }
}

/// Run the `brain` tool: records are handed over at their rating (wrapped from 2).
pub async fn run_tool(app: &crate::AppState, session: Uuid, args: &Value) -> (String, bool) {
    match manage(&app.db, Some(session), args).await {
        Ok((v, risk)) => {
            match crate::taint::hand_over_rating(
                app,
                session,
                risk,
                "brain",
                &string(args, "action"),
                &serde_json::to_string_pretty(&v).unwrap_or_default(),
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

/// Search support: a record's file, if it still has the indexed body, with its rating.
pub(crate) async fn current(db: &PgPool, id: &str, body: &str) -> Option<(String, i16)> {
    let (r, text, risk) = records::read(db, id).await.ok()?;
    (string(&r, "body") == body).then_some((text, risk))
}

/// Search support: typed links touching these records, as (from, relation, to).
pub(crate) async fn links(db: &PgPool, ids: &[String]) -> Result<Vec<(String, String, String)>> {
    records::links(db, ids).await
}

/// The historical wiki job now checks record files. Only counts enter the morning note: record
/// paths and parse errors can contain untrusted text.
pub async fn tend(db: &PgPool) -> Result<Vec<String>> {
    records::index(db).await?;
    let n = records::lint(db).await?.len();
    Ok(if n == 0 { vec![] } else { vec![format!("brain: {n} record issues; run zen brain lint to inspect")] })
}
