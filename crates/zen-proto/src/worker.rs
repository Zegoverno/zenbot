//! The worker protocol's messages (docs/worker-protocol.md), typed, so the kernel and every worker
//! build and read them by one definition. JSON-RPC 2.0, one object per line: `method` names the
//! message and `params` carries the fields below.
//!
//! Reading is lenient, as a protocol between versions must be: unknown fields are ignored, and a
//! missing or `null` field takes its default. Messages (history, `turn.message`) stay JSON in Pi's
//! format, and a usage report is an open set of fields (`Usage`). Senders turn a message into a
//! `serde_json::Value` before writing it, so its keys go out in the same (sorted) order as before.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::or_default;

/// A request from the kernel to a worker, by method.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    /// Is the worker alive? Answered with `Pong`.
    #[serde(rename = "ping")]
    Ping {},
    /// Which models does it serve? Answered with `ModelsList`.
    #[serde(rename = "models.list")]
    ModelsList {},
    /// Run a turn. Answered with `Ack` at once; the turn then reports with `TurnEvent`s.
    #[serde(rename = "turn.start")]
    TurnStart(TurnStart),
    /// Stop a turn (or every turn of a session). Answered with `Ack`.
    #[serde(rename = "turn.abort")]
    TurnAbort(TurnAbort),
    /// One completion without tools. Answered with a `Completion` or `CompleteError`.
    #[serde(rename = "complete")]
    Complete(Complete),
}

impl Request {
    /// A request from its method and params (missing or `null` params read as `{}`).
    pub fn parse(method: &str, params: Value) -> serde_json::Result<Request> {
        parse_tagged(method, params)
    }

    /// The JSON-RPC method.
    pub fn method(&self) -> &'static str {
        match self {
            Request::Ping {} => "ping",
            Request::ModelsList {} => "models.list",
            Request::TurnStart(_) => "turn.start",
            Request::TurnAbort(_) => "turn.abort",
            Request::Complete(_) => "complete",
        }
    }

    /// The JSON-RPC params.
    pub fn params(&self) -> Value {
        params_of(self)
    }
}

/// `turn.start`: everything the worker needs for one turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnStart {
    #[serde(deserialize_with = "or_default")]
    pub session_id: String,
    /// The kernel's id for the turn, sent back on every message of the turn. None from a kernel
    /// older than turn ids.
    pub turn_id: Option<String>,
    /// The full model id, `<engine>/<model>`.
    #[serde(deserialize_with = "or_default")]
    pub model: String,
    /// The thinking level; None only for a model without levels.
    pub effort: Option<String>,
    #[serde(deserialize_with = "or_default")]
    pub system_prompt: String,
    /// The session so far, as messages in Pi's format (each with its `seq`).
    #[serde(deserialize_with = "or_default")]
    pub history: Vec<Value>,
    #[serde(deserialize_with = "or_default")]
    pub prompt: String,
    /// This turn's context (the date when it changed, …), sent after the prompt.
    pub prompt_context: Option<String>,
    /// The tools the model may use: `{ name, description, parameters }` (JSON Schema).
    #[serde(deserialize_with = "or_default")]
    pub tools: Vec<Value>,
    /// The worker's own engine session the kernel considers in sync with the tape.
    pub resume: Option<Resume>,
    /// The kind of session (`verifier`, `subagent`, `job`); None for the owner's sessions.
    pub kind: Option<String>,
}

/// An engine session to continue (`turn.start`'s `resume`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    #[serde(default, deserialize_with = "or_default")]
    pub id: String,
}

/// `turn.abort`: stop the turn named, or every turn of the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnAbort {
    #[serde(deserialize_with = "or_default")]
    pub session_id: String,
    pub turn_id: Option<String>,
}

/// `complete`: one completion without tools.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Complete {
    #[serde(deserialize_with = "or_default")]
    pub model: String,
    #[serde(deserialize_with = "or_default")]
    pub system: String,
    #[serde(deserialize_with = "or_default")]
    pub prompt: String,
}

/// The answer to `ping`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    #[serde(default, deserialize_with = "or_default")]
    pub pong: bool,
}

/// The answer to `turn.start` and `turn.abort`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ack {
    #[serde(default, deserialize_with = "or_default")]
    pub ok: bool,
}

/// The answer to `models.list`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsList {
    /// Per engine, whether it is installed and answers (the sign-in is checked when a turn runs).
    #[serde(deserialize_with = "or_default")]
    pub authenticated: Map<String, Value>,
    #[serde(deserialize_with = "or_default")]
    pub models: Vec<Model>,
}

