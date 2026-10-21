//! Canonical entities and knowledge: validated metadata, typed links and recoverable publishing.
use super::*;
use std::path::PathBuf;
const KINDS: &[&str] = &[
    "concept",
    "claim",
    "argument",
    "question",
    "synthesis",
    "source",
    "event",
    "subject",
    "person",
    "organization",
    "project",
];
const RELATIONS: &[&str] = &[
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
const SCHEMA: &str = "# Second-brain schema v1\n\nOne canonical ID per record; aliases are case-insensitive. Search before creating.\nKnowledge lives under wiki/{concepts,claims,arguments,questions,syntheses,sources,events,subjects}/.\nEntities live under entities/{people,organizations,projects}/. Folders are primary homes; subjects\nand typed links are multiple classifications. Frontmatter is a JSON object (valid YAML), followed\nby the readable body. Preserve IDs when renaming. Evidence items cite source and locator; inferential\nlinks are tentative. Status: active, provisional, superseded, archived. Never treat mentions as proof.\nSources describe precisely what was inspected. Memory is curated experience, sessions are archives.\nGenerated memory projections are edited through brain actions, not silently overwritten.\n";
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn target_parts(value: &str) -> (&str, Option<&str>) {
    match value.split_once('#') {
        Some((id, anchor)) => (id, Some(anchor)),
        None => (value, None),
    }
}
fn has_anchor(body: &str, anchor: &str) -> bool {
    body.contains(&format!("{{#{anchor}}}")) || body.contains(&format!("id=\"{anchor}\""))
}
fn folder(kind: &str) -> &str {
    match kind {
        "person" => "people",
        "organization" => "organizations",
        "synthesis" => "syntheses",
        "subject" => "subjects",
        "claim" => "claims",
        "argument" => "arguments",
        "question" => "questions",
        "source" => "sources",
        "event" => "events",
        "project" => "projects",
        _ => "concepts",
    }
}
fn path(store: &str, kind: &str, id: &str) -> PathBuf {
    (if store == "entity" {
        root().join("entities")
    } else {
        crate::wiki::root()
    })
    .join(folder(kind))
    .join(format!("{id}.md"))
}
fn validate(r: &Value) -> Result<()> {
    ensure!(
        valid_id(&string(r, "id")),
        "id: lowercase letters, digits and hyphens, 1..96 characters"
    );
    ensure!(
        matches!(r["store"].as_str(), Some("knowledge" | "entity")),
        "store must be knowledge or entity"
    );
    ensure!(
        KINDS.contains(&string(r, "kind").as_str()),
        "unknown record kind"
    );
    ensure!(
        (r["store"] == "entity")
            == matches!(
                r["kind"].as_str(),
                Some("person" | "organization" | "project")
            ),
        "entity kind/store mismatch"
    );
    ensure!(
        !string(r, "title").trim().is_empty() && string(r, "title").len() <= 240,
        "title required, at most 240 bytes"
    );
    ensure!(
        !string(r, "body").trim().is_empty() && string(r, "body").len() <= 64 * 1024,
        "body required, at most 64 KiB"
    );
    ensure!(
        matches!(
            r["status"].as_str().unwrap_or("active"),
            "active" | "provisional" | "superseded" | "archived"
        ),
        "invalid status"
    );
    ensure!(
        r["schema_version"].is_null() || r["schema_version"] == 1,
        "unsupported schema version"
    );
    let id = string(r, "id");
    ensure!(
        !(id.starts_with('m') && id[1..].parse::<i64>().is_ok()),
        "numeric m IDs are reserved for memory"
    );
    let m = &r["metadata"];
    ensure!(m.to_string().len() <= 24000, "metadata exceeds 24k bytes");
    for subject in m["subjects"].as_array().into_iter().flatten() {
        ensure!(
            subject.as_str().is_some_and(valid_id),
            "subject IDs must be canonical slugs"
        );
    }
    ensure!(m.is_null() || m.is_object(), "metadata must be an object");
    for key in ["aliases", "subjects", "evidence", "links"] {
        ensure!(
            m[key].is_null() || m[key].is_array(),
            "{key} must be an array"
        );
    }
    for e in m["evidence"].as_array().into_iter().flatten() {
        ensure!(
            !string(e, "source").is_empty() && !string(e, "locator").is_empty(),
            "evidence needs source and locator"
        );
        ensure!(
            matches!(
                e["basis"].as_str().unwrap_or("external_claim"),
                "owner_statement"
                    | "observed"
                    | "external_claim"
                    | "inference"
                    | "proof"
                    | "empirical"
                    | "hypothesis"
                    | "interpretation"
                    | "tradition"
            ),
            "invalid evidence basis"
        );
    }
    for l in m["links"].as_array().into_iter().flatten() {
        ensure!(
            RELATIONS.contains(&string(l, "relation").as_str()) && {
                let target = string(l, "target");
                let (id, anchor) = target_parts(&target);
                valid_id(id) && anchor.is_none_or(valid_id)
            },
            "invalid relationship"
        );
        ensure!(
            !matches!(
                l["relation"].as_str(),
                Some("supports" | "contradicts" | "derived_from")
            ) || l["evidence"].as_array().is_some_and(|a| !a.is_empty()),
            "evidential links need evidence"
        );
    }
    Ok(())
}
fn render(r: &Value) -> Result<String> {
    let mut meta = r.clone();
    let body = meta
        .as_object_mut()
        .unwrap()
        .remove("body")
        .unwrap_or(json!(""));
    Ok(format!(
        "---\n{}\n---\n\n{}\n",
        serde_json::to_string_pretty(&meta)?,
        body.as_str().unwrap_or_default()
    ))
}
fn parse(text: &str) -> Result<Value> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow::anyhow!("missing schema frontmatter"))?;
    let (meta, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("unclosed frontmatter"))?;
    let mut r: Value = serde_json::from_str(meta)?;
    r["body"] = json!(body.trim());
    validate(&r)?;
    Ok(r)
}
async fn read(db: &PgPool, id: &str, session: Option<Uuid>) -> Result<Value> {
    let row = sqlx::query(
        "SELECT *,zen_brain_contaminated(id) AS contaminated FROM brain_records WHERE id=$1",
    )
    .bind(id)
    .fetch_one(db)
    .await?;
    ensure!(
        row.get::<bool, _>("published"),
        "record publication pending"
    );
    let p = PathBuf::from(row.get::<String, _>("path"));
    let text = tokio::fs::read_to_string(&p).await?;
    ensure!(
        Some(hash(&text)) == row.get::<Option<String>, _>("digest"),
        "file changed; run lint/import before using this revision"
    );
    let mut r = parse(&text)?;
    r["revision"] = json!(row.get::<i64, _>("revision"));
    r["risk"] = json!(row.get::<i16, _>("risk"));
    if let Some(s) = session {
        ensure!(!row.get::<bool, _>("contaminated"), "record writer denied");
        let risk = crate::sources::file_rating(db, &p, &text)
            .await?
            .max(row.get("risk"));
        let writers: Vec<Uuid> =
            sqlx::query_scalar("SELECT writer FROM brain_record_inputs WHERE record_id=$1")
                .bind(id)
                .fetch_all(db)
                .await?;
        crate::sources::depend(db, s, &writers).await?;
        admit(db, s, risk, row.get("session_id")).await?;
        crate::sources::depend_file(db, s, &text).await?;
    }
    Ok(r)
}
async fn bind_evidence(
    db: &PgPool,
    evidence: &mut [Value],
    risk: &mut i16,
    writers: &mut Vec<Uuid>,
) -> Result<()> {
    for e in evidence {
        ensure!(
            !string(e, "source").is_empty() && !string(e, "locator").is_empty(),
            "evidence needs source and locator"
        );
        let reference = string(e, "source");
        let source = reference.strip_prefix("record:").unwrap_or(&reference);
        if let Some(memory_id) = source
            .strip_prefix('m')
            .and_then(|id| id.parse::<i64>().ok())
        {
            let memory = memory_json(db, memory_id).await?;
            let current = memory["revision"].as_i64().unwrap_or(0);
            let revision = e["revision"].as_i64().unwrap_or(current);
            if revision != current {
                let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM brain_revisions WHERE record_id=$1 AND revision=$2)").bind(source).bind(revision).fetch_one(db).await?;
                ensure!(exists, "unknown memory evidence revision");
            }
            e["revision"] = json!(revision);
            let stopped: bool = sqlx::query_scalar("SELECT zen_memory_contaminated($1)")
                .bind(memory_id)
                .fetch_one(db)
                .await?;
            *risk = (*risk).max(if stopped {
                5
            } else {
                memory["risk"].as_i64().unwrap_or(4) as i16
            });
            let inputs: Vec<Uuid> =
                sqlx::query_scalar("SELECT writer FROM brain_memory_inputs WHERE memory_id=$1")
                    .bind(memory_id)
                    .fetch_all(db)
                    .await?;
            writers.extend(inputs);
            continue;
        }
        if let Some(row) = sqlx::query("SELECT revision,digest,CASE WHEN zen_brain_contaminated(id) THEN 5::smallint ELSE risk END AS risk,session_id FROM brain_records WHERE id=$1 AND published").bind(source).fetch_optional(db).await? {
            let current: i64 = row.get("revision");
            let revision = e["revision"].as_i64().unwrap_or(current);
            if revision == current {
                read(db, source, None).await?;
                e["digest"] = json!(row.get::<Option<String>,_>("digest"));
            } else {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM brain_revisions WHERE record_id=$1 AND revision=$2)").bind(source).bind(revision).fetch_one(db).await?;
                ensure!(exists, "unknown evidence revision");
                e.as_object_mut().ok_or_else(||anyhow::anyhow!("evidence must be an object"))?.remove("digest");
            }
            e["revision"] = json!(revision);
            *risk = (*risk).max(row.get("risk"));
            let inputs: Vec<Uuid> = sqlx::query_scalar("SELECT writer FROM brain_record_inputs WHERE record_id=$1").bind(source).fetch_all(db).await?;
            writers.extend(inputs);
            if let Some(writer) = row.get::<Option<Uuid>,_>("session_id") {writers.push(writer);}
        } else {
            e["verification"] = json!("reference_only; original bytes not registered");
        }
    }
    Ok(())
}
async fn put(
    app: &crate::AppState,
    session: Option<Uuid>,
    r: &Value,
    expected: Option<i64>,
    import: bool,
) -> Result<Value> {
    validate(r)?;
    let (id, store, kind) = (string(r, "id"), string(r, "store"), string(r, "kind"));
    let mut risk = guard(&app.db, session).await?;
    let body = crate::secrets::mask(&string(r, "body"));
    let mut r: Value = serde_json::from_str(&crate::secrets::mask(&r.to_string()))?;
    r["body"] = json!(body);
    if r["metadata"].is_null() {
        r["metadata"] = json!({});
    }
    // Outside claims or inferences never masquerade as verified personal intent.
    if session.is_some()
        && r["metadata"]["evidence"]
            .as_array()
            .is_none_or(|e| e.is_empty())
    {
        r["status"] = json!("provisional");
    }
    let mut input_writers = vec![];
    if let Some(s) = session {
        input_writers.push(s);
    }
    let mut evidence_groups = vec![];
    if let Some(evidence) = r["metadata"]["evidence"].as_array_mut() {
        evidence_groups.push(evidence);
    }
    for evidence in evidence_groups {
        bind_evidence(&app.db, evidence, &mut risk, &mut input_writers).await?;
    }
    if let Some(links) = r["metadata"]["links"].as_array_mut() {
        for link in links {
            if let Some(evidence) = link["evidence"].as_array_mut() {
                bind_evidence(&app.db, evidence, &mut risk, &mut input_writers).await?;
            }
        }
    }
    if let Some(s) = session {
        crate::sources::depend(&app.db, s, &input_writers).await?;
    }
    let p = path(&store, &kind, &id);
    let mut tx = app.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(704,4)")
        .execute(&mut *tx)
        .await?;
    let prior = sqlx::query("SELECT * FROM brain_records WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(old) = &prior {
        ensure!(
            expected == Some(old.get("revision")),
            "expected_revision required; record changed"
        );
        ensure!(
            old.get::<String, _>("path") == p.to_string_lossy(),
            "kind/store cannot change during update"
        );
        let old_text = tokio::fs::read_to_string(&p).await?;
        ensure!(
            import || Some(hash(&old_text)) == old.get::<Option<String>, _>("digest"),
            "manual file edit pending; import it first"
        );
        risk = risk.max(old.get("risk"));
        sqlx::query("INSERT INTO brain_revisions(record_id,revision,snapshot) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(&id).bind(old.get::<i64,_>("revision")).bind(parse(&old_text)?).execute(&mut *tx).await?;
    } else {
        ensure!(expected.is_none(), "record not found");
    }
    if r["status"].is_null() {
        r["status"] = json!("active");
    }
    let now = chrono::Utc::now().to_rfc3339();
    r["updated"] = json!(now);
    if let Some(old) = &prior {
        let old_text = tokio::fs::read_to_string(old.get::<String, _>("path")).await?;
        r["created"] = parse(&old_text)?
            .get("created")
            .cloned()
            .unwrap_or(json!(now));
    } else {
        r["created"] = json!(now);
    }
    let rev = prior
        .as_ref()
        .map(|o| o.get::<i64, _>("revision") + 1)
        .unwrap_or(1);
    let mut aliases = vec![string(&r, "title").to_lowercase()];
    for a in r["metadata"]["aliases"].as_array().into_iter().flatten() {
        ensure!(
            a.as_str().is_some_and(|s| !s.trim().is_empty()),
            "alias must be text"
        );
        aliases.push(a.as_str().unwrap().trim().to_lowercase());
    }
    for a in &aliases {
        let matches: Vec<String> = sqlx::query_scalar(
            "SELECT record_id FROM brain_aliases WHERE alias=$1 AND record_id!=$2",
        )
        .bind(a)
        .bind(&id)
        .fetch_all(&mut *tx)
        .await?;
        ensure!(
            matches.is_empty(),
            "ambiguous name/alias {a}; resolve existing identities: {matches:?}"
        );
    }
    for l in r["metadata"]["links"].as_array().into_iter().flatten() {
        let target_ref = string(l, "target");
        let (target, anchor) = target_parts(&target_ref);
        ensure!(
            target != id || anchor.is_some(),
            "self-link needs an explicit claim anchor"
        );
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM brain_records WHERE id=$1 AND published)",
        )
        .bind(target)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(exists, "unknown or pending link target {target}");
        if let Some(anchor) = anchor {
            let body: String = sqlx::query_scalar("SELECT body FROM brain_records WHERE id=$1")
                .bind(target)
                .fetch_one(&mut *tx)
                .await?;
            ensure!(has_anchor(&body, anchor), "target claim anchor missing");
        }
        if let Some(anchor) = l["source_anchor"].as_str() {
            ensure!(
                valid_id(anchor) && has_anchor(&string(&r, "body"), anchor),
                "source claim anchor missing"
            );
        }
    }
    r["revision"] = json!(rev);
    r["schema_version"] = json!(1);
    let text = render(&r)?;
    sqlx::query("INSERT INTO brain_records(id,store,kind,title,body,metadata,revision,session_id,risk,status,path) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(id) DO UPDATE SET title=$4,body=$5,metadata=$6,revision=$7,session_id=COALESCE($8,brain_records.session_id),risk=$9,status=$10,published=false,updated_at=now()")
        .bind(&id).bind(&store).bind(&kind).bind(string(&r,"title")).bind(string(&r,"body")).bind(&r["metadata"]).bind(rev).bind(session).bind(risk).bind(r["status"].as_str().unwrap_or("active")).bind(p.to_string_lossy().as_ref()).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM brain_aliases WHERE record_id=$1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    for alias in aliases {
        sqlx::query(
            "INSERT INTO brain_aliases(alias,record_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
        )
        .bind(alias)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    }
    for writer in input_writers {
        sqlx::query("INSERT INTO brain_record_inputs(record_id,writer) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(&id).bind(writer).execute(&mut *tx).await?;
    }
    let payload = json!({"id":id,"revision":rev,"text":text,"prior":if import {tokio::fs::read_to_string(&p).await.ok().map(|t|hash(&t))}else{prior.as_ref().and_then(|o|o.get::<Option<String>,_>("digest"))}});
    sqlx::query("INSERT INTO brain_work(kind,work_key,payload) VALUES('publish',$1,$2)")
        .bind(format!("publish:{id}:{rev}"))
        .bind(&payload)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    match publish(app, &payload).await {
        Ok(()) => {
            sqlx::query("UPDATE brain_work SET state='done',error=NULL WHERE work_key=$1")
                .bind(format!("publish:{id}:{rev}"))
                .execute(&app.db)
                .await?;
        }
        Err(e) => {
            return Ok(
                json!({"id":id,"revision":rev,"publication":"pending","error":e.to_string()}),
            );
        }
    }
    Ok(json!({"id":id,"revision":rev,"publication":"published"}))
}
async fn publish(app: &crate::AppState, p: &Value) -> Result<()> {
    let id = string(p, "id");
    let revision = p["revision"].as_i64().unwrap_or(0);
    let text = string(p, "text");
    let r = parse(&text)?;
    let mut tx = app.db.begin().await?;
    let row = sqlx::query("SELECT * FROM brain_records WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    ensure!(
        row.get::<i64, _>("revision") == revision,
        "publish superseded"
    );
    let path = PathBuf::from(row.get::<String, _>("path"));
    ensure!(
        path == self::path(&string(&r, "store"), &string(&r, "kind"), &id),
        "unexpected file path"
    );
    if let Ok(existing) = tokio::fs::read_to_string(&path).await {
        ensure!(
            hash(&existing) == hash(&text)
                || Some(hash(&existing)) == p["prior"].as_str().map(str::to_string),
            "manual edit conflict; publication held"
        );
    }
    if let Some(s) = row.get::<Option<Uuid>, _>("session_id") {
        let prior = tokio::fs::read_to_string(&path).await.ok();
        crate::sources::record_bytes(&app.db, s, &path, &text, prior.as_deref()).await?;
    }
    let writers: Vec<Uuid> =
        sqlx::query_scalar("SELECT writer FROM brain_record_inputs WHERE record_id=$1")
            .bind(&id)
            .fetch_all(&mut *tx)
            .await?;
    for writer in writers {
        sqlx::query("INSERT INTO item_ratings(path,sha256,session_id,risk) VALUES($1,$2,$3,$4) ON CONFLICT(path,sha256,session_id) DO UPDATE SET risk=GREATEST(item_ratings.risk,EXCLUDED.risk)").bind(path.to_string_lossy().as_ref()).bind(hash(&text)).bind(writer).bind(row.get::<i16,_>("risk")).execute(&mut *tx).await?;
    }
    private_write(&path, &text).await?;
    sqlx::query("DELETE FROM brain_aliases WHERE record_id=$1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    let mut aliases = vec![string(&r, "title").to_lowercase()];
    aliases.extend(
        r["metadata"]["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(|s| s.trim().to_lowercase())),
    );
    for a in aliases {
        sqlx::query(
            "INSERT INTO brain_aliases(alias,record_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
        )
        .bind(a)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM brain_links WHERE source=$1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    for l in r["metadata"]["links"].as_array().into_iter().flatten() {
        let reference = string(l, "target");
        let (target, anchor) = target_parts(&reference);
        sqlx::query("INSERT INTO brain_links(source,source_anchor,relation,target,target_anchor,evidence,tentative) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
            .bind(&id).bind(l["source_anchor"].as_str().unwrap_or("")).bind(string(l,"relation")).bind(target).bind(anchor.unwrap_or("")).bind(l.get("evidence").cloned().unwrap_or(json!([]))).bind(l["tentative"].as_bool().unwrap_or(false)).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE brain_records SET digest=$2,published=true WHERE id=$1")
        .bind(&id)
        .bind(hash(&text))
        .execute(&mut *tx)
        .await?;
    // Search index is a projection. Current bytes are always revalidated at read time.
    sqlx::query("INSERT INTO search_docs(kind,ref,ident,title,body,at) VALUES('brain',$1,$2,$3,$4,now()) ON CONFLICT(kind,ref) DO UPDATE SET ident=$2,title=$3,body=$4,embedding=NULL,embed_attempts=0,updated_at=now(),at=now()")
        .bind(&id).bind(format!("{} {} {}",id,r["title"],r["metadata"]["aliases"])).bind(string(&r,"title")).bind(&text).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
pub(crate) async fn private_write(path: &std::path::Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing parent"))?;
    let mut cursor = Some(parent);
    while let Some(p) = cursor {
        if let Ok(m) = tokio::fs::symlink_metadata(p).await {
            ensure!(!m.file_type().is_symlink(), "symlinked storage ancestor");
        }
        if p == root() || p == crate::wiki::root() {
            break;
        }
        cursor = p.parent();
    }
    tokio::fs::create_dir_all(parent).await?;
    // Refuse symlinked final directories/files; only canonical roots are selected by callers.
    ensure!(
        !tokio::fs::symlink_metadata(parent)
            .await?
            .file_type()
            .is_symlink(),
        "symlinked storage directory"
    );
    if let Ok(m) = tokio::fs::symlink_metadata(path).await {
        ensure!(!m.file_type().is_symlink(), "symlinked record");
    }
    let tmp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut f = options.open(&tmp).await?;
    use tokio::io::AsyncWriteExt;
    f.write_all(text.as_bytes()).await?;
    f.sync_all().await?;
    tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
    tokio::fs::rename(&tmp, path).await?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub async fn recover(app: &crate::AppState) -> Result<()> {
    let schema_path = root().join("brain-schema.md");
    if !schema_path.exists() {
        private_write(&schema_path, SCHEMA).await?;
    }
    // Close interrupted receipts only while no consolidation holds its snapshot lock.
    let mut sleep_lease = app.db.begin().await?;
    let idle: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(704,6)")
        .fetch_one(&mut *sleep_lease)
        .await?;
    if idle {
        sqlx::query("UPDATE sleep_runs SET ended_at=now(),error='interrupted; retained inputs will be re-snapshotted' WHERE ended_at IS NULL").execute(&mut *sleep_lease).await?;
    }
    sleep_lease.commit().await?;
    sqlx::query("UPDATE brain_work SET state='pending' WHERE kind='capture' AND state='failed' AND attempts<8 AND updated_at < now()-interval '1 minute'*attempts").execute(&app.db).await?;
    let rows=sqlx::query("UPDATE brain_work SET state='running',lease_until=now()+interval '2 minutes',attempts=attempts+1 WHERE id IN (SELECT id FROM brain_work WHERE (state='pending' OR (state='running' AND lease_until<now())) AND (kind!='capture' OR $1) ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 8) RETURNING id,kind,payload,attempts")
        .bind(crate::config::get().brain_mode=="on").fetch_all(&app.db).await?;
    for row in rows {
        let kind: String = row.get("kind");
        let payload: Value = row.get("payload");
        let res = match kind.as_str() {
            "publish" => publish(app, &payload).await,
            "capture" => super::sleep::process_capture(app, &payload).await,
            "memory_export" => super::sleep::export(&app.db).await,
            "synthesis" => match super::synthesis::process(app, &payload).await {
                Ok(result) => {
                    sqlx::query("UPDATE brain_work SET result=$2 WHERE id=$1 AND attempts=$3")
                        .bind(row.get::<i64, _>("id"))
                        .bind(result)
                        .bind(row.get::<i32, _>("attempts"))
                        .execute(&app.db)
                        .await?;
                    Ok(())
                }
                Err(e) => Err(e),
            },
            _ => Err(anyhow::anyhow!("unknown work kind")),
        };
        sqlx::query("UPDATE brain_work SET state=$2,error=$3,lease_until=NULL,updated_at=now() WHERE id=$1 AND attempts=$4")
            .bind(row.get::<i64,_>("id")).bind(if res.is_ok(){"done"}else{"failed"}).bind(res.err().map(|e|format!("{e:#}"))).bind(row.get::<i32,_>("attempts")).execute(&app.db).await?;
    }
    Ok(())
}
pub async fn search_rating(db: &PgPool, reader: Uuid, f: &crate::search::Found) -> Result<i16> {
    let row=sqlx::query("SELECT path,risk,session_id,published,digest,zen_brain_contaminated(id) AS contaminated FROM brain_records WHERE id=$1").bind(&f.reference).fetch_one(db).await?;
    ensure!(row.get::<bool, _>("published"), "publication pending");
    let path = PathBuf::from(row.get::<String, _>("path"));
    let text = tokio::fs::read_to_string(&path).await?;
    ensure!(
        text == f.body && Some(hash(&text)) == row.get::<Option<String>, _>("digest"),
        "stale indexed content"
    );
    let risk = if row.get::<bool, _>("contaminated") {
        5
    } else {
        crate::sources::file_rating(db, &path, &text)
            .await?
            .max(row.get("risk"))
    };
    if risk < 4 || f.exact {
        crate::sources::depend_file(db, reader, &text).await?;
        let writers: Vec<Uuid> =
            sqlx::query_scalar("SELECT writer FROM brain_record_inputs WHERE record_id=$1")
                .bind(&f.reference)
                .fetch_all(db)
                .await?;
        crate::sources::depend(db, reader, &writers).await?;
        crate::sources::depend(
            db,
            reader,
            &row.get::<Option<Uuid>, _>("session_id")
                .into_iter()
                .collect::<Vec<_>>(),
        )
        .await?;
    }
    Ok(risk)
}
pub(crate) async fn get_record(db: &PgPool, id: &str, s: Option<Uuid>) -> Result<Value> {
    read(db, id, s).await
}
pub async fn manage(app: &crate::AppState, session: Option<Uuid>, a: &Value) -> Result<Value> {
    guard(&app.db, session).await?;
    let action = string(a, "action");
    let id = string(a, "id");
    match action.as_str() {
        "put" => {
            put(
                app,
                session,
                &a["record"],
                a["expected_revision"].as_i64(),
                false,
            )
            .await
        }
        "import" => {
            ensure!(session.is_none(), "manual file import is owner-only");
            let row = sqlx::query("SELECT path,revision FROM brain_records WHERE id=$1")
                .bind(&id)
                .fetch_optional(&app.db)
                .await?;
            let (file, revision) = if let Some(row) = row {
                (
                    PathBuf::from(row.get::<String, _>("path")),
                    Some(row.get::<i64, _>("revision")),
                )
            } else {
                let kind = a["kind"].as_str().unwrap_or("concept");
                let store = a["store"].as_str().unwrap_or("knowledge");
                ensure!(
                    valid_id(&id) && KINDS.contains(&kind),
                    "canonical ID and kind required"
                );
                (path(store, kind, &id), None)
            };
            let text = tokio::fs::read_to_string(&file).await?;
            let record = parse(&text)?;
            ensure!(
                record["id"] == id
                    && path(&string(&record, "store"), &string(&record, "kind"), &id) == file,
                "file identity/path mismatch"
            );
            put(app, None, &record, revision, true).await
        }
        "get" => {
            if let Some(revision) = a["revision"].as_i64() {
                let current = if id.starts_with('m') && id[1..].parse::<i64>().is_ok() {
                    super::sleep::get(&app.db, &id, session).await?
                } else {
                    read(&app.db, &id, session).await?
                };
                if current["revision"].as_i64() == Some(revision) {
                    return Ok(current);
                }
                let mut prior: Value = sqlx::query_scalar(
                    "SELECT snapshot FROM brain_revisions WHERE record_id=$1 AND revision=$2",
                )
                .bind(&id)
                .bind(revision)
                .fetch_one(&app.db)
                .await?;
                if let Some(s) = session {
                    admit(
                        &app.db,
                        s,
                        prior["risk"]
                            .as_i64()
                            .unwrap_or(current["risk"].as_i64().unwrap_or(4))
                            as i16,
                        None,
                    )
                    .await?;
                }
                prior["historical"] = json!(true);
                prior["notice"] =
                    json!("Historical evidence; current content may supersede this version.");
                return Ok(prior);
            }
            if id.starts_with('m') && id[1..].parse::<i64>().is_ok() {
                super::sleep::get(&app.db, &id, session).await
            } else {
                read(&app.db, &id, session).await
            }
        }
        "list" | "search" => {
            let q = string(a, "query");
            let offset = a["offset"].as_i64().unwrap_or(0).max(0);
            if a["store"] == "memory" {
                let tier = string(a, "tier");
                ensure!(
                    matches!(
                        tier.as_str(),
                        "" | "short" | "long" | "archived" | "pending"
                    ),
                    "invalid memory tier"
                );
                let ids:Vec<i64>=sqlx::query_scalar("SELECT id FROM memories WHERE ($1='' AND tier!='archived' OR tier=$1) AND ($2='' OR record_type=$2) AND (text ILIKE '%'||$3||'%' OR details::text ILIKE '%'||$3||'%') ORDER BY id LIMIT 50 OFFSET $4").bind(tier).bind(string(a,"kind")).bind(&q).bind(offset).fetch_all(&app.db).await?;
                let mut out = vec![];
                for id in ids {
                    match super::sleep::get(&app.db, &format!("m{id}"), session).await {
                        Ok(r) => out.push(r),
                        Err(e) => out.push(json!({"id":format!("m{id}"),"withheld":e.to_string()})),
                    }
                }
                return Ok(
                    json!({"records":out,"offset":offset,"next_offset":if out.len()==50{Some(offset+50)}else{None},"complete":out.len()<50}),
                );
            }
            let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM brain_records WHERE published AND ($1='' OR store=$1) AND ($2='' OR kind=$2) AND ($3='' OR status=$3) AND (title ILIKE '%'||$4||'%' OR body ILIKE '%'||$4||'%' OR metadata::text ILIKE '%'||$4||'%') ORDER BY id LIMIT 50 OFFSET $5")
                .bind(string(a,"store")).bind(string(a,"kind")).bind(string(a,"status")).bind(q).bind(offset).fetch_all(&app.db).await?;
            let mut out = vec![];
            for id in ids {
                match read(&app.db, &id, session).await {
                    Ok(r) => out.push(r),
                    Err(e) => out.push(json!({"id":id,"withheld":e.to_string()})),
                };
            }
            Ok(
                json!({"records":out,"offset":offset,"next_offset":if out.len()==50{Some(offset+50)}else{None},"complete":out.len()<50}),
            )
        }
        "neighbors" => {
            read(&app.db, &id, session).await?;
            let rows=sqlx::query("SELECT source,source_anchor,relation,target,target_anchor,evidence,tentative FROM brain_links WHERE source=$1 OR target=$1 ORDER BY source,relation,target LIMIT 100").bind(&id).fetch_all(&app.db).await?;
            let mut edges = vec![];
            for r in rows {
                let other = if r.get::<String, _>("source") == id {
                    r.get::<String, _>("target")
                } else {
                    r.get::<String, _>("source")
                };
                if read(&app.db, &other, session).await.is_ok() {
                    edges.push(json!({"source":r.get::<String,_>("source"),"source_anchor":r.get::<String,_>("source_anchor"),"target_anchor":r.get::<String,_>("target_anchor"),"relation":r.get::<String,_>("relation"),"target":r.get::<String,_>("target"),"evidence":r.get::<Value,_>("evidence"),"tentative":r.get::<bool,_>("tentative")}));
                }
            }
            Ok(json!({"links":edges,"truncated":edges.len()==100}))
        }
        "brief" => {
            let entity = read(&app.db, &id, session).await?;
            let rows:Vec<i64>=sqlx::query_scalar("SELECT id FROM memories WHERE entities ? $1 AND tier IN ('short','long') ORDER BY COALESCE(event_at,created_at) DESC LIMIT 30").bind(&id).fetch_all(&app.db).await?;
            let mut memories = vec![];
            for m in rows {
                if let Ok(v) = super::sleep::get(&app.db, &format!("m{m}"), session).await {
                    memories.push(v);
                }
            }
            let related_ids:Vec<String>=sqlx::query_scalar("SELECT DISTINCT CASE WHEN source=$1 THEN target ELSE source END FROM brain_links WHERE source=$1 OR target=$1 LIMIT 30").bind(&id).fetch_all(&app.db).await?;
            let mut related = vec![];
            for linked in related_ids {
                if let Ok(record) = read(&app.db, &linked, session).await {
                    related.push(record);
                }
            }
            Ok(
                json!({"entity":entity,"memories":memories,"related":related,"notice":"Attributed records, not a complete relationship history. Check dates and unresolved commitments."}),
            )
        }
        "history" => {
            ensure!(
                session.is_none(),
                "revision history is owner-only; use source-aware get for agent reads"
            );
            let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('revision',revision,'snapshot',snapshot,'at',at) FROM brain_revisions WHERE record_id=$1 ORDER BY revision DESC LIMIT 100").bind(&id).fetch_all(&app.db).await?;
            Ok(json!({"revisions":rows}))
        }
        "skill_evidence" => {
            ensure!(session.is_none(), "skill evidence is owner-only");
            let rows: Vec<Value> = sqlx::query_scalar(
                "SELECT to_jsonb(u) FROM brain_skill_uses u ORDER BY id DESC LIMIT 100",
            )
            .fetch_all(&app.db)
            .await?;
            Ok(json!({"uses":rows}))
        }
        "lint" => {
            let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM brain_records ORDER BY id")
                .fetch_all(&app.db)
                .await?;
            let mut issues = vec![];
            for id in ids {
                if let Err(e) = read(&app.db, &id, session).await {
                    issues.push(json!({"id":id,"issue":e.to_string()}));
                }
            }
            Ok(json!({"issues":issues}))
        }
        "pin" | "promote" | "archive" | "correct" | "undo" | "merge" | "annotate" => {
            super::sleep::edit(&app.db, session, a).await
        }
        "sleep" => consolidate(app, "owner").await,
        "synthesize" => {
            let s = session
                .or_else(|| a["session"].as_str().and_then(|s| s.parse().ok()))
                .ok_or_else(|| anyhow::anyhow!("session required for attributed synthesis"))?;
            super::synthesis::propose(app, s, a).await
        }
        "dream" => super::sleep::dream(app, session).await,
        "retrieve" => {
            let s = session
                .or_else(|| a["session"].as_str().and_then(|s| s.parse().ok()))
                .ok_or_else(|| anyhow::anyhow!("session required"))?;
            let pack = super::context(app, s, &string(a, "query")).await?;
            Ok(json!({"context":pack,"receipt":super::explain(&app.db,s).await?}))
        }
        "context" => {
            let s = session
                .or_else(|| a["session"].as_str().and_then(|s| s.parse().ok()))
                .ok_or_else(|| anyhow::anyhow!("session required"))?;
            super::recall::explain(&app.db, s).await
        }
        "work" => {
            let rows: Vec<Value> = sqlx::query_scalar(
                "SELECT to_jsonb(w) FROM brain_work w WHERE $1::text IS NULL OR payload->>'session'=$1 ORDER BY id DESC LIMIT 100",
            ).bind(session.map(|s|s.to_string()))
            .fetch_all(&app.db)
            .await?;
            Ok(json!(rows))
        }
        "repair_projection" => {
            ensure!(session.is_none(), "projection repair is owner-only");
            super::sleep::repair_projection(&app.db, a).await
        }
        "repair" => {
            ensure!(session.is_none(), "repair is owner-only");
            let count=sqlx::query("UPDATE brain_work w SET state='pending',error=NULL WHERE kind='publish' AND payload->>'id'=$1 AND (payload->>'revision')::bigint=(SELECT revision FROM brain_records WHERE id=$1)").bind(&id).execute(&app.db).await?.rows_affected();
            ensure!(count > 0, "no publication receipt for this record");
            recover(app).await?;
            Ok(json!({"queued":count}))
        }
        "retry" => {
            ensure!(session.is_none(), "retry is owner-only");
            sqlx::query("UPDATE brain_work SET state='pending',error=NULL WHERE state='failed'")
                .execute(&app.db)
                .await?;
            recover(app).await?;
            Ok(json!({"ok":true}))
        }
        "subjects" => {
            ensure!(session.is_none(), "subject inventory is owner-only");
            let subjects:Vec<String>=sqlx::query_scalar("SELECT DISTINCT jsonb_array_elements_text(metadata->'subjects') FROM brain_records WHERE published AND metadata->'subjects' IS NOT NULL ORDER BY 1").fetch_all(&app.db).await?;
            Ok(
                json!({"subjects":subjects,"notice":"Derived subject inventory; use list/search to inspect source records."}),
            )
        }
        "schema" => Ok(json!({"schema":SCHEMA,"kinds":KINDS,"relations":RELATIONS})),
        _ => anyhow::bail!("unknown brain action"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_roundtrip_and_path_constraints() {
        let r = json!({"id":"conditional-probability","store":"knowledge","kind":"concept","title":"Conditional probability","body":"A definition.","metadata":{"evidence":[{"source":"book","locator":"chapter 2","basis":"proof"}]}});
        validate(&r).unwrap();
        assert_eq!(parse(&render(&r).unwrap()).unwrap()["body"], r["body"]);
        let mut bad = r;
        bad["id"] = json!("../../token");
        assert!(validate(&bad).is_err());
    }
    #[test]
    fn evidential_edges_require_evidence() {
        let r = json!({"id":"test","store":"knowledge","kind":"claim","title":"Claim","body":"Text","metadata":{"links":[{"relation":"supports","target":"other"}]}});
        assert!(validate(&r).is_err());
    }
}
