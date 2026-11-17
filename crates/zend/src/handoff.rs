//! A partial session's handoff. When the owner archives a session as `partial`, the kernel starts
//! a new session for the same agent, titled "Continue: <old title>" and linked to the old one
//! (`sessions.continues`), and the summary model writes a handoff brief from the old session's
//! tape: the job, what is done (with its evidence), what is left, the key decisions, and the
//! files, branches and pull requests involved. The brief is offered in the new session as its
//! suggested first prompt (`prompt_suggestions`, shown like any suggestion); no turn starts
//! until the owner sends it, edited or not, so its outcome is measured like every suggestion's.

use super::*;

/// The prompt below, recorded with each brief (`prompt_suggestions.prompt_version`).
const PROMPT_VERSION: &str = "handoff-v1";

const SYSTEM: &str = "You write a handoff brief: the first message of a new session with an AI agent, \
which continues a job the previous session left partly done. You read that session (the owner is User, \
the agent Assistant; tool calls are shown briefly) and reply with only the brief, in Markdown, written \
as the owner asking the agent to continue, in the owner's language. Sections, each short and concrete: \
**Job** (what the owner wants, in one or two sentences); **Done** (what was finished, each with its \
evidence: tests, runs, commits); **Left** (what remains, most important first); **Decisions** (choices \
made and why, the owner's above all); **Where** (files, branches, commits, pull requests, paths). Never \
invent anything the session doesn't contain; say \"unknown\" when it isn't clear. At most 400 words.";

/// Start the session that continues `old` and write its brief in the background. Returns the new
/// session's id at once (the brief takes a summary call).
pub(crate) async fn start(app: &AppState, old: Uuid) -> ApiResult<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, title, model, effort, title_source, model_source, agent, workspace, continues)
         SELECT $1, 'Continue: ' || CASE WHEN title = '' THEN 'untitled' ELSE title END, model, effort, 'owner', model_source, agent, workspace, id
         FROM sessions WHERE id = $2",
    )
    .bind(id)
    .bind(old)
    .execute(&app.db)
    .await?;
    let (app, work) = (app.clone(), app.background_work());
    tokio::spawn(async move {
        let _work = work;
        if let Err(e) = brief(&app, old, id).await {
            tracing::warn!("handoff brief from session {old} to {id}: {e:#}");
        }
    });
    Ok(id)
}

