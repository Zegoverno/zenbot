//! Record files: entities (people, organizations, projects) and long-term memory. Knowledge
//! records (in kind folders under `wiki/`) are from before knowledge moved to the owner's wiki
//! pages: they are still parsed, indexed, linted and read, but `brain put` no longer creates them.
//!
//! One Markdown file per record, its metadata a JSON frontmatter (a valid YAML subset). The files
//! are the source of truth: `brain_records`, `brain_aliases`, `brain_links` and the search index
//! are rebuilt from them (`index`), so the owner edits a file in any editor and the next index
//! pass picks it up.
//!
//! Every vault holds records: the global one (zenbot's, `global/`) and each agent's
//! (`global/agents/<name>/`, memory and entities only). Ids are one namespace across vaults; a
//! record's owner is the vault its path is in (`Scope`).
use super::*;
use std::path::{Path, PathBuf};

// Serialize record filenames and revisions.
pub(super) static WRITING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub const KNOWLEDGE: &[&str] = &[
    "concept",
    "claim",
    "argument",
    "question",
    "synthesis",
    "source",
    "event",
    "subject",
];
pub const ENTITIES: &[&str] = &["person", "organization", "project"];
pub const MEMORY: &[&str] = &["experience", "decision", "preference", "commitment", "question"];
pub const RELATIONS: &[&str] = &[
    "part_of",
    "depends_on",
    "supports",
    "contradicts",
    "derived_from",
    "applies_to",
    "supersedes",
    "related_to",
    "participated_in",
    "works_at",
    "about",
    "produced",
    "describes",
    "distinguishes",
    "mentions",
];
const BASES: &[&str] = &[
    "owner_statement",
    "observed",
    "external_claim",
    "inference",
    "proof",
    "empirical",
    "hypothesis",
    "interpretation",
    "tradition",
];
pub const SCHEMA: &str = "Records are Markdown files with a JSON frontmatter between `---` lines: \
{\"id\",\"store\",\"kind\",\"title\",\"status\",\"revision\",\"metadata\":{\"tags\",\"aliases\",\"subjects\",\"evidence\",\"links\"}}, then the body. \
Stores and kinds: entity (person, organization, project) under entities/<people|organizations|projects>/; memory (experience, decision, \
preference, commitment, question) under memory/. Knowledge is not a record: it lives in wiki pages, ~/.zenbot/global/wiki/<Title>.md \
with YAML frontmatter (type: primitive, hub, source, fact, solution, decision or idea; optional aliases), written with read/write/edit \
following ~/.zenbot/global/AGENTS.md (Wiki). Old knowledge records (concept, claim, argument, question, synthesis, source, event, \
subject, under wiki/<kind folder>/) stay readable but can't be put. One canonical id per record (lowercase letters, digits, hyphens); aliases are other names. \
Evidence items: {source, locator, basis}; basis is owner_statement, observed, external_claim, inference, proof, empirical, hypothesis, \
interpretation or tradition: it says how we know, not that it is true. Links: {relation, target, evidence}; supports, contradicts and \
derived_from need evidence. Status: active, provisional, superseded, archived. Keep qualifications and competing claims in the body.";

pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn folder(kind: &str) -> &str {
    match kind {
        "person" => "people",
        "organization" => "organizations",
        "project" => "projects",
        "concept" => "concepts",
        "claim" => "claims",
        "argument" => "arguments",
        "question" => "questions",
        "synthesis" => "syntheses",
        "source" => "sources",
        "event" => "events",
        _ => "subjects",
    }
}

/// Where a record lives in the vault `vault`. Long-term memory is one flat folder; the others have
/// a folder per kind (old knowledge records only in the global wiki).
pub fn path(vault: &Path, store: &str, kind: &str, id: &str) -> PathBuf {
    match store {
        "memory" => vault.join("memory"),
        "entity" => vault.join("entities").join(folder(kind)),
        _ => crate::wiki::root().join(folder(kind)),
    }
    .join(format!("{id}.md"))
}

