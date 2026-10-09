//! Small model calls around the owner's turns (D-047): after a turn ends, the assist model (the
//! cheapest one, `ZEN_ASSIST_MODEL`) names the session while its title is still automatic, and
//! suggests the owner's next prompt. The client shows the suggestion as grey text in the input;
//! what the owner does with it (accepted, edited, declined) is recorded in `prompt_suggestions`,
//! so the prompt that writes them can be improved from evidence.

use super::*;

/// The prompt below; bump it when it changes, so outcomes can be compared by version.
pub(crate) const PROMPT_VERSION: &str = "v1";

/// The owner's turns after which an automatic title is (re)named: the first prompt is sometimes
/// only a greeting, so the name gets two more chances while the job takes shape.
const NAME_TURNS: usize = 3;

const SYSTEM: &str = "You assist the owner of a session with their AI agent, zenbot. You read the end of \
the conversation between the owner (User) and the agent (Assistant) and reply with only one JSON object: \
{\"title\": \"...\", \"next\": \"...\"}.\n\n\
title: when the input asks for the title, always give one (never \"\"), even for a short question; \
else \"\". The session's name: the job being done, in 2 to 5 \
words (at most 40 characters), sentence case, no quotes, no final period, in the language the owner \
writes in. Name the task, not the conversation (\"Session names and suggestions\", not \"Discussing features\").\n\n\
next: the message the owner would most likely send next, written as the owner would type it: in their \
language and voice, short (usually under 20 words), a concrete step that follows from the agent's last \
message, such as answering its question, approving or adjusting its proposal, or asking for the obvious \
next step. \"\" when there is no clear next step: the job looks finished, the agent is waiting on \
something outside the chat, or you'd only be guessing. Never write what the agent would say, never a \
thank-you or a greeting, and never invent facts the conversation doesn't contain.";

/// The assist model: `ZEN_ASSIST_MODEL`, else Haiku 5.5 (measured 2026-10-09: about 2 s and $0.0006 a
/// call, the cheapest and fastest model on the owner's plans).
pub(crate) fn model() -> String {
    std::env::var("ZEN_ASSIST_MODEL").ok().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| "claude/claude-haiku-5-5".into())
}