/// Write the brief and offer it in `new` as its suggested first prompt.
async fn brief(app: &AppState, old: Uuid, new: Uuid) -> Result<()> {
    let blocks = tape::load(&app.db, old, &["message"]).await?;
    let mut model = compact::summary_model();
    let input = input(&blocks);
    let started = Instant::now();
    let (text, cost) = match crate::complete(app, &model, SYSTEM, &input).await {
        Ok(r) => (r.text.trim().to_string(), r.usage["cost_usd"].as_f64()),
        Err(e) => {
            // Without a model the brief is still a pointer to where the work is.
            tracing::warn!("handoff brief with {model}: {e:#}");
            model = "none".into();
            (format!("Continue the job from session {old}: read its last messages, say what is left, then carry on."), None)
        }
    };
    let text = if text.is_empty() { format!("Continue the job from session {old}.") } else { text };
    let sid: i64 = sqlx::query_scalar(
        "INSERT INTO prompt_suggestions (session_id, after_seq, suggested, model, prompt_version, latency_ms, cost_usd)
         VALUES ($1, 0, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(new)
    .bind(&text)
    .bind(&model)
    .bind(PROMPT_VERSION)
    .bind(started.elapsed().as_millis() as i32)
    .bind(cost)
    .fetch_one(&app.db)
    .await?;
    app.emit(new, json!({ "type": "suggestion", "id": sid, "text": text })).await;
    Ok(())
}

/// What the summary model reads: the session's messages, owner and agent text with tool calls
/// named (their inputs cut short), the first prompt always, then the most recent within budget.
fn input(blocks: &[tape::Block]) -> String {
    const BUDGET: usize = 60_000;
    let line = |m: &Value| -> Option<String> {
        let role = m["role"].as_str()?;
        let mut parts = Vec::new();
        for c in m["content"].as_array().into_iter().flatten() {
            match c["type"].as_str() {
                Some("text") => parts.push(zen_proto::head(c["text"].as_str().unwrap_or(""), 3_000).to_string()),
                Some("tool_use") => parts.push(format!("[tool {} {}]", c["name"].as_str().unwrap_or("?"), zen_proto::head(&c["input"].to_string(), 300))),
                _ => {}
            }
        }
        if let Some(t) = m["content"].as_str() {
            parts.push(zen_proto::head(t, 3_000).to_string());
        }
        let text = parts.join("\n");
        let who = match role {
            "user" if m["kernel"] != true => "User",
            "assistant" => "Assistant",
            _ => return None,
        };
        (!text.trim().is_empty()).then(|| format!("{who}: {text}"))
    };
    let lines: Vec<String> = blocks.iter().filter_map(|b| line(&b.payload)).collect();
    let mut recent = Vec::new();
    let mut used = 0;
    for l in lines.iter().skip(1).rev() {
        if used + l.len() > BUDGET {
            break;
        }
        used += l.len();
        recent.push(l.as_str());
    }
    recent.reverse();
    let mut out = String::from("The session:\n\n");
    if let Some(first) = lines.first() {
        out.push_str(first);
        out.push_str("\n\n");
    }
    if recent.len() + 1 < lines.len() {
        out.push_str("[…]\n\n");
    }
    out.push_str(&recent.join("\n\n"));
    out.push_str("\n\nWrite the handoff brief.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Partial: a new linked session for the same agent, offered the brief, with no turn started.
    #[tokio::test]
    async fn partial_starts_a_linked_session_offered_the_brief() {
        let (_db_dir, db) = crate::test_util::database().await;
        let app = crate::test_util::app(db.clone());
        let old = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model,title,agent) VALUES($1,'faux/smoke','Fix the parser','garry')").bind(old).execute(&db).await.unwrap();
        let new = start(&app, old).await.ok().unwrap();
        let row = sqlx::query("SELECT title, agent, continues, archived FROM sessions WHERE id=$1").bind(new).fetch_one(&db).await.unwrap();
        assert_eq!(row.get::<String, _>("title"), "Continue: Fix the parser");
        assert_eq!(row.get::<String, _>("agent"), "garry");
        assert_eq!(row.get::<Option<Uuid>, _>("continues"), Some(old));
        let mut brief = None;
        for _ in 0..100 {
            brief = sqlx::query_scalar::<_, String>("SELECT suggested FROM prompt_suggestions WHERE session_id=$1 AND outcome IS NULL").bind(new).fetch_optional(&db).await.unwrap();
            if brief.is_some() { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(brief.is_some_and(|b| !b.is_empty()), "the brief is offered as a suggestion");
        let turns: i64 = sqlx::query_scalar("SELECT count(*) FROM turns WHERE session_id=$1").bind(new).fetch_one(&db).await.unwrap();
        assert_eq!(turns, 0, "no turn starts on its own");
        db.close().await;
    }

    #[test]
    fn the_input_keeps_the_first_prompt_and_names_tools() {
        let b = |seq, p: Value| tape::Block { seq, kind: "message".into(), payload: p };
        let blocks = vec![
            b(1, json!({"role":"user","content":[{"type":"text","text":"Fix the parser"}]})),
            b(2, json!({"role":"assistant","content":[{"type":"tool_use","name":"bash","input":{"command":"cargo test"}}]})),
            b(3, json!({"role":"user","kernel":true,"content":"internal"})),
        ];
        let s = input(&blocks);
        assert!(s.contains("User: Fix the parser") && s.contains("[tool bash") && !s.contains("internal"));
    }
}