/// The record folders of one vault, with their store.
fn vault_folders(vault: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut out = vec![(vault.join("memory"), "memory")];
    out.extend(ENTITIES.iter().map(|k| (vault.join("entities").join(folder(k)), "entity")));
    out
}

/// Every folder that holds records, with its store: the global vault's, the old knowledge
/// folders, and every agent's vault.
fn folders() -> Vec<(PathBuf, &'static str)> {
    let mut out = vault_folders(&root());
    for (_, vault) in crate::agents::vaults() {
        out.extend(vault_folders(&vault));
    }
    let mut seen = vec![];
    for k in KNOWLEDGE {
        if !seen.contains(&folder(k)) {
            seen.push(folder(k));
            out.push((crate::wiki::root().join(folder(k)), "knowledge"));
        }
    }
    out
}

fn kinds(store: &str) -> &'static [&'static str] {
    match store {
        "memory" => MEMORY,
        "entity" => ENTITIES,
        _ => KNOWLEDGE,
    }
}

fn slugs(v: &Value, what: &str) -> Result<()> {
    ensure!(
        v.is_null()
            || v.as_array()
                .is_some_and(|a| a.iter().all(|s| s.as_str().is_some_and(valid_id))),
        "{what} must be an array of slugs (lowercase letters, digits, hyphens)"
    );
    Ok(())
}

pub fn validate(r: &Value) -> Result<()> {
    let store = string(r, "store");
    ensure!(
        valid_id(&string(r, "id")),
        "id: lowercase letters, digits and hyphens, 1..96 characters"
    );
    ensure!(
        matches!(store.as_str(), "knowledge" | "entity" | "memory"),
        "store must be knowledge, entity or memory"
    );
    ensure!(
        kinds(&store).contains(&string(r, "kind").as_str()),
        "kind must be one of {:?} for {store}",
        kinds(&store)
    );
    let title = string(r, "title");
    ensure!(
        !title.trim().is_empty() && title.len() <= 240,
        "title required, at most 240 bytes"
    );
    let body = string(r, "body");
    ensure!(
        !body.trim().is_empty() && body.len() <= 64 * 1024,
        "body required, at most 64 KiB"
    );
    ensure!(
        matches!(
            r["status"].as_str().unwrap_or("active"),
            "active" | "provisional" | "superseded" | "archived"
        ),
        "status must be active, provisional, superseded or archived"
    );
    let m = &r["metadata"];
    ensure!(m.is_null() || m.is_object(), "metadata must be an object");
    ensure!(m.to_string().len() <= 24000, "metadata exceeds 24k bytes");
    slugs(&m["tags"], "tags")?;
    slugs(&m["subjects"], "subjects")?;
    ensure!(
        m["aliases"].is_null()
            || m["aliases"]
                .as_array()
                .is_some_and(|a| a.iter().all(|s| s.as_str().is_some_and(|s| !s.trim().is_empty()))),
        "aliases must be an array of names"
    );
    for e in m["evidence"].as_array().into_iter().flatten() {
        ensure!(
            !string(e, "source").is_empty() && !string(e, "locator").is_empty(),
            "evidence needs source and locator"
        );
        ensure!(
            BASES.contains(&e["basis"].as_str().unwrap_or("external_claim")),
            "evidence basis must be one of {BASES:?}"
        );
    }
    ensure!(
        m["evidence"].is_null() || m["evidence"].is_array(),
        "evidence must be an array"
    );
    ensure!(m["links"].is_null() || m["links"].is_array(), "links must be an array");
    for l in m["links"].as_array().into_iter().flatten() {
        ensure!(
            RELATIONS.contains(&string(l, "relation").as_str()),
            "link relation must be one of {RELATIONS:?}"
        );
        ensure!(
            valid_id(&string(l, "target")) && string(l, "target") != string(r, "id"),
            "link target must be another record's id"
        );
        ensure!(
            !matches!(
                l["relation"].as_str(),
                Some("supports" | "contradicts" | "derived_from")
            ) || l["evidence"].as_array().is_some_and(|a| !a.is_empty()),
            "supports, contradicts and derived_from links need evidence"
        );
    }
    Ok(())
}

