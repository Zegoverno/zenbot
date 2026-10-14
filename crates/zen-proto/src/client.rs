//! The client protocol's events (docs/client-protocol.md, "WebSocket"): what the kernel sends on a
//! session's stream, typed, so every client (`zen`, `zen-matrix`) reads them by one set of rules.
//! An event a client doesn't know, or one whose fields don't fit, is kept as `Other` (the raw JSON):
//! a client that predates an event ignores it.
//!
//! Messages stay JSON (`Value`) in Pi's format, as in the worker protocol (`worker`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{or_default, text_of};

/// One event on a session's stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A message added to the session (Pi's format, no `seq`). A user message with `kernel: true`
    /// is the kernel talking to the model, not the owner.
    Message { message: Value },
    /// A turn started (also one the kernel started itself).
    Busy {
        #[serde(default)]
        busy: bool,
        #[serde(default)]
        turn_id: Option<String>,
        #[serde(default)]
        harness: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
    },
    /// Streamed answer text.
    Delta {
        #[serde(default, deserialize_with = "or_default")]
        delta: String,
    },
    /// Streamed reasoning.
    Thinking {
        #[serde(default, deserialize_with = "or_default")]
        delta: String,
    },
    /// A tool call began (`args` with secrets masked, each string cut at 4 KB).
    ToolStart {
        #[serde(default, deserialize_with = "or_default")]
        call_id: String,
        #[serde(default, deserialize_with = "or_default")]
        name: String,
        #[serde(default)]
        args: Value,
    },
    /// A tool call finished.
    ToolEnd {
        #[serde(default, deserialize_with = "or_default")]
        call_id: String,
        #[serde(default, deserialize_with = "or_default")]
        is_error: bool,
        #[serde(default)]
        ms: Option<u64>,
    },
    /// The turn ended. `turn` is the kernel's record of it; `cost` the session's total (absent when
    /// the turn failed to start). With `next`, the kernel goes on with a turn of its own: wait for `idle`.
    End {
        #[serde(default)]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<f64>,
        #[serde(default)]
        turn: Value,
        #[serde(default, deserialize_with = "or_default")]
        next: bool,
    },
    /// A child session's turn ended (a verifier's); its cost counts with the turn.
    ChildEnd {
        #[serde(default)]
        turn: Value,
    },
    /// Reserved: the kernel's own turns are over (after `end` with `next`). Nothing sends it today.
    Idle {},
    /// The kernel named the session.
    Title {
        #[serde(default, deserialize_with = "or_default")]
        title: String,
    },
    /// The owner's likely next prompt, after a turn; its `id` goes back with the next prompt.
    Suggestion { id: i64, text: String },
    /// The session's model changed without the owner choosing it: a usage-limit failover moved it
    /// to another model, which it keeps until the owner picks one (D-057). `effort` is the
    /// session's thinking level on it (None: the model's default).
    Model {
        #[serde(default, deserialize_with = "or_default")]
        model: String,
        #[serde(default)]
        effort: Option<String>,
    },
    /// Progress the kernel reports (a long tool, a delegation, a summary, a failover).
    Status {
        #[serde(default, deserialize_with = "or_default")]
        text: String,
    },
    /// The `ask` tool's questions for the owner, recommended option first.
    Questions {
        #[serde(default)]
        questions: Vec<Question>,
    },
    /// The session was marked as having read untrusted content (`tainted`, with where it came
    /// from), or the owner lifted the mark (D-054).
    Taint {
        #[serde(default)]
        risky: bool,
        #[serde(default, deserialize_with = "or_default")]
        tainted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        about: Option<String>,
    },
    /// A request failed.
    Error {
        #[serde(default, deserialize_with = "or_default")]
        error: String,
    },
    /// The client fell behind and missed `skipped` events; `busy` says whether a turn still runs.
    Resync {
        #[serde(default)]
        skipped: u64,
        #[serde(default, deserialize_with = "or_default")]
        busy: bool,
    },
    /// Anything else: an event this version doesn't know, or one whose fields don't fit.
    #[serde(untagged)]
    Other(Value),
}

/// One of the `ask` tool's questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    #[serde(default, deserialize_with = "or_default")]
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