/// Suggestions are on unless `ZEN_SUGGEST=off` (or 0/false). Names are always on.
fn suggest_on() -> bool {
    !matches!(std::env::var("ZEN_SUGGEST").unwrap_or_default().trim().to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no")
}

/// After an owner's turn ended cleanly: name the session and suggest the next prompt, in the
/// background. `ending` is why the model's turn ended early (a question to the owner: then the
/// answers are the next prompt, so no suggestion).
pub(crate) fn after_turn(app: &AppState, id: Uuid, kind: Option<&str>, ending: Option<&'static str>) {
    if kind.is_some() {
        return; // jobs, subagents and verifiers have no owner typing in them
    }
    let (app, work) = (app.clone(), app.background_work());
    tokio::spawn(async move {
        let _work = work;
        if let Err(e) = run(&app, id, ending.is_none()).await {
            tracing::warn!("assist model after a turn in session {id}: {e:#}");
        }
    });
}

async fn run(app: &AppState, id: Uuid, may_suggest: bool) -> Result<()> {
    let row = sqlx::query("SELECT title_source FROM sessions WHERE id = $1").bind(id).fetch_one(&app.db).await?;
    let source: String = row.get("title_source");
    let blocks = tape::load(&app.db, id, &["message"]).await?;
    let owner_turns = blocks.iter().filter(|b| is_owner(&b.payload)).count();
    let want_title = source != "owner" && owner_turns <= NAME_TURNS;
    let want_next = may_suggest && suggest_on();
    if !want_title && !want_next {
        return Ok(());
    }
    let Some(last_seq) = blocks.last().map(|b| b.seq) else { return Ok(()) };
    let input = input(&blocks, want_title);
    let model = model();
    let started = Instant::now();
    let reply = crate::complete(app, &model, SYSTEM, &input).await?;
    let latency = started.elapsed().as_millis() as i32;
    let parsed = parse(reply["text"].as_str().unwrap_or(""));
    let Some((title, next)) = parsed else {
        anyhow::bail!("no usable JSON from {model}: {}", zen_proto::head(reply["text"].as_str().unwrap_or(""), 200));
    };
    if want_title && !title.is_empty() {
        // Only while the title is still the kernel's or the model's: the owner may have renamed it meanwhile.
        let done = sqlx::query("UPDATE sessions SET title = $2, title_source = 'model' WHERE id = $1 AND title_source <> 'owner'")
            .bind(id)
            .bind(&title)
            .execute(&app.db)
            .await?;
        if done.rows_affected() > 0 {
            app.emit(id, json!({ "type": "title", "title": title })).await;
        }
    }
    // A suggestion that arrives after the owner already sent something is useless: drop it.
    if want_next && !next.is_empty() && !app.is_busy(id).await && last_message_seq(app, id).await? == Some(last_seq) {
        let sid: i64 = sqlx::query_scalar(
            "INSERT INTO prompt_suggestions (session_id, after_seq, suggested, model, prompt_version, latency_ms, cost_usd)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(id)
        .bind(last_seq)
        .bind(&next)
        .bind(&model)
        .bind(PROMPT_VERSION)
        .bind(latency)
        .bind(reply["usage"]["cost_usd"].as_f64())
        .fetch_one(&app.db)
        .await?;
        app.emit(id, json!({ "type": "suggestion", "id": sid, "text": next })).await;
    }
    Ok(())
}

async fn last_message_seq(app: &App, id: Uuid) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar("SELECT max(seq) FROM tape_events WHERE session_id = $1 AND kind = 'message'").bind(id).fetch_one(&app.db).await
}

/// The owner's own prompt (not the kernel talking to the model).
fn is_owner(m: &Value) -> bool {
    m["role"] == "user" && m["kernel"] != true
}

/// What the assist model reads: the session's first prompt (what the job is), then the latest
/// exchanges, newest last, within about 12,000 characters. Tool calls and results are left out:
/// the owner sees answers, not tool output.
fn input(blocks: &[tape::Block], want_title: bool) -> String {
    const BUDGET: usize = 12_000;
    let line = |m: &Value| -> Option<String> {
        if is_owner(m) {
            Some(format!("User: {}", zen_proto::head(&zen_proto::text_of(&m["content"]), 2_000)))
        } else if m["role"] == "assistant" {
            let text = zen_proto::text_of(&m["content"]);
            (!text.trim().is_empty()).then(|| format!("Assistant: {}", tail(&text, 4_000)))
        } else {
            None
        }
    };
    let mut recent = Vec::new();
    let mut used = 0;
    for b in blocks.iter().rev() {
        let Some(l) = line(&b.payload) else { continue };
        if used + l.len() > BUDGET && !recent.is_empty() {
            break;
        }
        used += l.len();
        recent.push(l);
    }
    recent.reverse();
    let first = blocks.iter().find(|b| is_owner(&b.payload)).and_then(|b| line(&b.payload)).unwrap_or_default();
    let mut out = String::new();
    if !first.is_empty() && recent.first() != Some(&first) {
        out.push_str(&format!("The session began with:\n{first}\n\n[…]\n\n"));
    }
    out.push_str("The end of the conversation:\n");
    out.push_str(&recent.join("\n\n"));
    out.push_str(if want_title { "\n\nGive the title and next." } else { "\n\nGive next only (title \"\")." });
    out
}

/// The last `max` characters of `s`: an answer's end is where it hands over to the owner.
fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    format!("[…] {}", s.chars().skip(n - max).collect::<String>())
}

/// `(title, next)` from the model's reply, cleaned: one line each, no wrapping quotes, the title
/// at most 40 characters and the suggestion at most 500.
fn parse(reply: &str) -> Option<(String, String)> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let v: Value = serde_json::from_str(reply.get(start..=end)?).ok()?;
    let clean = |s: &str, max: usize| -> String {
        let s = s.trim().trim_matches(['"', '\'', '`', '“', '”']).trim();
        s.chars().take(max).collect::<String>().trim().to_string()
    };
    let title = clean(&v["title"].as_str().unwrap_or("").replace('\n', " "), 40).trim_end_matches('.').to_string();
    let next = clean(v["next"].as_str().unwrap_or(""), 500);
    Some((title, next))
}

/// The owner sent a prompt: settle the session's open suggestion. `taken` is what the client
/// reported for suggestion `reported` (Tab pressed on it); a suggestion the client didn't report
/// on was never shown (`unseen`).
pub(crate) async fn settle(db: &PgPool, id: Uuid, text: &str, reported: Option<(i64, bool)>) -> Result<(), sqlx::Error> {
    let open: Vec<(i64, String)> = sqlx::query_as("SELECT id, suggested FROM prompt_suggestions WHERE session_id = $1 AND outcome IS NULL")
        .bind(id)
        .fetch_all(db)
        .await?;
    for (sid, suggested) in open {
        let outcome = match reported {
            Some((r, taken)) if r == sid => outcome(&suggested, text, taken),
            _ => "unseen",
        };
        sqlx::query("UPDATE prompt_suggestions SET outcome = $2, final = $3, decided_at = now() WHERE id = $1")
            .bind(sid)
            .bind(outcome)
            .bind((outcome != "accepted").then_some(text))
            .execute(db)
            .await?;
    }
    Ok(())
}

