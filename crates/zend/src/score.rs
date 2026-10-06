//! Live scoring: after a session's work is judged (`/done`) or has gone quiet, a System One model
//! answers fixed questions about it, and the answers are stored with the session
//! (`session_scores`). The owner's decisions are the ground truth these are later compared with.
//!
//! - Off unless ZEN_S1_MODEL names a classifier a worker lists (e.g. `openrouter/typesafe/jev-1.13`,
//!   served by the Pi worker with OPENROUTER_API_KEY).
//! - Only the user's messages and the agent's final answer per turn are sent, never tool output.
//! - Only sessions with traced turns are scored, and each turn once per trigger kind (a failed
//!   score is retried after an hour).
//! - Questions are versioned (QUESTIONS_VERSION): change the set, bump the version.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::App;
use zen_proto::text_of;

pub const QUESTIONS_VERSION: &str = "v1";
/// Most text sent per session; older turns are dropped first (a long state also lowers accuracy).
const MAX_STATE_CHARS: usize = 24_000;
const MAX_MESSAGE_CHARS: usize = 3_000;


/// The classifier to score with, if scoring is on.
pub fn scorer() -> Option<String> {
    std::env::var("ZEN_S1_MODEL").ok().map(|m| m.trim().to_string()).filter(|m| !m.is_empty())
}

/// The v1 questions. Each is atomic; a choice has an `unknown` option because the model can't abstain.
pub fn questions() -> Value {
    json!({
        "work": {
            "type": "choice",
            "instructions": "What kind of work did the user ask the agent to do in this conversation?",
            "criteria": {
                "understand": "Find out or explain something: research, reading code or data, answering a question",
                "shape": "Frame a problem or a plan: scope, requirements, a brief, options to choose from",
                "bet": "Decide whether to commit time or money to something",
                "build": "Make or change something: code, documents, configuration",
                "verify": "Check that something works or is correct: tests, reviews, audits",
                "maintain": "Keep something running: fix breakage, upgrade, clean up, operate",
                "reflect": "Look back: what worked, lessons learned, retrospectives",
                "reach": "Communicate outward: write to or for other people, publish, market",
                "unknown": "None of these, or not enough to tell"
            }
        },
        "corrected": {
            "type": "bool",
            "instructions": "Did the user have to correct the agent, or repeat or rephrase a request because the agent got it wrong?",
            "criteria": {
                "true": "The user pointed out a mistake, said the agent misunderstood, or asked again for the same thing",
                "false": "The user only gave new requests or follow-ups"
            }
        },
        "unverified_claim": {
            "type": "bool",
            "instructions": "Does the agent claim that something is done, fixed or working without saying how it checked?",
            "criteria": {
                "true": "It claims success with no mention of a test, a run, output or another check",
                "false": "It says how it checked, or makes no such claim"
            }
        },
        "outcome": {
            "type": "score",
            "instructions": "How far did the agent get with what the user asked, judging by the conversation?",
            "criteria": [
                "Not done: the request was not carried out, or the result is wrong",
                "Partly done: some of it is missing or unresolved",
                "Done, but the user had to step in or fix things",
                "Done cleanly: the user got what they asked for without stepping in"
            ]
        },
        "report": {
            "type": "score",
            "instructions": "How well do the agent's answers serve the user? Good answers are short and direct, and put what needs the user's decision or attention first.",
            "criteria": [
                "Hard to use: long, vague, or burying what matters",
                "Usable, but padded or disorganized",
                "Clear and concise",
                "Clear, concise, and leads with what needs the user"
            ]
        }
    })
}


/// What the scorer sees: each user message and the agent's last text before the next one (its
/// final answer for that turn). Tool calls and their output are left out. The newest turns are kept
/// when the conversation is too long.
pub fn state(messages: &[Value]) -> Value {
    let mut turns: Vec<(String, String)> = Vec::new();
    for m in messages {
        match m["role"].as_str() {
            Some("user") => turns.push((text_of(&m["content"]), String::new())),
            Some("assistant") => {
                let t = text_of(&m["content"]);
                if let (Some(last), false) = (turns.last_mut(), t.trim().is_empty()) {
                    last.1 = t;
                }
            }
            _ => {}
        }
    }
    let mut kept: Vec<Value> = Vec::new();
    let mut size = 0;
    let total = turns.len();
    for (user, answer) in turns.into_iter().rev() {
        let (user, answer) = (zen_proto::head(&user, MAX_MESSAGE_CHARS), zen_proto::head(&answer, MAX_MESSAGE_CHARS));
        size += user.len() + answer.len();
        if size > MAX_STATE_CHARS && !kept.is_empty() {
            break;
        }
        kept.push(json!({ "user": user, "agent": answer }));
    }
    kept.reverse();
    let mut s = json!({ "conversation": kept });
    if kept.len() < total {
        s["earlier_turns_left_out"] = json!(total - kept.len());
    }
    s
}

/// A turn counts as scored for a trigger once a score with this question set succeeded, or failed
/// within the last hour (failures are retried after that, e.g. once a missing key is added).
async fn already_scored(app: &App, turn: Uuid, trigger: &str) -> Result<bool> {
    let found = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM session_scores WHERE turn_id = $1 AND trigger = $2 AND questions = $3
                        AND (error IS NULL OR created_at > now() - interval '1 hour'))",
    )
    .bind(turn)
    .bind(trigger)
    .bind(QUESTIONS_VERSION)
    .fetch_one(&app.db)
    .await?;
    Ok(found)
}

