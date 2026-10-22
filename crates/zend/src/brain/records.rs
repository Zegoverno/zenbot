//! Record files: knowledge, entities (people, organizations, projects) and long-term memory.
//!
//! One Markdown file per record, its metadata a JSON frontmatter (a valid YAML subset). The files
//! are the source of truth: `brain_records`, `brain_aliases`, `brain_links` and the search index
//! are rebuilt from them (`index`), so the owner edits a file in any editor and the next index
//! pass picks it up. A file's rating is the rating of its bytes (item_ratings, D-067): bytes the
//! agent writes carry its session's rating and every cited writer's; bytes nobody registered (the
//! owner's edit) take the highest rating the path's writers had. Reading hands a record over at
//! that rating; a record whose writer was denied is withheld.
use super::*;
use std::path::{Path, PathBuf};

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
Stores and kinds: knowledge (concept, claim, argument, question, synthesis, source, event, subject) under wiki/<kind folder>/; \
entity (person, organization, project) under entities/<people|organizations|projects>/; memory (experience, decision, preference, \
commitment, question) under memory/. One canonical id per record (lowercase letters, digits, hyphens); aliases are other names. \
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

/// Where a record lives. Long-term memory is one flat folder; the others have a folder per kind.
pub fn path(store: &str, kind: &str, id: &str) -> PathBuf {
    match store {
        "memory" => root().join("memory"),
        "entity" => root().join("entities").join(folder(kind)),
        _ => crate::wiki::root().join(folder(kind)),
    }
    .join(format!("{id}.md"))
}