impl Event {
    /// An event from the stream's text: None when it isn't JSON at all.
    pub fn parse(text: &str) -> Option<Event> {
        serde_json::from_str::<Value>(text).ok().map(Event::from_value)
    }

    /// An event from its JSON; never fails (`Other` when it doesn't fit a known event).
    pub fn from_value(v: Value) -> Event {
        Event::deserialize(&v).unwrap_or(Event::Other(v))
    }

    /// The event's `type` (for `Other`, whatever its JSON says).
    pub fn kind(&self) -> &str {
        match self {
            Event::Other(v) => v["type"].as_str().unwrap_or(""),
            Event::Message { .. } => "message",
            Event::Busy { .. } => "busy",
            Event::Delta { .. } => "delta",
            Event::Thinking { .. } => "thinking",
            Event::ToolStart { .. } => "tool_start",
            Event::ToolEnd { .. } => "tool_end",
            Event::End { .. } => "end",
            Event::ChildEnd { .. } => "child_end",
            Event::Idle {} => "idle",
            Event::Title { .. } => "title",
            Event::Suggestion { .. } => "suggestion",
            Event::Model { .. } => "model",
            Event::Status { .. } => "status",
            Event::Questions { .. } => "questions",
            Event::Taint { .. } => "taint",
            Event::Error { .. } => "error",
            Event::Resync { .. } => "resync",
        }
    }

    /// After this event no turn runs in the session: an `end` the kernel doesn't follow with a
    /// turn of its own, `idle`, or a `resync` that found the session idle. Every client ends a
    /// turn by this rule.
    pub fn ends_turn(&self) -> bool {
        matches!(self, Event::End { next: false, .. } | Event::Idle {} | Event::Resync { busy: false, .. })
    }
}

/// Whether message `m` is the kernel's echo of a prompt this client sent: an owner's message
/// (not the kernel's) with the same text, leading and trailing whitespace aside.
pub fn echoes(m: &Value, prompt: &str) -> bool {
    m["role"] == "user" && m["kernel"] != true && text_of(&m["content"]).trim() == prompt.trim()
}

/// Prompts a client sent that the kernel hasn't echoed yet, so the echo isn't shown twice and a
/// prompt from another client (the terminal, Matrix) can be told apart.
#[derive(Debug, Default)]
pub struct Sent(Vec<String>);

impl Sent {
    pub fn push(&mut self, prompt: impl Into<String>) {
        self.0.push(prompt.into());
    }