/// Score a session's work up to its latest traced turn, unless that turn was already scored for
/// this trigger. Records the result (or the error) in `session_scores`.
pub async fn score_session(app: &App, session: Uuid, trigger: &str) -> Result<()> {
    let Some(model) = scorer() else { return Ok(()) };
    let latest: Option<Uuid> = sqlx::query_scalar("SELECT id FROM turns WHERE session_id = $1 ORDER BY started_at DESC LIMIT 1")
        .bind(session)
        .fetch_optional(&app.db)
        .await?;
    let Some(turn_id) = latest else { return Ok(()) };
    if already_scored(app, turn_id, trigger).await? {
        return Ok(());
    }
    let messages = crate::load_messages(&app.db, session).await?;
    let questions = questions();
    let res = match crate::worker_for(app, &model).await {
        Some(w) => app.workers[w].mind().request("s1.decide", json!({ "model": model, "state": state(&messages), "questions": questions })).await,
        None => Err(anyhow::anyhow!("no worker serves classifier `{model}` (is the pi worker running?)")),
    };
    let (resolved, answers, usage, error) = match res {
        Ok(r) => (r["model"].as_str().map(String::from), r["answers"].clone(), r["usage"].clone(), r["error"].as_str().map(String::from)),
        Err(e) => (None, Value::Null, Value::Null, Some(e.to_string())),
    };
    if let Some(e) = &error {
        tracing::warn!("scoring session {session}: {e}");
    }
    let has = |v: &Value| !v.is_null() && v.as_object().is_none_or(|o| !o.is_empty());
    sqlx::query(
        "INSERT INTO session_scores (session_id, turn_id, scorer, scorer_resolved, questions, trigger, answers, usage, cost_usd, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(session)
    .bind(turn_id)
    .bind(&model)
    .bind(resolved)
    .bind(QUESTIONS_VERSION)
    .bind(trigger)
    .bind(has(&answers).then_some(&answers))
    .bind(has(&usage).then_some(&usage))
    .bind(usage["cost"]["total"].as_f64())
    .bind(error)
    .execute(&app.db)
    .await
    .context("recording the score")?;
    Ok(())
}

/// Score sessions that have gone quiet: their latest turn ended more than ZEN_SCORE_IDLE_SECS
/// (default 2 hours) ago and hasn't been scored. A few per round, so turning scoring on doesn't
/// send a burst.
pub async fn idle_loop(app: std::sync::Arc<App>) {
    let Some(model) = scorer() else { return };
    let idle = crate::env_num("ZEN_SCORE_IDLE_SECS", 7200.0) as i64;
    tracing::info!("scoring sessions with {model} once idle for {idle}s");
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        // Each session's latest turn, quiet long enough and not scored yet (a failed score is
        // retried after an hour); a few per round.
        let due: Vec<Uuid> = sqlx::query_scalar(
            // One index lookup per session (turns_session), not a sort of every turn.
            "SELECT s.id AS session_id FROM sessions s
             CROSS JOIN LATERAL (
                 SELECT t.id, t.ended_at FROM turns t
                 WHERE t.session_id = s.id AND t.ended_at IS NOT NULL AND t.model NOT LIKE 'faux/%'
                 ORDER BY t.started_at DESC LIMIT 1) l
             WHERE l.ended_at < now() - make_interval(secs => $1)
               AND NOT EXISTS (SELECT 1 FROM session_scores s WHERE s.turn_id = l.id AND s.trigger = 'idle' AND s.questions = $2
                               AND (s.error IS NULL OR s.created_at > now() - interval '1 hour'))
             LIMIT 5",
        )
        .bind(idle as f64)
        .bind(QUESTIONS_VERSION)
        .fetch_all(&app.db)
        .await
        .unwrap_or_default();
        for session in due {
            if app.is_busy(session).await {
                continue;
            }
            if let Err(e) = score_session(&app, session, "idle").await {
                tracing::warn!("scoring idle session {session}: {e:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_keeps_prompts_and_final_answers_only() {
        let msgs = vec![
            json!({ "role": "user", "content": "fix the build" }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "Looking." }, { "type": "toolCall", "name": "bash", "arguments": { "command": "cargo build" } }] }),
            json!({ "role": "toolResult", "content": [{ "type": "text", "text": "SECRET OUTPUT" }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "Fixed: the test passes now." }] }),
            json!({ "role": "user", "content": "thanks" }),
        ];
        let s = state(&msgs);
        assert_eq!(s["conversation"], json!([{ "user": "fix the build", "agent": "Fixed: the test passes now." }, { "user": "thanks", "agent": "" }]));
        assert!(!s.to_string().contains("SECRET"), "tool output never leaves the VM");
        assert!(s.get("earlier_turns_left_out").is_none());
    }

    #[test]
    fn long_conversations_keep_the_newest_turns() {
        let msgs: Vec<Value> = (0..40)
            .flat_map(|i| [json!({ "role": "user", "content": format!("q{i} {}", "x".repeat(1500)) }), json!({ "role": "assistant", "content": [{ "type": "text", "text": "a" }] })])
            .collect();
        let s = state(&msgs);
        let kept = s["conversation"].as_array().unwrap();
        assert!(s.to_string().len() < MAX_STATE_CHARS + 2_000);
        assert!(kept.last().unwrap()["user"].as_str().unwrap().starts_with("q39"));
        assert_eq!(s["earlier_turns_left_out"].as_u64().unwrap() as usize, 40 - kept.len());
    }

    #[test]
    fn questions_are_atomic_and_typed() {
        let q = questions();
        for (name, q) in q.as_object().unwrap() {
            assert!(["choice", "score", "bool"].contains(&q["type"].as_str().unwrap()), "{name}");
            assert!(!q["instructions"].as_str().unwrap().is_empty(), "{name}");
        }
        assert!(q["work"]["criteria"].get("unknown").is_some(), "a choice needs a way to abstain");
    }
}