pub fn render(r: &Value) -> Result<String> {
    let mut meta = r.clone();
    let body = meta.as_object_mut().and_then(|m| m.remove("body")).unwrap_or(json!(""));
    Ok(format!(
        "---\n{}\n---\n\n{}\n",
        serde_json::to_string_pretty(&meta)?,
        body.as_str().unwrap_or_default().trim()
    ))
}

pub fn parse(text: &str) -> Result<Value> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow::anyhow!("missing frontmatter"))?;
    let (meta, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("unclosed frontmatter"))?;
    let mut r: Value = serde_json::from_str(meta)?;
    ensure!(r.is_object(), "frontmatter must be a JSON object");
    r["body"] = json!(body.trim());
    validate(&r)?;
    Ok(r)
}

/// The file behind a record id, from the index.
async fn located(db: &SqlitePool, id: &str) -> Result<PathBuf> {
    let p: Option<String> = sqlx::query_scalar("SELECT path FROM brain_records WHERE id=$1")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(PathBuf::from(p.ok_or_else(|| anyhow::anyhow!("no record `{id}`"))?))
}

/// Which records a reader sees, by the vault their file is in.
#[derive(Clone, Debug)]
pub struct Scope {
    /// Its own vault (`global/agents/<name>/`), or None for zenbot and the owner: every vault.
    own: Option<String>,
    /// Whether it also sees the global vault's records.
    global: bool,
    /// `global/agents/`: other agents' vaults, never seen by another agent.
    agents: String,
}

impl Scope {
    /// Everything: zenbot and the owner.
    pub fn all() -> Self {
        Scope { own: None, global: true, agents: String::new() }
    }

    /// What `agent` sees: everything for zenbot; its own vault, plus the global one when its
    /// `global` setting is read or write, for another.
    pub fn of(agent: &crate::agents::Agent) -> Self {
        Self::within(&crate::layout::global_dir(), agent)
    }

    fn within(global: &Path, agent: &crate::agents::Agent) -> Self {
        if agent.name == crate::agents::DEFAULT {
            return Self::all();
        }
        let slash = |p: PathBuf| format!("{}/", p.display());
        Scope {
            own: Some(slash(global.join("agents").join(&agent.name))),
            global: agent.global_level() != crate::agents::Global::None,
            agents: slash(global.join("agents")),
        }
    }

    pub fn sees(&self, path: &str) -> bool {
        match &self.own {
            None => true,
            Some(own) => path.starts_with(own.as_str()) || (self.global && !path.starts_with(&self.agents)),
        }
    }
}

/// A record's current file, parsed, and its text, if `scope` sees it.
pub async fn read(db: &SqlitePool, id: &str, scope: &Scope) -> Result<(Value, String)> {
    let p = located(db, id).await?;
    ensure!(scope.sees(&p.display().to_string()), "no record `{id}`");
    let text = crate::sandbox::read_vault_file(&p)?;
    let mut r = parse(&text)?;
    r["path"] = json!(p.display().to_string());
    Ok((r, text))
}

pub(crate) async fn private_write(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("missing parent"))?;
    tokio::fs::create_dir_all(parent).await?;
    for p in [parent, path] {
        if let Ok(m) = tokio::fs::symlink_metadata(p).await {
            ensure!(
                !m.file_type().is_symlink(),
                "refusing to write through a symlink: {}",
                p.display()
            );
        }
    }
    // Written through the folder actually opened, once it's sure to be in its vault: an agent
    // can't redirect the kernel's write with a link in place of a folder.
    let dir = crate::sandbox::open_vault_dir(parent)?;
    let parent = &PathBuf::from(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&dir)));
    let name = path.file_name().ok_or_else(|| anyhow::anyhow!("missing file name"))?;
    let tmp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .await?;
    f.write_all(text.as_bytes()).await?;
    f.sync_all().await?;
    tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
    tokio::fs::rename(&tmp, parent.join(name)).await?;
    drop(dir);
    Ok(())
}