/// A model a worker serves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Model {
    /// The full id, `<engine>/<model>`.
    #[serde(deserialize_with = "or_default")]
    pub id: String,
    #[serde(deserialize_with = "or_default")]
    pub name: String,
    #[serde(deserialize_with = "or_default")]
    pub engine: String,
    /// The thinking levels it accepts, in order; empty when it has none to choose.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "or_default")]
    pub efforts: Vec<String>,
    /// The level used when a session picks none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    /// The context window in tokens, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    /// Anything else the worker says about it (Codex's `default`), passed on as is.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The answer to `complete` when it worked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Completion {
    #[serde(deserialize_with = "or_default")]
    pub text: String,
    /// The tokens it used: `{ input, output, … }`.
    pub usage: Value,
    /// The model that answered.
    #[serde(deserialize_with = "or_default")]
    pub model: String,
}

/// The answer to `complete` when it failed (an answer, not a JSON-RPC error).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompleteError {
    #[serde(default, deserialize_with = "or_default")]
    pub error: String,
}

/// `tool.call`: a request from the worker; the kernel runs the tool and answers `ToolResult`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolCall {
    #[serde(deserialize_with = "or_default")]
    pub session_id: String,
    pub turn_id: Option<String>,
    #[serde(deserialize_with = "or_default")]
    pub call_id: String,
    #[serde(deserialize_with = "or_default")]
    pub name: String,
    /// The tool's arguments (`{}` when missing).
    #[serde(deserialize_with = "args_or_empty")]
    pub args: Value,
}

impl ToolCall {
    pub const METHOD: &'static str = "tool.call";
}

/// The answer to `tool.call`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolResult {
    #[serde(deserialize_with = "or_default")]
    pub content: String,
    #[serde(deserialize_with = "or_default")]
    pub is_error: bool,
}

/// A notification a turn sends the kernel, by method. On the wire its params also carry the
/// session and turn ids (`TurnRef`), which `to_params` adds and `parse` leaves out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum TurnEvent {
    /// Streamed answer text.
    #[serde(rename = "turn.delta")]
    Delta {
        #[serde(default, deserialize_with = "or_default")]
        delta: String,
    },
    /// Streamed reasoning.
    #[serde(rename = "turn.thinking")]
    Thinking {
        #[serde(default, deserialize_with = "or_default")]
        delta: String,
    },
    /// A finished message (Pi's format), appended to the session's tape.
    #[serde(rename = "turn.message")]
    Message {
        #[serde(default)]
        message: Value,
    },
    /// The worker's report for the whole turn, before `turn.end`.
    #[serde(rename = "turn.usage")]
    Usage(Usage),
    /// A hard subscription limit made the turn continue on another provider.
    #[serde(rename = "turn.fallback")]
    Fallback {
        #[serde(default, deserialize_with = "or_default")]
        from: String,
        #[serde(default, deserialize_with = "or_default")]
        to: String,
        #[serde(default, deserialize_with = "or_default")]
        reason: String,
    },
    /// The turn is over: `error` is None on success, `"interrupted"` after an abort.
    #[serde(rename = "turn.end")]
    End {
        #[serde(default)]
        error: Option<String>,
    },
}

/// A turn's usage report: `engine`, `engine_version`, `provider`, `model`, the totals (`input`,
/// `output`, `cache_read`, `cache_write`, `cost_usd`), `render`, `engine_session` (`{ id,
/// resumable }`), `fallback_reason`, and after a failover `provider_reports`. Open: the kernel
/// keeps whatever the worker reports.
pub type Usage = Map<String, Value>;

impl TurnEvent {
    /// A notification from its method and params. Err for a method that isn't a turn's
    /// notification, or fields that don't fit.
    pub fn parse(method: &str, params: Value) -> serde_json::Result<TurnEvent> {
        parse_tagged(method, params)
    }

    /// The JSON-RPC method.
    pub fn method(&self) -> &'static str {
        match self {
            TurnEvent::Delta { .. } => "turn.delta",
            TurnEvent::Thinking { .. } => "turn.thinking",
            TurnEvent::Message { .. } => "turn.message",
            TurnEvent::Usage(_) => "turn.usage",
            TurnEvent::Fallback { .. } => "turn.fallback",
            TurnEvent::End { .. } => "turn.end",
        }
    }

    /// The JSON-RPC params for the turn `ids` names.
    pub fn to_params(&self, ids: &TurnRef) -> Value {
        let mut params = params_of(self);
        if let Value::Object(p) = &mut params {
            p.insert("session_id".into(), Value::String(ids.session_id.clone()));
            p.insert("turn_id".into(), ids.turn_id.clone().map_or(Value::Null, Value::String));
        }
        params
    }
}

