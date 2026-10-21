//! Re-rating requires authenticated exact result bytes. Legacy origin is never invented.
//! Missing content, non-web reasons, owner overrides and contamination are left unchanged.
use super::*;

/// Recompute authenticated web blocks only; max aggregation preserves every other reason.
/// Only items with new, explicit writer-only evidence can lower; historical reasons stay unknown.
pub async fn rerate(db: &PgPool) -> Result<Value> {
    let rows = sqlx::query("SELECT e.session_id,e.seq,e.payload,e.risk,e.tool_call_record,t.origin,t.result_sha256
        FROM tape_events e JOIN tool_calls t ON t.id=e.tool_call_record
        JOIN sessions s ON s.id=e.session_id
        WHERE e.kind='message' AND e.risk BETWEEN 2 AND 4 AND t.name='web_fetch'
        AND NOT t.is_error AND e.origin LIKE 'web_fetch:%' AND t.origin=e.origin
        AND s.risk_trusted_through=0 AND NOT zen_session_contaminated(s.id)")
        .fetch_all(db).await?;
    let mut changed=0u64;
    let mut items=0u64;
    let mut complete=true;
    for row in rows {
        let payload: Value=row.get("payload");
        let text=zen_proto::text_of(&payload["content"]);
        let hash: String=sqlx::query_scalar("SELECT encode(sha256(convert_to($1,'UTF8')),'hex')").bind(zen_proto::digest_text(&payload["content"])).fetch_one(db).await?;
        if row.get::<Option<String>,_>("result_sha256").as_deref()!=Some(&hash) { continue; }
        let origin: String=row.get("origin");
        let Some(url)=origin.strip_prefix("web_fetch:") else {continue};
        let risk=rated(db,url,"historical_authenticated_fetch").await?;
        if risk>=row.get::<i16,_>("risk") {
            let scope=key(url);
            let cached=if let Ok(scope)=scope {
                sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM source_ratings WHERE scope=$1 AND rated_at>now()-interval '30 days')")
                    .bind(scope).fetch_one(db).await?
            } else {false};
            if risk==4 && !cached && base(db,url).await?==4 {complete=false}
            continue
        }
        let (risk,available)=model::content_rating(db,&text,risk).await;
        complete &= available;
        if risk>=row.get::<i16,_>("risk") {continue}
        let sid:Uuid=row.get("session_id");
        let mut tx=db.begin().await?;
        lock_provenance(&mut tx).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,7))").bind(sid).execute(&mut *tx).await?;
        // Chain/session lock serializes appends and risk raises during this decision.
        let allowed: bool=sqlx::query_scalar("SELECT risk_trusted_through=0 AND NOT zen_session_contaminated(id) FROM sessions WHERE id=$1 FOR UPDATE")
            .bind(sid).fetch_one(&mut *tx).await?;
        if !allowed {continue}
        let old_max: i16=sqlx::query_scalar("SELECT COALESCE(max(risk),1)::smallint FROM tape_events WHERE session_id=$1").bind(sid).fetch_one(&mut *tx).await?;
        changed+=sqlx::query("UPDATE tape_events SET risk=$3 WHERE session_id=$1 AND seq=$2 AND risk=$4 AND tool_call_record=$5")
            .bind(sid).bind(row.get::<i32,_>("seq")).bind(risk).bind(row.get::<i16,_>("risk")).bind(row.get::<i64,_>("tool_call_record"))
            .execute(&mut *tx).await?.rows_affected();
        // Retain the tool record's old rating: its digest may have multiple consumers and those
        // lack independent content-check evidence. Tape metadata is the per-read authoritative risk.
        // No lowering if any legacy/risky/other event still supplies the old maximum.
        sqlx::query("UPDATE sessions SET risk=GREATEST(1,(SELECT COALESCE(max(risk),1) FROM tape_events WHERE session_id=$1))
            WHERE id=$1 AND risk BETWEEN 2 AND 4 AND risk_trusted_through=0
            AND risk=$2 AND NOT EXISTS(SELECT 1 FROM session_inputs WHERE reader=$1) AND parent IS NULL")
            .bind(sid).bind(old_max).execute(&mut *tx).await?;
        for table in ["memories","job_runs"] {
            items += sqlx::query(&format!("UPDATE {table} i SET risk=s.risk FROM sessions s
                WHERE i.session_id=$1 AND s.id=$1 AND i.rating_reason='writer_session'
                AND i.risk=i.rating_writer_risk AND i.risk BETWEEN 2 AND 4 AND s.risk<i.risk
                AND NOT zen_session_contaminated(s.id)"))
                .bind(sid).execute(&mut *tx).await?.rows_affected();
        }
        tx.commit().await?;
    }
    Ok(json!({"web_blocks":changed,"items_lowered":items,"complete":complete,"note":"missing or non-exclusive provenance retained"}))
}

/// Idempotent startup pass. An unavailable classifier cannot lower anything.
pub async fn rerate_once(db: &PgPool) -> Result<()> {
    let done: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_rerates WHERE version=65)").fetch_one(db).await?;
    if done || !crate::s1::on() || !crate::s1::openrouter_key() {return Ok(())}
    let result=rerate(db).await?;
    if result["complete"]!=true {return Ok(())}
    sqlx::query("INSERT INTO source_rerates(version,note) VALUES(65,'authenticated web bytes only; ambiguous historical provenance retained') ON CONFLICT DO NOTHING").execute(db).await?;
    Ok(())
}