/// Create or update a record file. An update names the revision it read (`expected_revision`). A new
/// long-term memory needs `new_memory` (the sleep, or `remember` past its high bar). The previous
/// version is backed up in `backups/brain/`.
pub async fn put(
    db: &SqlitePool,
    vault: &Path,
    scope: &Scope,
    record: &Value,
    expected: Option<i64>,
    new_memory: bool,
) -> Result<Value> {
    let mut r: Value = serde_json::from_str(&crate::secrets::mask(&record.to_string()))?;
    ensure!(r.is_object(), "record must be an object");
    if r["metadata"].is_null() {
        r["metadata"] = json!({});
    }
    r.as_object_mut().unwrap().remove("path");
    validate(&r)?;
    let (id, store, kind) = (string(&r, "id"), string(&r, "store"), string(&r, "kind"));
    let p = path(vault, &store, &kind, &id);
    let _writing = WRITING.lock().await;
    let indexed: Option<String> = sqlx::query_scalar("SELECT path FROM brain_records WHERE id=$1")
        .bind(&id)
        .fetch_optional(db)
        .await?;
    if let Some(other) = indexed.filter(|o| Path::new(o) != p && Path::new(o).exists()) {
        // Ids are one namespace across vaults: a record the writer can't see is only "taken".
        ensure!(scope.sees(&other), "`{id}` is taken; choose another id");
        anyhow::bail!("`{id}` is already {other}: an id is one record (a kind or store can't change)");
    }
    let prior = crate::sandbox::read_vault_file(&p).ok();
    let old = prior.as_deref().map(parse).transpose()?;
    match &old {
        Some(o) => ensure!(
            expected == o["revision"].as_i64(),
            "`{id}` is at revision {}: read it again and pass expected_revision",
            o["revision"]
        ),
        None => ensure!(expected.is_none(), "no record `{id}` to update"),
    }
    if store == "memory" && old.is_none() && !new_memory {
        anyhow::bail!("new long-term memories come from `remember` (long: true) or the sleep, not put");
    }
    let mut names = vec![string(&r, "title").to_lowercase()];
    names.extend(
        r["metadata"]["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str())
            .map(|a| a.trim().to_lowercase()),
    );
    // A name is one person, organization, project or concept within its store and its vault;
    // memories may repeat.
    let mine = crate::agents::owner_of(&p);
    let taken: Vec<String> = sqlx::query_as::<_, (String, String)>(
        "SELECT DISTINCT a.record_id, r.path FROM brain_aliases a JOIN brain_records r ON r.id=a.record_id
         WHERE a.alias IN (SELECT value FROM json_each($1)) AND a.record_id<>$2 AND r.store=$3 AND r.store<>'memory'",
    )
    .bind(serde_json::json!(names))
    .bind(&id)
    .bind(&store)
    .fetch_all(db)
    .await?
    .into_iter()
    .filter(|(_, path)| crate::agents::owner_of(Path::new(path)) == mine)
    .map(|(id, _)| id)
    .collect();
    ensure!(
        taken.is_empty(),
        "a name or alias already belongs to {taken:?}: update that record, or resolve who is who with the owner"
    );
    for l in r["metadata"]["links"].as_array().into_iter().flatten() {
        let target = string(l, "target");
        let at: Option<String> = sqlx::query_scalar("SELECT path FROM brain_records WHERE id=$1")
            .bind(&target)
            .fetch_optional(db)
            .await?;
        ensure!(at.is_some_and(|at| scope.sees(&at)), "link target `{target}` doesn't exist: create it first");
    }
    let now = chrono::Utc::now().to_rfc3339();
    r["schema_version"] = json!(1);
    r["revision"] = json!(old.as_ref().and_then(|o| o["revision"].as_i64()).unwrap_or(0) + 1);
    r["created"] = old
        .as_ref()
        .map(|o| o["created"].clone())
        .filter(|c| !c.is_null())
        .unwrap_or(json!(now));
    r["updated"] = json!(now);
    if r["status"].is_null() {
        r["status"] = json!("active");
    }
    let text = render(&r)?;
    if let Some(prior) = &prior {
        let backup = crate::zen_home()
            .join("backups/brain")
            .join(format!("{id}-r{}.md", r["revision"].as_i64().unwrap_or(1) - 1));
        private_write(&backup, prior).await?;
    }
    private_write(&p, &text).await?;
    let mut tx = crate::storage::begin(db).await?;
    index_file(&mut tx, &p, &store, &text).await?;
    tx.commit().await?;
    Ok(json!({"id": id, "revision": r["revision"], "path": p.display().to_string()}))
}

/// Index one record file: its row, names, links and search document.
pub(super) async fn index_file(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, p: &Path, store: &str, text: &str) -> Result<()> {
    let r = parse(text)?;
    ensure!(
        r["store"] == store,
        "{} says store {} but lives in the {store} folder",
        p.display(),
        r["store"]
    );
    let id = string(&r, "id");
    let modified = chrono::DateTime::<chrono::Utc>::from(std::fs::metadata(p)?.modified()?);
    let m = &r["metadata"];
    let path = p.display().to_string();
    let other: Option<String> = sqlx::query_scalar("SELECT path FROM brain_records WHERE id=$1 AND path<>$2")
        .bind(&id)
        .bind(&path)
        .fetch_optional(&mut **tx)
        .await?;
    if let Some(other) = other.filter(|o| Path::new(o).exists()) {
        anyhow::bail!("same id `{id}` as {other}");
    }
    sqlx::query("DELETE FROM brain_records WHERE path=$1 AND id<>$2")
        .bind(&path)
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO brain_records(id,store,kind,title,path,digest,modified,metadata) VALUES($1,$2,$3,$4,$5,$6,$7,$8)
        ON CONFLICT(id) DO UPDATE SET store=$2,kind=$3,title=$4,path=$5,digest=$6,modified=$7,metadata=$8",
    )
    .bind(&id)
    .bind(store)
    .bind(string(&r, "kind"))
    .bind(string(&r, "title"))
    .bind(&path)
    .bind(hash(text))
    .bind(modified)
    .bind(m)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM brain_aliases WHERE record_id=$1")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    let mut names = vec![string(&r, "title").to_lowercase()];
    names.extend(
        m["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str())
            .map(|a| a.trim().to_lowercase()),
    );
    sqlx::query("INSERT INTO brain_aliases SELECT DISTINCT value,$2 FROM json_each($1) WHERE true ON CONFLICT DO NOTHING")
        .bind(serde_json::json!(names))
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM brain_links WHERE source=$1")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    for l in m["links"].as_array().into_iter().flatten() {
        sqlx::query("INSERT INTO brain_links VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(&id)
            .bind(string(l, "relation"))
            .bind(string(l, "target"))
            .execute(&mut **tx)
            .await?;
    }
    let words = |k: &str| {
        m[k].as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let ident = format!("{id} {} {} {}", words("aliases"), words("tags"), words("subjects"));
    let title = format!("{} {}: {}", store, string(&r, "kind"), string(&r, "title"));
    sqlx::query(
        "INSERT INTO search_docs(kind,ref,ident,title,body,at,agent) VALUES($1,$2,$3,$4,$5,$6,$7)
        ON CONFLICT(kind,ref) DO UPDATE SET ident=$3,title=$4,body=$5,at=$6,agent=$7,updated_at=strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            embedding=CASE WHEN search_docs.body=EXCLUDED.body THEN search_docs.embedding END,
            embed_attempts=CASE WHEN search_docs.body=EXCLUDED.body THEN search_docs.embed_attempts ELSE 0 END",
    )
    .bind(format!("record:{store}"))
    .bind(&id)
    .bind(ident)
    .bind(title)
    .bind(string(&r, "body"))
    .bind(modified)
    .bind(crate::agents::owner_of(p))
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM search_docs WHERE ref=$1 AND kind LIKE 'record:%' AND kind<>$2")
        .bind(&id)
        .bind(format!("record:{store}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Bring the index up to the files: changed files are read again, removed ones are dropped. A
/// file that doesn't parse is left out (and listed by `lint`).
pub async fn index(db: &SqlitePool) -> Result<usize> {
    let known: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>> =
        sqlx::query_as("SELECT path, modified FROM brain_records")
            .fetch_all(db)
            .await?
            .into_iter()
            .collect();
    let mut seen = vec![];
    let mut changed = 0;
    for (dir, store) in folders() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "md") {
                continue;
            }
            let path = p.display().to_string();
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .map(chrono::DateTime::<chrono::Utc>::from)
                .ok();
            if known
                .get(&path)
                .is_some_and(|at| Some(at.timestamp_micros()) == modified.map(|m| m.timestamp_micros()))
            {
                seen.push(path);
                continue;
            }
            let Ok(text) = crate::sandbox::read_vault_file(&p) else {
                continue;
            };
            let _writing = WRITING.lock().await;
            let mut tx = crate::storage::begin(db).await?;
            match index_file(&mut tx, &p, store, &text).await {
                Ok(()) => {
                    tx.commit().await?;
                    seen.push(path);
                    changed += 1;
                }
                Err(e) => tracing::warn!("record {}: {e:#}", p.display()),
            }
        }
    }
    let gone: Vec<String> = sqlx::query_scalar("DELETE FROM brain_records WHERE NOT (path IN (SELECT value FROM json_each($1))) RETURNING id")
        .bind(serde_json::json!(seen))
        .fetch_all(db)
        .await?;
    if !gone.is_empty() {
        sqlx::query("DELETE FROM brain_aliases WHERE record_id IN (SELECT value FROM json_each($1))")
            .bind(serde_json::json!(gone))
            .execute(db)
            .await?;
        sqlx::query("DELETE FROM brain_links WHERE source IN (SELECT value FROM json_each($1))")
            .bind(serde_json::json!(gone))
            .execute(db)
            .await?;
        sqlx::query("DELETE FROM search_docs WHERE kind LIKE 'record:%' AND ref IN (SELECT value FROM json_each($1))")
            .bind(serde_json::json!(gone))
            .execute(db)
            .await?;
    }
    Ok(changed + gone.len())
}

/// Problems in the record files: ones that don't parse, duplicate ids, links to nothing.
pub async fn lint(db: &SqlitePool) -> Result<Vec<Value>> {
    let mut issues = vec![];
    let mut ids: std::collections::HashMap<String, String> = Default::default();
    for (dir, store) in folders() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "md") {
                continue;
            }
            match crate::sandbox::read_vault_file(&p)
                .map_err(anyhow::Error::from)
                .and_then(|t| parse(&t))
            {
                Ok(r) if r["store"] != store => {
                    issues.push(json!({"path": p, "issue": format!("store {} in the {store} folder", r["store"])}))
                }
                Ok(r) => {
                    if let Some(other) = ids.insert(string(&r, "id"), p.display().to_string()) {
                        issues.push(json!({"path": p, "issue": format!("same id as {other}")}));
                    }
                }
                Err(e) => issues.push(json!({"path": p, "issue": format!("{e:#}")})),
            }
        }
    }
    let dangling: Vec<(String, String)> = sqlx::query_as(
        "SELECT source, target FROM brain_links l WHERE NOT EXISTS (SELECT 1 FROM brain_records r WHERE r.id=l.target)",
    )
    .fetch_all(db)
    .await?;
    issues.extend(
        dangling
            .into_iter()
            .map(|(s, t)| json!({"id": s, "issue": format!("links to missing record `{t}`")})),
    );
    Ok(issues)
}

