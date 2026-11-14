//! The second brain's curated stores: short-term memory (`memory.rs`, one capped
//! `MEMORY.md` in every session's instructions), and record files: entities (people,
//! organizations, projects) and long-term memory, one Markdown file each, linked by typed
//! relationships (`records.rs`). The sleep moves lasting short-term memories into long-term ones
//! (`sleep.rs`). Knowledge lives in one place, the owner's wiki pages (`crate::wiki`): the `brain`
//! tool refuses new knowledge records, while existing ones are still read, listed, indexed and
//! linted. Nothing is pushed into a turn: the brain finds records and pages with `search` (which
//! follows record links) and reads or changes records with the `brain` tool. Session history stays
//! a searchable archive, not memory. The historical wiki job indexes and lints records.
//!
//! Each agent has its own: its short-term memory, and records in its vault (`agents::Agent::vault`).
//! An agent other than zenbot writes records only into its own vault and reads its own, plus the
//! global vault's when its `global` setting is read or write (even with write, `brain put` goes to its
//! own vault); zenbot and the owner see every vault. Nothing outside its scope shows through: an id
//! taken in another vault is just taken, links to records it can't see are left out, and a name
//! is unique within its vault.
mod records;
mod sleep;
use anyhow::{Result, ensure};
pub use records::{index, put, Scope};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub use sleep::{consolidate, long_term};
use sqlx::SqlitePool;
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

/// Why a new knowledge record is refused: knowledge has one source of truth, the wiki.
pub const KNOWLEDGE_IN_WIKI: &str = "knowledge lives in wiki pages now: write ~/.zenbot/global/wiki/<Title>.md following ~/.zenbot/global/AGENTS.md (Wiki); brain records are for people, organizations, projects and long-term memory";

pub fn spec() -> Value {
    json!({
        "name": "brain",
        "description": "Curated Markdown records: people, organizations, projects and long-term memory. Knowledge (concepts, sources, facts, solutions, decisions, ideas) is not a record: write wiki pages (~/.zenbot/global/wiki/<Title>.md, rules in ~/.zenbot/global/AGENTS.md) with read/write/edit; old knowledge records stay readable. Search before creating. get reads links; list filters; put creates/updates using expected_revision; schema explains format. New long-term memories use remember. Never invent sources; preserve uncertainty and disagreements; ask the owner to disambiguate people.",
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

/// A `brain` action for the agent (`session`) or the owner (none).
pub async fn manage(db: &SqlitePool, session: Option<Uuid>, a: &Value) -> Result<Value> {
    let id = string(a, "id");
    // Whose vault it writes to and which records it sees: the session's agent's, or all for the owner.
    let (vault, scope) = match session {
        Some(s) => {
            let who = crate::agents::load(&crate::agents::of_session(db, s).await?)?;
            (who.vault(), Scope::of(&who))
        }
        None => (root(), Scope::all()),
    };
    match string(a, "action").as_str() {
        "get" => {
            let (mut r, _) = records::read(db, &id, &scope).await?;
            let links: Vec<Value> = records::visible_links(db, &id, &scope)
                .await?
                .into_iter()
                .map(|(from, relation, to)| json!({"from": from, "relation": relation, "to": to}))
                .collect();
            r["links"] = json!(links);
            Ok(r)
        }
        "list" => {
            let rows = records::list(db, a, &scope).await?;
            let more = rows.len() == 50;
            let out: Vec<Value> = rows
                .into_iter()
                .map(|(id, store, kind, title, path)| json!({"id": id, "store": store, "kind": kind, "title": title, "path": path}))
                .collect();
            Ok(json!({"records": out, "more": more}))
        }
        "put" => {
            ensure!(string(&a["record"], "store") != "knowledge", "{KNOWLEDGE_IN_WIKI}");
            // The owner may write a new long-term memory; the agent asks `remember` (long: true).
            records::put(
                db,
                &vault,
                &scope,
                &a["record"],
                a["expected_revision"].as_i64(),
                session.is_none(),
            )
            .await
        }
        "schema" => Ok(json!({"schema": records::SCHEMA, "relations": records::RELATIONS})),
        "lint" => {
            ensure!(session.is_none(), "lint is the owner's");
            Ok(json!({"issues": records::lint(db).await?}))
        }
        "index" => {
            ensure!(session.is_none(), "index is the owner's");
            Ok(json!({"changed": records::index(db).await?}))
        }
        other => anyhow::bail!("unknown brain action `{other}`: get, list, put or schema"),
    }
}

/// Run the `brain` tool.
pub async fn run_tool(app: &crate::AppState, session: Uuid, args: &Value) -> (String, bool) {
    match manage(&app.db, Some(session), args).await {
        Ok(v) => (serde_json::to_string_pretty(&v).unwrap_or_default(), false),
        Err(e) => (format!("brain: {e:#}"), true),
    }
}

/// Search support: typed links touching these records, as (from, relation, to).
pub(crate) async fn links(db: &SqlitePool, ids: &[String]) -> Result<Vec<(String, String, String)>> {
    records::links(db, ids).await
}

/// The historical wiki job now checks record files. Only counts enter the morning note: record
/// paths and parse errors can contain untrusted text.
pub async fn tend(db: &SqlitePool) -> Result<Vec<String>> {
    records::index(db).await?;
    let n = records::lint(db).await?.len();
    Ok(if n == 0 { vec![] } else { vec![format!("brain: {n} record issues; run zen brain lint to inspect")] })
}