fn outcome(suggested: &str, sent: &str, taken: bool) -> &'static str {
    match (taken, suggested.trim() == sent.trim()) {
        (true, true) => "accepted",
        (true, false) => "edited",
        (false, _) => "declined",
    }
}

/// `GET /api/suggestions`: outcomes by prompt version and model, and the latest suggestions.
pub(crate) async fn stats(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(
        "SELECT prompt_version, model, count(*) AS shown,
                count(*) FILTER (WHERE outcome = 'accepted') AS accepted,
                count(*) FILTER (WHERE outcome = 'edited') AS edited,
                count(*) FILTER (WHERE outcome = 'declined') AS declined,
                count(*) FILTER (WHERE outcome = 'unseen') AS unseen,
                count(*) FILTER (WHERE outcome IS NULL) AS open,
                avg(latency_ms)::float8 AS latency_ms, sum(cost_usd)::float8 AS cost_usd
         FROM prompt_suggestions GROUP BY 1, 2 ORDER BY min(created_at)",
    )
    .fetch_all(&app.db)
    .await?;
    let by_version: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "prompt_version": r.get::<String, _>("prompt_version"), "model": r.get::<String, _>("model"),
                "shown": r.get::<i64, _>("shown"), "accepted": r.get::<i64, _>("accepted"), "edited": r.get::<i64, _>("edited"),
                "declined": r.get::<i64, _>("declined"), "unseen": r.get::<i64, _>("unseen"), "open": r.get::<i64, _>("open"),
                "latency_ms": r.get::<Option<f64>, _>("latency_ms"), "cost_usd": r.get::<Option<f64>, _>("cost_usd"),
            })
        })
        .collect();
    let recent = sqlx::query(
        "SELECT id, session_id, suggested, final, outcome, prompt_version, created_at FROM prompt_suggestions ORDER BY id DESC LIMIT 20",
    )
    .fetch_all(&app.db)
    .await?;
    let recent: Vec<Value> = recent
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<i64, _>("id"), "session_id": r.get::<Uuid, _>("session_id"), "suggested": r.get::<String, _>("suggested"),
                "final": r.get::<Option<String>, _>("final"), "outcome": r.get::<Option<String>, _>("outcome"),
                "prompt_version": r.get::<String, _>("prompt_version"), "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "by_version": by_version, "recent": recent })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(seq: i32, payload: Value) -> tape::Block {
        tape::Block { seq, kind: "message".into(), payload }
    }

    #[test]
    fn parses_and_cleans_the_reply() {
        let (t, n) = parse("Sure:\n```json\n{\"title\": \"\\\"Fix the login test.\\\"\", \"next\": \" Run it again \"}\n```").unwrap();
        assert_eq!((t.as_str(), n.as_str()), ("Fix the login test", "Run it again"));
        let (t, _) = parse(&format!("{{\"title\": \"{}\", \"next\": \"\"}}", "x".repeat(80))).unwrap();
        assert_eq!(t.chars().count(), 40);
        assert!(parse("no json here").is_none());
    }

    #[test]
    fn outcomes() {
        assert_eq!(outcome("Run the tests", " Run the tests ", true), "accepted");
        assert_eq!(outcome("Run the tests", "Run the tests twice", true), "edited");
        assert_eq!(outcome("Run the tests", "Run the tests", false), "declined");
    }

    #[test]
    fn input_keeps_the_first_prompt_and_the_latest_answers_without_tools() {
        let mut blocks = vec![block(1, json!({ "role": "user", "content": "Build feature X" }))];
        for i in 0..20 {
            blocks.push(block(2 + i * 3, json!({ "role": "assistant", "content": [{ "type": "text", "text": format!("answer {i} {}", "y".repeat(1500)) }, { "type": "toolCall", "name": "bash", "arguments": {} }] })));
            blocks.push(block(3 + i * 3, json!({ "role": "toolResult", "toolName": "bash", "content": "SECRET TOOL OUTPUT" })));
            blocks.push(block(4 + i * 3, json!({ "role": "user", "content": format!("prompt {i}"), "kernel": i == 19 })));
        }
        let s = input(&blocks, true);
        assert!(s.starts_with("The session began with:\nUser: Build feature X"));
        assert!(s.contains("answer 19") && !s.contains("answer 2 ") && !s.contains("SECRET") && !s.contains("prompt 19"));
        assert!(s.len() < 16_000 && s.ends_with("Give the title and next."));
    }
}