/// The ids every message of a turn carries: its session, and the kernel's id for the turn (None
/// from a worker older than turn ids).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnRef {
    #[serde(deserialize_with = "or_default")]
    pub session_id: String,
    pub turn_id: Option<String>,
}

impl TurnRef {
    /// The ids in a message's params (empty when it has none).
    pub fn of(params: &Value) -> TurnRef {
        TurnRef::deserialize(params).unwrap_or_default()
    }
}

/// Read `{ method, params }` as an adjacently tagged enum; params missing or `null` are `{}`.
fn parse_tagged<T: serde::de::DeserializeOwned>(method: &str, params: Value) -> serde_json::Result<T> {
    let params = if params.is_null() { Value::Object(Map::new()) } else { params };
    let mut msg = Map::new();
    msg.insert("method".into(), Value::String(method.into()));
    msg.insert("params".into(), params);
    T::deserialize(Value::Object(msg))
}

/// The `params` of an adjacently tagged message (`{}` when it has none).
fn params_of<T: Serialize>(msg: &T) -> Value {
    match serde_json::to_value(msg) {
        Ok(Value::Object(mut m)) => m.remove("params").unwrap_or_else(|| Value::Object(Map::new())),
        _ => Value::Object(Map::new()),
    }
}

/// Tool arguments: missing or `null` are `{}`.
fn args_or_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
    Ok(Option::<Value>::deserialize(d)?.filter(|v| !v.is_null()).unwrap_or_else(|| Value::Object(Map::new())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Each message read from a sample shaped as docs/worker-protocol.md shows it, and written back
    /// to the same JSON.
    fn round_trip_request(sample: Value) -> Request {
        let req = Request::parse(sample["method"].as_str().unwrap(), sample["params"].clone()).unwrap();
        assert_eq!(req.method(), sample["method"]);
        assert_eq!(req.params(), sample["params"], "{}", req.method());
        req
    }

    fn round_trip_event(sample: Value) -> TurnEvent {
        let params = sample["params"].clone();
        let ev = TurnEvent::parse(sample["method"].as_str().unwrap(), params.clone()).unwrap();
        assert_eq!(ev.method(), sample["method"]);
        assert_eq!(ev.to_params(&TurnRef::of(&params)), params, "{}", ev.method());
        ev
    }

    #[test]
    fn kernel_requests_round_trip() {
        assert_eq!(round_trip_request(json!({ "method": "ping", "params": {} })), Request::Ping {});
        assert_eq!(round_trip_request(json!({ "method": "models.list", "params": {} })), Request::ModelsList {});
        let start = round_trip_request(json!({ "method": "turn.start", "params": {
            "session_id": "s1", "turn_id": "t1", "model": "claude/claude-opus-5-5", "effort": "medium",
            "system_prompt": "You are zen.", "history": [{ "role": "user", "content": "hi", "seq": 1 }],
            "prompt": "fix it", "prompt_context": "<turn_context>\nToday is X.\n</turn_context>",
            "tools": [{ "name": "bash", "description": "Run", "parameters": { "type": "object" } }],
            "resume": { "id": "e1" }, "kind": null
        } }));
        let Request::TurnStart(start) = start else { panic!() };
        assert_eq!((start.resume.unwrap().id.as_str(), start.kind), ("e1", None));
        round_trip_request(json!({ "method": "turn.start", "params": {
            "session_id": "s1", "turn_id": "t1", "model": "faux/smoke", "effort": null, "system_prompt": "", "history": [],
            "prompt": "p", "prompt_context": null, "tools": [], "resume": null, "kind": "verifier"
        } }));
        round_trip_request(json!({ "method": "turn.abort", "params": { "session_id": "s1", "turn_id": "t1" } }));
        round_trip_request(json!({ "method": "complete", "params": { "model": "codex/gpt-6.1-sol", "system": "Summarize.", "prompt": "…" } }));
    }

    #[test]
    fn turn_notifications_round_trip() {
        let ids = json!({ "session_id": "s1", "turn_id": "t1" });
        let with = |method: &str, mut params: Value| {
            params.as_object_mut().unwrap().extend(ids.as_object().unwrap().clone());
            json!({ "method": method, "params": params })
        };
        round_trip_event(with("turn.delta", json!({ "delta": "Hel" })));
        round_trip_event(with("turn.thinking", json!({ "delta": "hmm" })));
        round_trip_event(with("turn.message", json!({ "message": { "role": "assistant", "provider": "claude", "model": "claude-opus-5",
            "stopReason": "toolUse", "content": [{ "type": "toolCall", "id": "call-1", "name": "bash", "arguments": { "command": "ls" } }],
            "usage": { "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0, "cost": { "total": 0.0 } }, "timestamp": 0 } })));
        let usage = round_trip_event(with("turn.usage", json!({ "engine": "claude-code", "engine_version": "2.1.287", "input": 10,
            "cost_usd": 0.01, "render": "resume", "engine_session": { "id": "e1", "resumable": true } })));
        let TurnEvent::Usage(u) = usage else { panic!() };
        assert_eq!(u["engine"], "claude-code");
        round_trip_event(with("turn.fallback", json!({ "from": "claude", "to": "codex/gpt-6.1-sol", "reason": "You've hit your weekly limit" })));
        assert_eq!(round_trip_event(with("turn.end", json!({ "error": null }))), TurnEvent::End { error: None });
        round_trip_event(with("turn.end", json!({ "error": "interrupted" })));
    }

    #[test]
    fn answers_and_tool_calls_round_trip() {
        let round_trip = |sample: Value, typed: Value| assert_eq!(typed, sample);
        let call = json!({ "session_id": "s1", "turn_id": "t1", "call_id": "call-1", "name": "bash", "args": { "command": "ls" } });
        round_trip(call.clone(), serde_json::to_value(ToolCall::deserialize(&call).unwrap()).unwrap());
        let result = json!({ "content": "a.txt", "is_error": false });
        round_trip(result.clone(), serde_json::to_value(ToolResult::deserialize(&result).unwrap()).unwrap());
        let pong = json!({ "pong": true });
        round_trip(pong.clone(), serde_json::to_value(Pong::deserialize(&pong).unwrap()).unwrap());
        let ok = json!({ "ok": true });
        round_trip(ok.clone(), serde_json::to_value(Ack::deserialize(&ok).unwrap()).unwrap());
        let models = json!({ "authenticated": { "claude": true, "codex": false }, "models": [
            { "id": "claude/claude-opus-5-5", "name": "Claude Opus 5.5", "engine": "claude-code", "efforts": ["low", "medium"], "default_effort": "medium" },
            { "id": "codex/gpt-6.1-sol", "name": "Codex GPT-6.1", "engine": "codex", "efforts": ["low"], "default_effort": "low", "context": 400000, "default": true },
            { "id": "faux/smoke", "name": "Test model (scripted)", "engine": "faux" }
        ] });
        round_trip(models.clone(), serde_json::to_value(ModelsList::deserialize(&models).unwrap()).unwrap());
        let done = json!({ "text": "{}", "usage": { "input": 3, "output": 4 }, "model": "faux/smoke" });
        round_trip(done.clone(), serde_json::to_value(Completion::deserialize(&done).unwrap()).unwrap());
        let failed = json!({ "error": "codex didn't complete within 540 s" });
        round_trip(failed.clone(), serde_json::to_value(CompleteError::deserialize(&failed).unwrap()).unwrap());
    }

    #[test]
    fn reading_is_lenient() {
        // Unknown fields are ignored; missing or null ones take their default; no params is `{}`.
        let Request::TurnStart(start) = Request::parse("turn.start", json!({ "session_id": "s1", "prompt": null, "future": 1 })).unwrap() else { panic!() };
        assert_eq!((start.session_id.as_str(), start.prompt.as_str(), start.turn_id, start.tools.len()), ("s1", "", None, 0));
        assert_eq!(Request::parse("ping", Value::Null).unwrap(), Request::Ping {});
        assert!(Request::parse("turn.explode", json!({})).is_err());
        let call = ToolCall::deserialize(&json!({ "session_id": "s1", "name": "read", "args": null })).unwrap();
        assert_eq!((call.args, call.turn_id), (json!({}), None));
        assert_eq!(TurnEvent::parse("turn.delta", json!({ "delta": null, "session_id": "s1" })).unwrap(), TurnEvent::Delta { delta: String::new() });
        assert!(TurnEvent::parse("tool.call", json!({})).is_err(), "not a notification");
        assert_eq!(TurnRef::of(&json!({ "session_id": "s1", "turn_id": "t1", "x": 2 })), TurnRef { session_id: "s1".into(), turn_id: Some("t1".into()) });
        assert_eq!(TurnRef::of(&Value::Null), TurnRef::default());
    }
}