    /// Whether `m` echoes one of the prompts sent (that prompt is then forgotten).
    pub fn take_echo(&mut self, m: &Value) -> bool {
        let Some(i) = self.0.iter().position(|p| echoes(m, p)) else { return false };
        self.0.remove(i);
        true
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_documented_event_parses() {
        let busy = Event::from_value(json!({ "type": "busy", "busy": true, "turn_id": "6d3c", "harness": "abc123", "model": "faux/smoke", "effort": null }));
        assert!(matches!(busy, Event::Busy { busy: true, model: Some(m), effort: None, .. } if m == "faux/smoke"));
        assert_eq!(Event::from_value(json!({ "type": "delta", "delta": "Hel" })), Event::Delta { delta: "Hel".into() });
        assert_eq!(Event::from_value(json!({ "type": "thinking", "delta": null })), Event::Thinking { delta: String::new() }, "a missing delta is empty");
        let start = Event::from_value(json!({ "type": "tool_start", "call_id": "c1", "name": "bash", "args": { "command": "ls" } }));
        assert!(matches!(&start, Event::ToolStart { name, args, .. } if name == "bash" && args["command"] == "ls"));
        assert_eq!(Event::from_value(json!({ "type": "tool_end", "call_id": "c1", "is_error": true, "ms": 12 })), Event::ToolEnd { call_id: "c1".into(), is_error: true, ms: Some(12) });
        let end = Event::from_value(json!({ "type": "end", "error": null, "cost": 0.5, "turn": { "input_tokens": 3 }, "next": false }));
        assert!(matches!(&end, Event::End { error: None, cost: Some(c), turn, next: false } if *c == 0.5 && turn["input_tokens"] == 3));
        assert!(matches!(Event::from_value(json!({ "type": "end", "error": "boom", "turn": null })), Event::End { error: Some(e), cost: None, .. } if e == "boom"));
        assert!(matches!(Event::from_value(json!({ "type": "child_end", "turn": {} })), Event::ChildEnd { .. }));
        assert_eq!(Event::from_value(json!({ "type": "idle" })), Event::Idle {});
        assert_eq!(Event::from_value(json!({ "type": "title", "title": "Taxes" })), Event::Title { title: "Taxes".into() });
        assert_eq!(Event::from_value(json!({ "type": "suggestion", "id": 4, "text": "go on" })), Event::Suggestion { id: 4, text: "go on".into() });
        assert_eq!(Event::from_value(json!({ "type": "status", "text": "verifying" })), Event::Status { text: "verifying".into() });
        assert_eq!(Event::from_value(json!({ "type": "model", "model": "codex/gpt-6.1-sol", "effort": null })), Event::Model { model: "codex/gpt-6.1-sol".into(), effort: None });
        let qs = Event::from_value(json!({ "type": "questions", "questions": [{ "question": "Which?", "options": ["a", "b"] }] }));
        assert_eq!(qs, Event::Questions { questions: vec![Question { question: "Which?".into(), options: vec!["a".into(), "b".into()] }] });
        assert_eq!(Event::from_value(json!({ "type": "error", "error": "session is busy" })), Event::Error { error: "session is busy".into() });
        assert_eq!(Event::from_value(json!({ "type": "resync", "skipped": 3, "busy": false })), Event::Resync { skipped: 3, busy: false });
        let m = Event::from_value(json!({ "type": "message", "message": { "role": "user", "content": "hi" } }));
        assert!(matches!(&m, Event::Message { message } if message["content"] == "hi"));
        assert_eq!(m.kind(), "message");
    }

    #[test]
    fn unknown_or_misshapen_events_are_kept_not_rejected() {
        let new = json!({ "type": "brief", "text": "x" });
        assert_eq!(Event::from_value(new.clone()), Event::Other(new));
        let bad = json!({ "type": "suggestion", "id": "not a number" });
        assert_eq!(Event::from_value(bad.clone()), Event::Other(bad));
        assert_eq!(Event::parse(r#"{"type":"disconnected"}"#).unwrap().kind(), "disconnected");
        assert_eq!(Event::parse("not json"), None);
    }

    #[test]
    fn the_wire_format_round_trips() {
        for v in [
            json!({ "type": "end", "error": null, "cost": 1.5, "turn": { "a": 1 }, "next": false }),
            json!({ "type": "tool_start", "call_id": "c", "name": "read", "args": { "path": "x" } }),
            json!({ "type": "resync", "skipped": 2, "busy": true }),
        ] {
            assert_eq!(serde_json::to_value(Event::from_value(v.clone())).unwrap(), v);
        }
    }

    #[test]
    fn the_turn_ends_by_one_rule() {
        let ends = |v: Value| Event::from_value(v).ends_turn();
        assert!(ends(json!({ "type": "end", "error": null })));
        assert!(!ends(json!({ "type": "end", "next": true })), "the kernel goes on");
        assert!(ends(json!({ "type": "idle" })));
        assert!(ends(json!({ "type": "resync", "skipped": 1, "busy": false })));
        assert!(!ends(json!({ "type": "resync", "skipped": 1, "busy": true })));
        assert!(!ends(json!({ "type": "child_end", "turn": {} })), "a verifier's end isn't the turn's");
    }

    #[test]
    fn echoes_are_matched_once_and_kernel_messages_never() {
        let mut sent = Sent::default();
        sent.push("  do it ");
        assert!(!sent.take_echo(&json!({ "role": "user", "kernel": true, "content": "do it" })), "the kernel's own prompt");
        assert!(!sent.take_echo(&json!({ "role": "assistant", "content": "do it" })));
        assert!(sent.take_echo(&json!({ "role": "user", "content": [{ "type": "text", "text": "do it" }] })));
        assert!(sent.is_empty());
        assert!(!sent.take_echo(&json!({ "role": "user", "content": "do it" })), "once");
    }
}