/// The links touching record `id` that `scope` may see: the other end must be a record it sees (or
/// a target named in the record's own file).
pub async fn visible_links(db: &SqlitePool, id: &str, scope: &Scope) -> Result<Vec<(String, String, String)>> {
    let all = links(db, &[id.to_string()]).await?;
    let others: Vec<&str> = all.iter().map(|(from, _, to)| if from == id { to.as_str() } else { from.as_str() }).collect();
    let paths: std::collections::HashMap<String, String> = sqlx::query_as("SELECT id, path FROM brain_records WHERE id IN (SELECT value FROM json_each($1))")
        .bind(json!(others))
        .fetch_all(db)
        .await?
        .into_iter()
        .collect();
    Ok(all
        .into_iter()
        .filter(|(from, _, to)| {
            let other = if from == id { to } else { from };
            match paths.get(other) {
                Some(path) => scope.sees(path),
                None => from == id,
            }
        })
        .collect())
}

/// Typed links touching these records, both ways: (from, relation, to).
pub async fn links(db: &SqlitePool, ids: &[String]) -> Result<Vec<(String, String, String)>> {
    Ok(sqlx::query_as("SELECT source, relation, target FROM brain_links WHERE source IN (SELECT value FROM json_each($1)) OR target IN (SELECT value FROM json_each($1)) ORDER BY source, relation, target LIMIT 200")
        .bind(serde_json::json!(ids))
        .fetch_all(db)
        .await?)
}

