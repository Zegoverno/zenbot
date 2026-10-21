//! Selected experience: bounded short tier, long-term records, snapshots and atomic dispositions.
use super::*;
const MEMORY_LIMIT: usize = 600;
fn cap() -> usize {
    crate::memory::cap() * 2
}
fn line(v: &Value) -> String {
    format!(
        "- [m{} r{}] {} ({}, {})\n",
        v["id"],
        v["revision"],
        string(v, "text").replace('\n', " "),
        string(v, "record_type"),
        string(v, "source")
    )
}
async fn enqueue_export(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    sqlx::query("INSERT INTO brain_work(kind,work_key,payload) VALUES('memory_export',$1,'{}')")
        .bind(format!("memory-export:{}", Uuid::new_v4()))
        .execute(&mut **tx)
        .await?;
    Ok(())
}
struct Capture<'a> {
    text: &'a str,
    source: &'a str,
    kind: &'a str,
    evidence: &'a Value,
    entities: &'a Value,
    pinned: bool,
    key: &'a str,
}
async fn add(db: &PgPool, session: Option<Uuid>, capture: Capture<'_>) -> Result<Value> {
    let Capture {
        text,
        source,
        kind,
        evidence,
        entities,
        pinned,
        key,
    } = capture;
    ensure!(
        !text.trim().is_empty() && text.chars().count() <= MEMORY_LIMIT,
        "memory needs 1..600 characters"
    );
    ensure!(
        matches!(
            kind,
            "experience"
                | "decision"
                | "preference"
                | "commitment"
                | "project"
                | "meeting"
                | "question"
        ),
        "invalid memory type"
    );
    ensure!(
        entities.is_array() && evidence.is_array(),
        "entities and evidence must be arrays"
    );
    let risk = guard(db, session).await?;
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(704,5)")
        .execute(&mut *tx)
        .await?;
    for e in entities.as_array().unwrap() {
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM brain_records WHERE id=$1 AND store='entity' AND published)").bind(e.as_str().unwrap_or("")).fetch_one(&mut *tx).await?;
        ensure!(exists, "unknown entity; resolve identity first");
    }
    let used:i64=sqlx::query_scalar("SELECT COALESCE(sum(length(text)+80),0)::bigint FROM memories WHERE tier='short' AND risk<4 AND NOT zen_memory_contaminated(id)").fetch_one(&mut *tx).await?;
    let tier = if pinned {
        "long"
    } else if used as usize + text.chars().count() + 80 > cap() {
        "pending"
    } else {
        "short"
    };
    let id:i64=sqlx::query_scalar("INSERT INTO memories(text,source,session_id,tier,record_type,evidence,entities,pinned,capture_key,tainted) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(capture_key) WHERE capture_key IS NOT NULL DO UPDATE SET capture_key=EXCLUDED.capture_key RETURNING id")
        .bind(crate::secrets::mask(text)).bind(source).bind(session).bind(tier).bind(kind).bind(evidence).bind(entities).bind(pinned).bind(key).bind(risk>=4).fetch_one(&mut *tx).await?;
    enqueue_export(&mut tx).await?;
    tx.commit().await?;
    let state: String = sqlx::query_scalar("SELECT tier FROM memories WHERE id=$1")
        .bind(id)
        .fetch_one(db)
        .await?;
    Ok(
        json!({"id":format!("m{id}"),"state":state,"notice":if tier=="pending"{"Saved durably; waiting for short-term space. Retrieve by ID immediately."}else{"Saved; available to subsequent turn context."}}),
    )
}
pub async fn remember(app: &crate::AppState, session: Uuid, a: &Value) -> Result<String> {
    let action = a["action"].as_str().unwrap_or("add");
    if action != "add" {
        let mut a = a.clone();
        a["action"] = json!(match action {
            "replace" => "correct",
            "remove" => "archive",
            other => other,
        });
        return Ok(edit(&app.db, Some(session), &a).await?.to_string());
    }
    let text = string(a, "text");
    let source = string(a, "source");
    let source = if source == "owner" {
        // Exact owner evidence, never an assistant's invented attribution.
        let yes:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tape_events WHERE session_id=$1 AND origin='owner' AND kind='message' AND payload->>'role'='user' AND strpos(payload->>'content',$2)>0)").bind(session).bind(&text).fetch_one(&app.db).await?;
        ensure!(
            yes,
            "owner attribution requires an exact span of an authenticated owner message"
        );
        "owner"
    } else {
        "inferred"
    };
    let evidence = json!([{"source":format!("session:{session}"),"locator":"remember tool call","basis":if source=="owner"{"owner_statement"}else{"inference"}}]);
    let key = a["key"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("explicit:{session}:{}", hash(&text)));
    let v = add(
        &app.db,
        Some(session),
        Capture {
            text: &text,
            source,
            kind: a["kind"].as_str().unwrap_or("experience"),
            evidence: &evidence,
            entities: a.get("entities").unwrap_or(&json!([])),
            pinned: a["pinned"].as_bool().unwrap_or(false),
            key: &key,
        },
    )
    .await?;
    export(&app.db).await?;
    let used:i64=sqlx::query_scalar("SELECT COALESCE(sum(length(text)+80),0)::bigint FROM memories WHERE tier='short' AND risk<4 AND NOT zen_memory_contaminated(id)").fetch_one(&app.db).await?;
    if v["state"] == "pending" || used as usize > crate::memory::cap() {
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = consolidate(&app, "ceiling").await {
                tracing::warn!("sleep: {e:#}");
            }
        });
    }
    Ok(v.to_string())
}
pub(crate) async fn get(db: &PgPool, id: &str, session: Option<Uuid>) -> Result<Value> {
    let id = memory_id(id)?;
    let v = memory_json(db, id).await?;
    if let Some(s) = session {
        let writers: Vec<Uuid> =
            sqlx::query_scalar("SELECT writer FROM brain_memory_inputs WHERE memory_id=$1")
                .bind(id)
                .fetch_all(db)
                .await?;
        crate::sources::depend(db, s, &writers).await?;
        let writer = v["session_id"].as_str().and_then(|s| s.parse().ok());
        admit(db, s, v["risk"].as_i64().unwrap_or(4) as i16, writer).await?;
    }
    Ok(v)
}
pub(crate) async fn edit(db: &PgPool, session: Option<Uuid>, a: &Value) -> Result<Value> {
    guard(db, session).await?;
    let id = memory_id(&string(a, "id"))?;
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(704,5)")
        .execute(&mut *tx)
        .await?;
    let old: Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM memories m WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let revision = old["revision"].as_i64().unwrap_or(0);
    ensure!(
        a["expected_revision"].as_i64() == Some(revision),
        "expected_revision required; current revision is {revision}"
    );
    if let Some(s) = session {
        admit(
            db,
            s,
            old["risk"].as_i64().unwrap_or(4) as i16,
            old["session_id"].as_str().and_then(|s| s.parse().ok()),
        )
        .await?;
    }
    let action = string(a, "action");
    match action.as_str() {
        "correct" => {
            let text = crate::secrets::mask(&string(a, "text"));
            ensure!(
                !text.is_empty() && text.chars().count() <= MEMORY_LIMIT,
                "correction needs 1..600 characters"
            );
            sqlx::query("UPDATE memories SET text=$2,session_id=COALESCE($3,session_id),source='inferred',updated_at=now() WHERE id=$1").bind(id).bind(text).bind(session).execute(&mut *tx).await?;
        }
        "annotate" => {
            let details = a.get("details").cloned().unwrap_or(json!({}));
            ensure!(
                details.is_object() && details.to_string().len() <= 16000,
                "details must be an object up to 16k bytes"
            );
            let event_at = a["event_at"]
                .as_str()
                .map(str::parse::<chrono::DateTime<chrono::Utc>>)
                .transpose()?;
            sqlx::query("UPDATE memories SET details=$2,event_at=COALESCE($3,event_at),updated_at=now() WHERE id=$1").bind(id).bind(details).bind(event_at).execute(&mut *tx).await?;
        }
        "merge" => {
            let other = memory_id(&string(a, "merge_id"))?;
            ensure!(other != id, "cannot merge memory into itself");
            let source: Value =
                sqlx::query_scalar("SELECT to_jsonb(m) FROM memories m WHERE id=$1 FOR UPDATE")
                    .bind(other)
                    .fetch_one(&mut *tx)
                    .await?;
            ensure!(
                source["revision"].as_i64() == a["merge_revision"].as_i64(),
                "source merge_revision required"
            );
            let text = crate::secrets::mask(&string(a, "text"));
            ensure!(
                !text.is_empty() && text.chars().count() <= 600,
                "supply a concise merged account"
            );
            sqlx::query("INSERT INTO brain_memory_inputs SELECT $1,writer FROM brain_memory_inputs WHERE memory_id=$2 ON CONFLICT DO NOTHING").bind(id).bind(other).execute(&mut *tx).await?;
            let evidence = json!({"merged_from":[old["evidence"].clone(),source["evidence"].clone()],"source_memory":other});
            sqlx::query("UPDATE memories SET text=$2,tier='long',details=details||$3,updated_at=now() WHERE id=$1").bind(id).bind(text).bind(evidence).execute(&mut *tx).await?;
            sqlx::query("UPDATE memories SET risk=GREATEST(risk,$2) WHERE id=$1")
                .bind(id)
                .bind(source["risk"].as_i64().unwrap_or(4) as i16)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE memories SET tier='archived',reason=$2,updated_at=now() WHERE id=$1",
            )
            .bind(other)
            .bind(format!("merged into m{id}"))
            .execute(&mut *tx)
            .await?;
        }
        "pin" | "promote" | "archive" => {
            sqlx::query("UPDATE memories SET pinned=CASE WHEN $2='pin' THEN true ELSE pinned END,tier=CASE WHEN $2='archive' THEN 'archived' ELSE 'long' END,reason=$2,updated_at=now() WHERE id=$1").bind(id).bind(&action).execute(&mut *tx).await?;
        }
        "undo" => {
            let rev = a["revision"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("revision to restore required"))?;
            let prior: Value = sqlx::query_scalar(
                "SELECT snapshot FROM brain_revisions WHERE record_id=$1 AND revision=$2",
            )
            .bind(format!("m{id}"))
            .bind(rev)
            .fetch_one(&mut *tx)
            .await?;
            // Restore content but never lower current provenance or reactivate a denied writer.
            sqlx::query("UPDATE memories SET text=$2,tier=$3,pinned=$4,reason='owner undo',updated_at=now() WHERE id=$1")
                .bind(id).bind(string(&prior,"text")).bind(string(&prior,"tier")).bind(prior["pinned"].as_bool().unwrap_or(false)).execute(&mut *tx).await?;
        }
        _ => anyhow::bail!("unknown memory edit"),
    }
    enqueue_export(&mut tx).await?;
    tx.commit().await?;
    export(db).await?;
    Ok(json!({"ok":true,"id":format!("m{id}"),"revision":revision+1}))
}
async fn export_file(
    db: &PgPool,
    path: &std::path::Path,
    text: &str,
    records: &[Value],
) -> Result<()> {
    // Ratings are registered before publication, with every originating writer. A projection
    // containing several memories inherits every dependency, including denied writers.
    for v in records {
        let writers: Vec<Uuid> =
            sqlx::query_scalar("SELECT writer FROM brain_memory_inputs WHERE memory_id=$1")
                .bind(v["id"].as_i64())
                .fetch_all(db)
                .await?;
        for writer in writers {
            sqlx::query("INSERT INTO item_ratings(path,sha256,session_id,risk) VALUES($1,$2,$3,$4) ON CONFLICT(path,sha256,session_id) DO UPDATE SET risk=GREATEST(item_ratings.risk,EXCLUDED.risk)").bind(path.to_string_lossy().as_ref()).bind(hash(text)).bind(writer).bind(v["risk"].as_i64().unwrap_or(4) as i16).execute(db).await?;
        }
        sqlx::query("INSERT INTO item_ratings(path,sha256,session_id,risk) VALUES($1,$2,$3,$4) ON CONFLICT(path,sha256,session_id) DO UPDATE SET risk=GREATEST(item_ratings.risk,EXCLUDED.risk)")
            .bind(path.to_string_lossy().as_ref()).bind(hash(text)).bind(v["session_id"].as_str().and_then(|s|s.parse::<Uuid>().ok())).bind(v["risk"].as_i64().unwrap_or(4) as i16).execute(db).await?;
    }
    super::records::private_write(path, text).await
}
pub(crate) async fn export(db: &PgPool) -> Result<()> {
    let mut lease = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(704,7)")
        .execute(&mut *lease)
        .await?;
    let rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(m) FROM memories m ORDER BY id")
        .fetch_all(db)
        .await?;
    let mut included = vec![];
    let mut body =
        String::from("# Short-term memory\n\nSelected experience; use remember/brain to edit.\n\n");
    for v in &rows {
        if v["tier"] != "short" || v["risk"].as_i64().unwrap_or(4) >= 4 {
            continue;
        }
        let stopped: bool =
            sqlx::query_scalar("SELECT zen_memory_contaminated(id) FROM memories WHERE id=$1")
                .bind(v["id"].as_i64().unwrap())
                .fetch_one(db)
                .await?;
        if stopped {
            continue;
        }
        let l = line(v);
        if body.chars().count() + l.chars().count() <= cap() {
            body.push_str(&l);
            included.push(v.clone());
        }
    }
    export_file(db, &root().join("MEMORY.md"), &body, &included).await?;
    for v in rows
        .iter()
        .filter(|v| v["tier"] == "long" || v["tier"] == "archived")
    {
        let name = format!("m{}.md", v["id"]);
        let folder = if v["tier"] == "archived" {
            "memory/archive"
        } else {
            "memory/records"
        };
        let text = format!(
            "---\n{}\n---\n\n{}\n",
            serde_json::to_string_pretty(v)?,
            string(v, "text")
        );
        export_file(
            db,
            &root().join(folder).join(name),
            &text,
            std::slice::from_ref(v),
        )
        .await?;
    }
    // Complete export includes pending and withheld records and their metadata, for owner recovery.
    export_file(
        db,
        &root().join("memory/export.json"),
        &serde_json::to_string_pretty(&rows)?,
        &rows,
    )
    .await?;
    Ok(())
}
pub async fn consolidate(app: &crate::AppState, trigger: &str) -> Result<Value> {
    let db = &app.db;
    let mut connection = db.begin().await?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(704,6)")
        .fetch_one(&mut *connection)
        .await?;
    ensure!(acquired, "sleep already running");
    let result = consolidate_inner(app, trigger).await;
    if let Err(e) = &result {
        sqlx::query("UPDATE sleep_runs SET ended_at=now(),error=$1 WHERE ended_at IS NULL")
            .bind(format!("{e:#}"))
            .execute(db)
            .await?;
    }
    // Session advisory lock lives on this dedicated connection; release even when the operation errs.
    connection.rollback().await?;
    result
}
async fn consolidate_inner(app: &crate::AppState, trigger: &str) -> Result<Value> {
    let db = &app.db;
    let run: i64 =
        sqlx::query_scalar("INSERT INTO sleep_runs(trigger,scorer) VALUES($1,$2) RETURNING id")
            .bind(trigger)
            .bind(crate::s1::default_model())
            .fetch_one(db)
            .await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(m) FROM memories m WHERE tier IN ('short','pending') ORDER BY id LIMIT 200").fetch_all(db).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(
            |v| json!({"id":v["id"],"text":v["text"],"source":v["source"],"kind":v["record_type"]}),
        )
        .collect();
    let mut decisions = vec![];
    let mut dispositions = vec![];
    for batch in items.chunks(12) {
        let (durable, disposition) = tokio::join!(
            crate::s1::decide_items(
                db,
                "memory_durable",
                crate::s1::Facts::new(None),
                batch.to_vec()
            ),
            crate::s1::decide_items(
                db,
                "memory_disposition",
                crate::s1::Facts::new(None),
                batch.to_vec()
            )
        );
        decisions.extend(durable);
        dispositions.extend(disposition);
    }
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(704,5)")
        .execute(&mut *tx)
        .await?;
    let (mut promoted, mut kept, mut held, mut archived) = (0, 0, 0, 0);
    for (i, v) in rows.iter().enumerate() {
        let fresh: bool =
            sqlx::query_scalar("SELECT revision=$2 FROM memories WHERE id=$1 FOR UPDATE")
                .bind(v["id"].as_i64())
                .bind(v["revision"].as_i64())
                .fetch_one(&mut *tx)
                .await?;
        if !fresh {
            held += 1;
            continue;
        }
        let pinned = v["pinned"] == true;
        let durable = decisions.get(i).is_some_and(|d| {
            d.answered
                && d.prob("durable")
                    .is_some_and(|p| p >= crate::config::get().promote_bar)
        });
        if pinned || durable {
            sqlx::query("UPDATE memories SET tier='long',reason='sleep: durable experience',updated_at=now() WHERE id=$1").bind(v["id"].as_i64()).execute(&mut *tx).await?;
            promoted += 1;
        } else if !pinned
            && dispositions.get(i).is_some_and(|d| {
                d.answered
                    && d.choice("disposition") == Some("archive")
                    && d.answers["disposition"]["probabilities"]["archive"]
                        .as_f64()
                        .is_some_and(|p| p >= 0.95)
            })
        {
            sqlx::query("UPDATE memories SET tier='archived',reason='sleep: no longer active; evidence retained',updated_at=now() WHERE id=$1").bind(v["id"].as_i64()).execute(&mut *tx).await?;
            archived += 1;
        } else {
            kept += 1;
        }
    }
    let used:i64=sqlx::query_scalar("SELECT COALESCE(sum(length(text)+80),0)::bigint FROM memories WHERE tier='short' AND risk<4 AND NOT zen_memory_contaminated(id)").fetch_one(&mut *tx).await?;
    let mut used = used as usize;
    let pending: Vec<(i64, String)> =
        sqlx::query_as("SELECT id,text FROM memories WHERE tier='pending' ORDER BY id FOR UPDATE")
            .fetch_all(&mut *tx)
            .await?;
    for (id, text) in pending {
        if used + text.chars().count() + 80 > cap() {
            break;
        }
        sqlx::query("UPDATE memories SET tier='short',updated_at=now() WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        used += text.chars().count() + 80;
    }
    let note=format!("Promoted {promoted}; kept {kept}; archived {archived}; changed {held}. Uncertain entries retained; prompt files unchanged.");
    sqlx::query("UPDATE sleep_runs SET ended_at=now(),entries=$2,kept=$3,promoted=$4,dropped=$6,note=$5 WHERE id=$1").bind(run).bind(rows.len() as i32).bind(kept).bind(promoted).bind(&note).bind(archived).execute(&mut *tx).await?;
    enqueue_export(&mut tx).await?;
    tx.commit().await?;
    export(db).await?;
    Ok(
        json!({"run":run,"promoted":promoted,"kept":kept,"changed":held,"archived":archived,"note":note}),
    )
}
pub async fn capture_turn(app: &crate::AppState, session: Uuid, turn: Uuid) -> Result<()> {
    let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM sessions WHERE id=$1")
        .bind(session)
        .fetch_one(&app.db)
        .await?;
    if kind.is_some() || crate::config::get().brain_mode != "on" {
        return Ok(());
    }
    queue(
        &app.db,
        "capture",
        &format!("turn:{turn}"),
        json!({"session":session,"turn":turn}),
    )
    .await
}
pub(crate) async fn process_capture(app: &crate::AppState, p: &Value) -> Result<()> {
    let session: Uuid = string(p, "session").parse()?;
    let turn: Uuid = string(p, "turn").parse()?;
    // Only the authenticated owner message for this completed turn; never assistant paraphrases.
    let input:Option<(i32,Value)>=sqlx::query_as("SELECT seq,payload FROM tape_events WHERE session_id=$1 AND kind='message' AND origin='owner' AND payload->>'role'='user' AND created_at >= (SELECT started_at FROM turns WHERE id=$2) ORDER BY seq LIMIT 1").bind(session).bind(turn).fetch_optional(&app.db).await?;
    let Some((seq, payload)) = input else {
        return Ok(());
    };
    let text = payload["content"].as_str().unwrap_or_default();
    let spans: Vec<String> = text
        .split(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|s| s.chars().count() >= 12 && s.chars().count() <= 600)
        .take(30)
        .map(str::to_owned)
        .collect();
    let choices = crate::s1::decide_items(
        &app.db,
        "memory_capture",
        crate::s1::Facts::new(Some(session)),
        spans.iter().map(|s| json!({"owner_statement":s})).collect(),
    )
    .await;
    ensure!(
        choices.len() == spans.len() && choices.iter().all(|d| d.answered),
        "capture classifier unavailable; retry from durable source"
    );
    for (i, (span, d)) in spans.iter().zip(choices).enumerate() {
        if d.prob("remember").is_some_and(|p| p >= 0.9) {
            let ev = json!([{"source":format!("session:{session}"),"locator":format!("message:{seq}"),"basis":"owner_statement"}]);
            add(
                &app.db,
                Some(session),
                Capture {
                    text: span,
                    source: "owner",
                    kind: "experience",
                    evidence: &ev,
                    entities: &json!([]),
                    pinned: false,
                    key: &format!("capture:{turn}:{i}"),
                },
            )
            .await?;
        }
    }
    export(&app.db).await
}
pub(crate) async fn dream(app: &crate::AppState, session: Option<Uuid>) -> Result<Value> {
    let rows:Vec<(i64,String)>=sqlx::query_as("SELECT id,text FROM memories WHERE tier='long' AND risk<4 AND NOT zen_memory_contaminated(id) ORDER BY updated_at DESC LIMIT 30").fetch_all(&app.db).await?;
    if let Some(s) = session {
        for (id, _) in &rows {
            get(&app.db, &format!("m{id}"), Some(s)).await?;
        }
    }
    let mut proposals = vec![];
    for (i, (a, ta)) in rows.iter().enumerate() {
        for (b, tb) in rows.iter().skip(i + 1) {
            let words: std::collections::HashSet<_> = ta
                .split_whitespace()
                .filter(|w| w.len() > 4)
                .map(str::to_lowercase)
                .collect();
            let common = tb
                .split_whitespace()
                .filter(|w| words.contains(&w.to_lowercase()))
                .count();
            if common >= 2 {
                proposals.push(json!({"records":[format!("m{a}"),format!("m{b}")],"status":"proposal","question":"Do these experiences share a useful pattern?","basis":"lexical overlap, not an established relationship"}));
            }
        }
    }
    Ok(json!({"proposals":proposals.into_iter().take(10).collect::<Vec<_>>(),"published":false}))
}
