//! Per-turn measurement of the context (docs/context.md): what was sent, and whether the request
//! could reuse the previous turn's prompt cache. Plain code, no model.
//!
//! `cache_break` names why the cache could not be reused:
//! - expected: `first` (a session's first turn), `instructions` (new instructions or tools),
//!   `summary` (a summary replaced older turns), `model` (another model), `engine_session` (the
//!   engine's own session was restarted, e.g. after an interrupted turn), `expired` (a pause longer
//!   than the cache lives);
//! - unexpected: `history` (the history did not extend the previous turn's: a bug), `miss` (nothing
//!   explains it, but the provider read less than half of the previous context from its cache).

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::compile::SummaryRef;

/// The parts of the previous turn that decide whether this one can reuse its cache.
#[derive(Clone, Debug, Default)]
pub struct Previous {
    pub model: String,
    pub envelope: Option<String>,
    pub summary_seq: Option<i64>,
    pub history_from: Option<i64>,
    pub idle_secs: Option<i64>,
    pub context_tokens: Option<i64>,
}

pub async fn previous(db: &PgPool, session: Uuid) -> Result<Option<Previous>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT model, envelope, context, context_tokens, EXTRACT(EPOCH FROM now() - ended_at)::bigint AS idle
         FROM turns WHERE session_id = $1 ORDER BY started_at DESC LIMIT 1",
    )
    .bind(session)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| {
        let c: Option<Value> = r.get("context");
        let c = c.unwrap_or(Value::Null);
        Previous {
            model: r.get("model"),
            envelope: r.get("envelope"),
            summary_seq: c["summary"]["seq"].as_i64(),
            history_from: c["history"]["from"].as_i64(),
            idle_secs: r.get("idle"),
            context_tokens: r.get("context_tokens"),
        }
    }))
}

/// How long an engine's prompt cache lives: Claude Code asks for one hour, others five minutes.
pub fn cache_ttl_secs(model: &str) -> i64 {
    if model.starts_with("claude/") {
        3600
    } else {
        300
    }
}

/// The record of what a turn sends.
pub fn record(system: &str, tools: &Value, history: &[Value], summary: &Option<SummaryRef>, prompt: &str, turn_context: &Option<String>, resume: bool, new_envelope: Option<&str>) -> Value {
    let messages: Vec<&Value> = history.iter().filter(|m| m["summary"] != true).collect();
    let bytes = |v: &Value| v.to_string().len();
    let history_bytes: usize = history.iter().map(bytes).sum();
    let tools_bytes = bytes(tools);
    let footer = turn_context.as_deref().map(str::len).unwrap_or(0);
    let total = system.len() + tools_bytes + history_bytes + prompt.len() + footer;
    json!({
        "system_bytes": system.len(),
        "tools_bytes": tools_bytes,
        "summary": summary.as_ref().map(|s| json!({ "seq": s.seq, "from": s.from, "to": s.to })),
        "history": {
            "from": messages.first().and_then(|m| m["seq"].as_i64()),
            "to": messages.last().and_then(|m| m["seq"].as_i64()),
            "messages": messages.len(),
            "bytes": history_bytes,
        },
        "prompt_bytes": prompt.len(),
        "turn_context_bytes": footer,
        "est_tokens": total / 4,
        "new_envelope": new_envelope,
        "resume_offered": resume,
    })
}

/// Why this turn can't reuse the previous turn's cache, judged before it runs; None if it should.
pub fn break_at_start(prev: Option<&Previous>, model: &str, envelope: &str, record: &Value) -> Option<&'static str> {
    let Some(p) = prev else { return Some("first") };
    if p.envelope.as_deref() != Some(envelope) {
        return Some("instructions");
    }
    if p.summary_seq != record["summary"]["seq"].as_i64() {
        return Some("summary");
    }
    if p.model != model {
        return Some("model");
    }
    if p.history_from.is_some() && p.history_from != record["history"]["from"].as_i64() {
        return Some("history");
    }
    if p.idle_secs.is_some_and(|s| s > cache_ttl_secs(model)) {
        return Some("expired");
    }
    None
}

/// The final verdict once the turn ran: the engine may have had to restart its own session, and the
/// provider's numbers show whether the cache was actually reused.
pub fn break_at_end(at_start: Option<&str>, render: Option<&str>, resume_offered: bool, first_cache_read: Option<i64>, prev_context: Option<i64>) -> Option<String> {
    if let Some(b) = at_start {
        return Some(b.to_string());
    }
    if render == Some("seed") || (resume_offered && render == Some("inject")) {
        return Some("engine_session".into());
    }
    match (first_cache_read, prev_context) {
        (Some(read), Some(prev)) if prev > 0 && read * 2 < prev => Some("miss".into()),
        _ => None,
    }
}

/// The provider's numbers for a turn: what its first model call read from the cache, and the
/// context size of its last call (input including cached tokens). None where an engine reports none.
pub async fn provider_numbers(db: &PgPool, turn: Uuid) -> Result<(Option<i64>, Option<i64>), sqlx::Error> {
    let rows = sqlx::query("SELECT cache_read, input_tokens + cache_read + cache_write AS ctx FROM model_calls WHERE turn_id = $1 ORDER BY id")
        .bind(turn)
        .fetch_all(db)
        .await?;
    let first = rows.first().map(|r| r.get::<i64, _>("cache_read"));
    let last = rows.last().map(|r| r.get::<i64, _>("ctx")).filter(|c| *c > 0);
    Ok((first.filter(|_| last.is_some()), last))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(from: i64, summary: Option<i64>) -> Value {
        json!({ "history": { "from": from }, "summary": summary.map(|s| json!({ "seq": s })) })
    }

    #[test]
    fn breaks_are_explained() {
        let p = Previous { model: "claude/x".into(), envelope: Some("e1".into()), history_from: Some(1), idle_secs: Some(10), ..Default::default() };
        assert_eq!(break_at_start(None, "claude/x", "e1", &rec(1, None)), Some("first"));
        assert_eq!(break_at_start(Some(&p), "claude/x", "e1", &rec(1, None)), None);
        assert_eq!(break_at_start(Some(&p), "claude/x", "e2", &rec(1, None)), Some("instructions"));
        assert_eq!(break_at_start(Some(&p), "claude/x", "e1", &rec(61, Some(80))), Some("summary"));
        assert_eq!(break_at_start(Some(&p), "claude/y", "e1", &rec(1, None)), Some("model"));
        assert_eq!(break_at_start(Some(&p), "claude/x", "e1", &rec(5, None)), Some("history"), "history rewritten without a summary");
        let idle = Previous { idle_secs: Some(4000), ..p.clone() };
        assert_eq!(break_at_start(Some(&idle), "claude/x", "e1", &rec(1, None)), Some("expired"));
        assert_eq!(break_at_start(Some(&Previous { idle_secs: Some(400), ..p }), "codex/y", "e1", &rec(1, None)), Some("model"));
    }

    #[test]
    fn end_verdict_uses_the_engine_and_the_provider() {
        assert_eq!(break_at_end(Some("summary"), Some("seed"), false, Some(0), Some(9000)).as_deref(), Some("summary"));
        assert_eq!(break_at_end(None, Some("seed"), true, Some(0), Some(9000)).as_deref(), Some("engine_session"));
        assert_eq!(break_at_end(None, Some("resume"), true, Some(8800), Some(9000)), None);
        assert_eq!(break_at_end(None, Some("resume"), true, Some(100), Some(9000)).as_deref(), Some("miss"));
        assert_eq!(break_at_end(None, Some("native"), false, None, Some(9000)), None, "no provider numbers: no verdict");
    }
}