/// Every folder that holds records, with its store.
fn folders() -> Vec<(PathBuf, &'static str)> {
    let mut out = vec![(root().join("memory"), "memory")];
    out.extend(
        ENTITIES
            .iter()
            .map(|k| (root().join("entities").join(folder(k)), "entity")),
    );
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
async fn located(db: &PgPool, id: &str) -> Result<PathBuf> {
    let p: Option<String> = sqlx::query_scalar("SELECT path FROM brain_records WHERE id=$1")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(PathBuf::from(p.ok_or_else(|| anyhow::anyhow!("no record `{id}`"))?))
}

/// A record's current file, parsed, with its rating (5: a writer was denied).
pub async fn read(db: &PgPool, id: &str) -> Result<(Value, String, i16)> {
    let p = located(db, id).await?;
    let text = tokio::fs::read_to_string(&p).await?;
    let mut r = parse(&text)?;
    let risk = crate::sources::file_rating(db, &p, &text).await?;
    r["path"] = json!(p.display().to_string());
    Ok((r, text, risk))
}

/// Hand a record's bytes to `reader`: record its writers as the reader's inputs. A record whose
/// writer was denied is never handed over.
pub async fn hand(db: &PgPool, reader: Option<Uuid>, text: &str, risk: i16) -> Result<()> {
    ensure!(risk < 5, "record withheld: a session that wrote it was denied");
    if let Some(s) = reader {
        crate::sources::depend_file(db, s, text).await?;
    }
    Ok(())
}

/// Evidence that cites a memory (`m12`) or a record takes on its rating and writers.
async fn bind_evidence(db: &PgPool, evidence: &[Value], risk: &mut i16, writers: &mut Vec<(Uuid, i16)>) -> Result<()> {
    for e in evidence {
        let source = string(e, "source");
        let source = source.strip_prefix("record:").unwrap_or(&source);
        if let Some(id) = source.strip_prefix('m').and_then(|n| n.parse::<i64>().ok()) {
            let row: Option<(i16, bool)> =
                sqlx::query_as("SELECT risk, zen_memory_contaminated(id) FROM memories WHERE id=$1")
                    .bind(id)
                    .fetch_optional(db)
                    .await?;
            let Some((r, contaminated)) = row else { continue };
            ensure!(!contaminated, "cited memory m{id} was written by a denied session");
            *risk = (*risk).max(r);
            let inputs: Vec<Uuid> = sqlx::query_scalar("SELECT writer FROM brain_memory_inputs WHERE memory_id=$1")
                .bind(id)
                .fetch_all(db)
                .await?;
            writers.extend(inputs.into_iter().map(|w| (w, r)));
        } else if valid_id(source) {
            if let Ok((_, text, r)) = read(db, source).await {
                ensure!(r < 5, "cited record {source} was written by a denied session");
                *risk = (*risk).max(r);
                writers.extend(crate::sources::writers_of(db, &text).await?);
            }
        }
    }
    Ok(())
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
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// Create or update a record file: written by the agent's `session`, or by the kernel (none) for
/// the owner or the sleep. An update names the revision it read (`expected_revision`). A new
/// long-term memory needs `new_memory` (the sleep, or `remember` past its high bar). The previous
/// version is backed up in `backups/brain/`.
pub async fn put(
    db: &PgPool,
    session: Option<Uuid>,
    record: &Value,
    expected: Option<i64>,
    new_memory: bool,
) -> Result<Value> {
    let mut r: Value = serde_json::from_str(&crate::secrets::mask(&record.to_string()))?;
    ensure!(r.is_object(), "record must be an object");
    if r["metadata"].is_null() {
        r["metadata"] = json!({});
    }
    for k in ["path", "risk"] {
        r.as_object_mut().unwrap().remove(k);
    }
    validate(&r)?;
    let (id, store, kind) = (string(&r, "id"), string(&r, "store"), string(&r, "kind"));
    let p = path(&store, &kind, &id);
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(704,4)")
        .execute(&mut *tx)
        .await?;
    let indexed: Option<String> = sqlx::query_scalar("SELECT path FROM brain_records WHERE id=$1")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(other) = indexed.filter(|o| Path::new(o) != p && Path::new(o).exists()) {
        anyhow::bail!("`{id}` is already {other}: an id is one record (a kind or store can't change)");
    }
    let prior = tokio::fs::read_to_string(&p).await.ok();
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
    // A name is one person, organization, project or concept within its store; memories may repeat.
    let taken: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT a.record_id FROM brain_aliases a JOIN brain_records r ON r.id=a.record_id
         WHERE a.alias=ANY($1) AND a.record_id<>$2 AND r.store=$3 AND r.store<>'memory'",
    )
    .bind(&names)
    .bind(&id)
    .bind(&store)
    .fetch_all(&mut *tx)
    .await?;
    ensure!(
        taken.is_empty(),
        "a name or alias already belongs to {taken:?}: update that record, or resolve who is who with the owner"
    );
    for l in r["metadata"]["links"].as_array().into_iter().flatten() {
        let target = string(l, "target");
        let known: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM brain_records WHERE id=$1)")
            .bind(&target)
            .fetch_one(&mut *tx)
            .await?;
        ensure!(known, "link target `{target}` doesn't exist: create it first");
    }
    // The new bytes' rating: the writer's, every cited source's, and the old bytes' writers'.
    let mut floor: i16 = 1;
    let mut cited = vec![];
    let mut evidence: Vec<Value> = r["metadata"]["evidence"].as_array().cloned().unwrap_or_default();
    for l in r["metadata"]["links"].as_array().into_iter().flatten() {
        evidence.extend(l["evidence"].as_array().cloned().unwrap_or_default());
    }
    bind_evidence(db, &evidence, &mut floor, &mut cited).await?;
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
    match session {
        Some(s) => crate::sources::record_bytes(db, s, &p, &text, prior.as_deref()).await?,
        None => {
            if let Some(prior) = &prior {
                floor = floor.max(crate::sources::file_rating(db, &p, prior).await?);
                cited.extend(crate::sources::writers_of(db, prior).await?);
            }
        }
    }
    for (w, r) in &cited {
        crate::sources::register_bytes(db, &p, &text, Some(*w), *r).await?;
    }
    crate::sources::register_bytes(db, &p, &text, None, floor).await?;
    private_write(&p, &text).await?;
    index_file(&mut tx, &p, &store, &text).await?;
    tx.commit().await?;
    Ok(json!({"id": id, "revision": r["revision"], "path": p.display().to_string()}))
}

