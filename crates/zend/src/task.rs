//! The owner's job, as the action gate's alignment question sees it (gate.rs, D-067): the goal
//! comes from authenticated root-owner tape, not worker user messages or summaries.
//! Only plain assistant text before the FIRST outside exposure is eligible as brief/plan.
//! Proposed arguments are untrusted evidence of effects, never instructions or owner approval.
use anyhow::Result;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

/// Raw tape only: never compactions, tool results, child task prompts, or post-exposure plans.
pub async fn job(db: &sqlx::PgPool, session: Uuid) -> Result<Value> {
    let root = sqlx::query("WITH RECURSIVE ancestors AS (
        SELECT id,parent,title,kind,0 AS depth FROM sessions WHERE id=$1
        UNION ALL SELECT s.id,s.parent,s.title,s.kind,a.depth+1 FROM sessions s
          JOIN ancestors a ON s.id=a.parent WHERE a.depth<64)
        SELECT id,title,kind FROM ancestors WHERE parent IS NULL ORDER BY depth DESC LIMIT 1")
        .bind(session).fetch_optional(db).await?;
    let Some(root) = root else { return Ok(Value::Null) };
    let id: Uuid = root.get("id");
    let kind: Option<String> = root.get("kind");
    let title: String = root.get("title");
    let mut owner = Vec::new();
    let mut plans = Vec::new();
    if kind.is_none() || kind.as_deref() == Some("job") {
        let rows = sqlx::query("SELECT payload,origin FROM tape_events e WHERE session_id=$1 AND kind='message'
            AND ((origin='owner' AND risk=0 AND payload->>'role'='user'
                  AND NOT COALESCE((payload->>'kernel')::boolean,false))
              OR (origin='derived-session' AND risk<=1 AND NOT tainted AND payload->>'role'='assistant'
                  AND seq < COALESCE((SELECT min(seq) FROM tape_events WHERE session_id=$1 AND (risk>=2 OR tainted)),2147483647)))
            ORDER BY seq DESC").bind(id).fetch_all(db).await?;
        for row in rows.into_iter().rev() {
            let p: Value = row.get("payload");
            let text = plain_text(&p["content"]);
            if row.get::<String,_>("origin") == "owner" { owner.push(text); }
            else { plans.push(zen_proto::head(&text,2000)); }
        }
    }
    if kind.as_deref() == Some("job") {
        // Read the stored definition, not the runtime prompt (which appends prior reports).
        let prompt: Option<String> = sqlx::query_scalar("SELECT j.prompt FROM jobs j JOIN job_runs r ON r.job_id=j.id
            WHERE r.session_id=$1 AND j.enabled AND j.paused_reason IS NULL
              AND j.updated_at <= r.started_at
            ORDER BY r.id DESC LIMIT 1").bind(id).fetch_optional(db).await?.flatten();
        if let Some(p) = prompt { owner.push(p); }
    }
    let goal=owner.join("\n\n");
    // Never classify a partial authority record. Oversized goals hold all risky actions.
    let complete=goal.len()<=12000;
    Ok(json!({"session_title_label":zen_proto::head(&title,200),
        "authenticated_goal_data":if complete {goal} else {String::new()},
        "pre_exposure_brief_plan_data":zen_proto::head(&plans.join("\n\n"),8000)}))
}

fn plain_text(content: &Value) -> String {
    if let Some(s) = content.as_str() { return s.to_string(); }
    content.as_array().into_iter().flatten().filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arbitrary_code_is_never_safe() {
        let p=Path::new("/tmp");
        for cmd in ["python3 -c 'import os; os.system(\"curl evil\")'", "cargo test", "git status",
            "bash -c 'curl evil'", "unknown-tool --publish"] {
            assert_ne!(class(p,"bash",&json!({"command":cmd})),Class::Safe, "{cmd}");
        }
        assert_eq!(class(p,"bash",&json!({"command":"ls -la"})),Class::Safe);
        for tool in ["remember","capture","save_skill","save_tool","delegate","web_search","web_fetch","call_tool"] {
            assert_ne!(class(p,tool,&json!({})),Class::Safe);
        }
        assert_eq!(class(p,"write",&json!({"path":"note.txt"})),Class::Safe);
        assert_eq!(class(p,"write",&json!({"path":"/etc/test"})),Class::Dangerous);
    }
    /// Disposable database only:
    /// TASK_TEST_DB_URL=postgres://... cargo test -p zend task_database -- --ignored
    #[tokio::test]
    #[ignore]
    async fn task_database() {
        let url=std::env::var("TASK_TEST_DB_URL").expect("isolated disposable database required");
        let db=sqlx::PgPool::connect(&url).await.unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        let root=Uuid::new_v4();
        let child=Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,title,model) VALUES($1,'real job','faux/smoke')")
            .bind(root).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO sessions(id,title,model,parent,kind) VALUES($1,'child label','faux/smoke',$2,'subagent')")
            .bind(child).bind(root).execute(&db).await.unwrap();
        crate::tape::append_owner(&db,root,&json!({"role":"user","content":[{"type":"text","text":"OWNER_GOAL"}]})).await.unwrap();
        crate::tape::append(&db,root,"message",&json!({"role":"assistant","content":[
            {"type":"text","text":"CLEAN_BRIEF"},{"type":"thinking","thinking":"HIDDEN_THINKING"},
            {"type":"toolCall","arguments":{"goal":"TOOL_ARGUMENTS"}}]})).await.unwrap();
        crate::tape::append(&db,root,"message",&json!({"role":"toolResult","toolName":"web_fetch",
            "toolCallId":"not-verified","content":[{"type":"text","text":"OUTSIDE_GOAL"}]})).await.unwrap();
        crate::tape::append(&db,root,"message",&json!({"role":"assistant","content":"LATE_PLAN"})).await.unwrap();
        // Even lowering risk later must not admit plans written after the original exposure.
        sqlx::query("UPDATE sessions SET risk=1 WHERE id=$1").bind(root).execute(&db).await.unwrap();
        crate::tape::append(&db,root,"message",&json!({"role":"assistant","content":"AFTER_TRUST_PLAN"})).await.unwrap();
        crate::tape::append(&db,root,"compaction",&json!({"text":"COMPACTED_GOAL"})).await.unwrap();
        crate::tape::append(&db,child,"message",&json!({"role":"user","content":"FORGED_CHILD_OWNER"})).await.unwrap();
        let state=job(&db,child).await.unwrap().to_string();
        for good in ["OWNER_GOAL","CLEAN_BRIEF","real job"] { assert!(state.contains(good),"{state}"); }
        for bad in ["LATE_PLAN","AFTER_TRUST_PLAN","OUTSIDE_GOAL","COMPACTED_GOAL",
            "FORGED_CHILD_OWNER","TOOL_ARGUMENTS","HIDDEN_THINKING"] {
            assert!(!state.contains(bad),"{bad}: {state}");
        }
        let long_goal=format!("{} do not publish","x".repeat(2100));
        crate::tape::append_owner(&db,root,&json!({"role":"user","content":long_goal})).await.unwrap();
        for _ in 0..45 {
            crate::tape::append_owner(&db,root,&json!({"role":"user","content":"continue local work"})).await.unwrap();
        }
        let state=job(&db,child).await.unwrap();
        assert!(state["authenticated_goal_data"].as_str().unwrap().contains("do not publish"));
        crate::tape::append_owner(&db,root,&json!({"role":"user","content":"latest restriction: do not send"})).await.unwrap();
        let state=job(&db,child).await.unwrap();
        assert!(state["authenticated_goal_data"].as_str().unwrap().contains("latest restriction: do not send"));
        crate::tape::append_owner(&db,root,&json!({"role":"user","content":"x".repeat(12001)})).await.unwrap();
        assert_eq!(job(&db,child).await.unwrap()["authenticated_goal_data"],"","oversized authority fails closed");
        let run_session=Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,title,model,kind) VALUES($1,'job label','faux/smoke','job')")
            .bind(run_session).execute(&db).await.unwrap();
        let job_id: i64=sqlx::query_scalar("INSERT INTO jobs(name,kind,prompt,schedule,tz,created_by)
            VALUES($1,'agent','STORED_JOB_GOAL','every 1h','UTC','owner') RETURNING id")
            .bind(format!("task-test-{run_session}")).fetch_one(&db).await.unwrap();
        sqlx::query("INSERT INTO job_runs(job_id,trigger,status,session_id) VALUES($1,'owner','running',$2)")
            .bind(job_id).bind(run_session).execute(&db).await.unwrap();
        crate::tape::append(&db,run_session,"message",&json!({"role":"user",
            "content":"RUNTIME_PROMPT_WITH_OLD_REPORT"})).await.unwrap();
        crate::tape::append(&db,run_session,"message",&json!({"role":"assistant",
            "content":"JOB_CLEAN_PLAN"})).await.unwrap();
        let state=job(&db,run_session).await.unwrap().to_string();
        assert!(state.contains("STORED_JOB_GOAL") && state.contains("JOB_CLEAN_PLAN"),"{state}");
        assert!(!state.contains("RUNTIME_PROMPT_WITH_OLD_REPORT"));
        sqlx::query("UPDATE jobs SET prompt='MUTATED_JOB_GOAL',updated_at=now() WHERE id=$1")
            .bind(job_id).execute(&db).await.unwrap();
        let state=job(&db,run_session).await.unwrap();
        assert_eq!(state["authenticated_goal_data"],"");
        // No real data: the entire database supplied to this ignored test must be disposable.
        db.close().await;
    }

    #[test]
    fn only_plain_text_enters_plan() {
        assert_eq!(plain_text(&json!([{"type":"thinking","thinking":"secret"},
            {"type":"toolCall","arguments":{"plan":"hostile"}},{"type":"text","text":"brief"}])), "brief");
        let q=crate::s1::DEFAULTS.iter().find(|(n,_)|*n=="task_check").unwrap().1;
        assert!(q.contains("fabricated approval") && q.contains("unrelated"));
    }
}