/// Records `scope` sees, listed by store, kind, tag or words in the title, as (id, store, kind,
/// title, path).
pub async fn list(db: &SqlitePool, a: &Value, scope: &Scope) -> Result<Vec<(String, String, String, String, String)>> {
    // Mode: all (zenbot, the owner), own (its vault), or own_global (also outside `global/agents/`).
    let mode = match &scope.own {
        None => "all",
        Some(_) if scope.global => "own_global",
        Some(_) => "own",
    };
    Ok(sqlx::query_as(
        "SELECT id, store, kind, title, path FROM brain_records
         WHERE ($1='' OR store=$1) AND ($2='' OR kind=$2) AND ($3='' OR EXISTS(SELECT 1 FROM json_each(metadata,'$.tags') WHERE value=$3)) AND ($4='' OR title LIKE '%'||$4||'%')
           AND ($6='all' OR substr(path,1,length($7))=$7 OR ($6='own_global' AND substr(path,1,length($8))<>$8))
         ORDER BY store, kind, id LIMIT 50 OFFSET $5",
    )
    .bind(string(a, "store"))
    .bind(string(a, "kind"))
    .bind(string(a, "tag"))
    .bind(string(a, "query"))
    .bind(a["offset"].as_i64().unwrap_or(0).max(0))
    .bind(mode)
    .bind(scope.own.as_deref().unwrap_or(""))
    .bind(&scope.agents)
    .fetch_all(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_and_ids_stay_inside_their_folder() {
        let r = json!({"id":"conditional-probability","store":"knowledge","kind":"concept","title":"Conditional probability","body":"A definition.",
            "metadata":{"tags":["math"],"evidence":[{"source":"book","locator":"chapter 2","basis":"proof"}]}});
        validate(&r).unwrap();
        assert_eq!(parse(&render(&r).unwrap()).unwrap()["body"], r["body"]);
        let mut bad = r.clone();
        bad["id"] = json!("../../token");
        assert!(validate(&bad).is_err());
        assert!(path(&root(), "knowledge", "concept", "x").ends_with("wiki/concepts/x.md"));
        assert!(path(&root(), "memory", "decision", "x").ends_with("global/memory/x.md"));
        assert!(path(&root(), "entity", "person", "ana").ends_with("entities/people/ana.md"));
        assert!(path(Path::new("/g/agents/garry"), "memory", "decision", "x").ends_with("/g/agents/garry/memory/x.md"));
    }

    #[tokio::test]
    async fn an_agent_sees_its_own_records_and_the_global_ones_only_when_allowed() {
        let (_dir, db) = crate::test_util::database().await;
        let g = Path::new("/z/global");
        for (id, path) in [("zen", "/z/global/memory/zen.md"), ("mine", "/z/global/agents/garry/memory/mine.md"), ("theirs", "/z/global/agents/ana/memory/theirs.md")] {
            sqlx::query("INSERT INTO brain_records(id,store,kind,title,path,digest,modified,metadata) VALUES($1,'memory','decision',$1,$2,'',strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),'{}')")
                .bind(id).bind(path).execute(&db).await.unwrap();
        }
        let agent = |global: &str| crate::agents::tests_agent("garry", global);
        let ids = |scope: Scope| {
            let db = db.clone();
            async move { list(&db, &json!({}), &scope).await.unwrap().into_iter().map(|r| r.0).collect::<Vec<_>>() }
        };
        assert_eq!(ids(Scope::within(g, &agent("none"))).await, ["mine"]);
        assert_eq!(ids(Scope::within(g, &agent("read"))).await, ["mine", "zen"]);
        assert_eq!(ids(Scope::within(g, &agent("write"))).await, ["mine", "zen"]);
        assert_eq!(ids(Scope::all()).await, ["mine", "theirs", "zen"]);
        assert_eq!(ids(Scope::within(g, &crate::agents::tests_agent("zenbot", "none"))).await, ["mine", "theirs", "zen"]);
        let none = Scope::within(g, &agent("none"));
        assert!(none.sees("/z/global/agents/garry/memory/mine.md") && !none.sees("/z/global/memory/zen.md"));
        assert!(!Scope::within(g, &agent("read")).sees("/z/global/agents/ana/memory/theirs.md"));
        assert!(!none.sees("/z/global/agents/garry-2/memory/x.md"), "a longer name isn't its vault");
    }

    #[tokio::test]
    async fn another_vaults_records_stay_hidden_from_put_and_links() {
        let (dir, db) = crate::test_util::database().await;
        // A vault of its own in the test folder: put writes nothing outside it.
        let g = dir.join("global");
        let zen = g.join("entities/people/ana.md");
        let theirs = g.join("agents/ana/entities/people/bob.md");
        let mine = g.join("agents/garry/entities/people/carl.md");
        for (id, path) in [("ana", &zen), ("bob", &theirs), ("carl", &mine)] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
            sqlx::query("INSERT INTO brain_records(id,store,kind,title,path,digest,modified,metadata) VALUES($1,'entity','person',$1,$2,'',strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),'{}')")
                .bind(id).bind(path.display().to_string()).execute(&db).await.unwrap();
        }
        for (s, t) in [("carl", "ana"), ("carl", "bob"), ("bob", "carl"), ("ana", "carl"), ("carl", "gone")] {
            sqlx::query("INSERT INTO brain_links VALUES($1,'applies_to',$2)").bind(s).bind(t).execute(&db).await.unwrap();
        }
        let scope = Scope::within(&g, &crate::agents::tests_agent("garry", "none"));
        let others = |l: Vec<(String, String, String)>| l.into_iter().map(|(f, _, t)| if f == "carl" { t } else { f }).collect::<Vec<_>>();
        assert_eq!(others(visible_links(&db, "carl", &scope).await.unwrap()), ["gone"]);
        let read = Scope::within(&g, &crate::agents::tests_agent("garry", "read"));
        assert_eq!(others(visible_links(&db, "carl", &read).await.unwrap()), ["ana", "ana", "gone"]);
        // Another vault's id: taken, nothing more. A link to a record it can't see: missing.
        let vault = g.join("agents/garry");
        let bob = json!({"id":"bob","store":"entity","kind":"person","title":"Bobby","body":"x"});
        let e = put(&db, &vault, &scope, &bob, None, false).await.unwrap_err().to_string();
        assert!(e.contains("is taken; choose another") && !e.contains("agents/ana"), "{e}");
        let linked = json!({"id":"dan","store":"entity","kind":"person","title":"Dan","body":"x","metadata":{"links":[{"relation":"applies_to","target":"bob"}]}});
        let e = put(&db, &vault, &scope, &linked, None, false).await.unwrap_err().to_string();
        assert!(e.contains("link target `bob` doesn't exist"), "{e}");
        assert!(!vault.join("entities/people/dan.md").exists() && !vault.join("entities/people/bob.md").exists());
    }

    #[test]
    fn evidential_links_need_evidence_and_kinds_belong_to_their_store() {
        let r = json!({"id":"t","store":"knowledge","kind":"claim","title":"Claim","body":"Text","metadata":{"links":[{"relation":"supports","target":"other"}]}});
        assert!(validate(&r).is_err());
        let r = json!({"id":"t","store":"memory","kind":"concept","title":"T","body":"Text"});
        assert!(validate(&r).is_err());
        let r = json!({"id":"t","store":"memory","kind":"decision","title":"T","body":"Text","metadata":{"tags":["Not A Slug"]}});
        assert!(validate(&r).is_err());
    }
}