/// Index one record file: its row, names, links and search document.
pub(super) async fn index_file(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, p: &Path, store: &str, text: &str) -> Result<()> {
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
    sqlx::query("INSERT INTO brain_aliases SELECT DISTINCT unnest($1::text[]),$2 ON CONFLICT DO NOTHING")
        .bind(&names)
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
        "INSERT INTO search_docs(kind,ref,ident,title,body,at) VALUES($1,$2,$3,$4,$5,$6)
        ON CONFLICT(kind,ref) DO UPDATE SET ident=$3,title=$4,body=$5,at=$6,updated_at=now(),
            embedding=CASE WHEN search_docs.body=EXCLUDED.body THEN search_docs.embedding END,
            embed_attempts=CASE WHEN search_docs.body=EXCLUDED.body THEN search_docs.embed_attempts ELSE 0 END",
    )
    .bind(format!("record:{store}"))
    .bind(&id)
    .bind(ident)
    .bind(title)
    .bind(string(&r, "body"))
    .bind(modified)
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
pub async fn index(db: &PgPool) -> Result<usize> {
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
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let mut tx = db.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(704,4)")
                .execute(&mut *tx)
                .await?;
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
    let gone: Vec<String> = sqlx::query_scalar("DELETE FROM brain_records WHERE NOT (path = ANY($1)) RETURNING id")
        .bind(&seen)
        .fetch_all(db)
        .await?;
    if !gone.is_empty() {
        sqlx::query("DELETE FROM brain_aliases WHERE record_id=ANY($1)")
            .bind(&gone)
            .execute(db)
            .await?;
        sqlx::query("DELETE FROM brain_links WHERE source=ANY($1)")
            .bind(&gone)
            .execute(db)
            .await?;
        sqlx::query("DELETE FROM search_docs WHERE kind LIKE 'record:%' AND ref=ANY($1)")
            .bind(&gone)
            .execute(db)
            .await?;
    }
    Ok(changed + gone.len())
}

/// Problems in the record files: ones that don't parse, duplicate ids, links to nothing.
pub async fn lint(db: &PgPool) -> Result<Vec<Value>> {
    let mut issues = vec![];
    let mut ids: std::collections::HashMap<String, String> = Default::default();
    for (dir, store) in folders() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "md") {
                continue;
            }
            match std::fs::read_to_string(&p)
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

/// Typed links touching these records, both ways: (from, relation, to).
pub async fn links(db: &PgPool, ids: &[String]) -> Result<Vec<(String, String, String)>> {
    Ok(sqlx::query_as("SELECT source, relation, target FROM brain_links WHERE source=ANY($1) OR target=ANY($1) ORDER BY source, relation, target LIMIT 200")
        .bind(ids)
        .fetch_all(db)
        .await?)
}

/// Records listed by store, kind, tag or words in the title, as (id, store, kind, title, path).
pub async fn list(db: &PgPool, a: &Value) -> Result<Vec<(String, String, String, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, store, kind, title, path FROM brain_records
         WHERE ($1='' OR store=$1) AND ($2='' OR kind=$2) AND ($3='' OR metadata->'tags' ? $3) AND ($4='' OR title ILIKE '%'||$4||'%')
         ORDER BY store, kind, id LIMIT 50 OFFSET $5",
    )
    .bind(string(a, "store"))
    .bind(string(a, "kind"))
    .bind(string(a, "tag"))
    .bind(string(a, "query"))
    .bind(a["offset"].as_i64().unwrap_or(0).max(0))
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
        assert!(path("knowledge", "concept", "x").ends_with("wiki/concepts/x.md"));
        assert!(path("memory", "decision", "x").ends_with("global/memory/x.md"));
        assert!(path("entity", "person", "ana").ends_with("entities/people/ana.md"));
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
