//! What a turn sends the model (docs/context.md), in an order that keeps the provider's prompt cache:
//! tools and instructions (fixed for the session) → summary → history (append-only) → the prompt
//! with its turn context (what changes per turn, at the end).

use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::context;
use crate::tape::{self, Block};

/// The instructions and tools a session uses. Fixed for the session: an AGENTS.md edited mid-session
/// takes effect in the next one, and the date is in the turn context, not here.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub hash: String,
    pub system: String,
    pub tools: Value,
}

/// The system prompt: zenbot's rules and the owner's instruction files from `/` down to the workspace.
pub fn system_prompt(workspace: &Path, repo: &str) -> String {
    let mut s = format!(
        "You are zenbot, the owner's personal agent running on their Linux VM.\n\
         You can run shell commands and read, write, edit and move files using your tools.\n\
         Be concise and direct. Show file paths clearly. Prefer doing the work over describing it.\n\
         The user sees every tool call and its full output in the interface, so never repeat raw tool output; \
         summarize what matters and quote only the relevant lines.\n\
         Ask before destructive or outward-facing actions (deleting data, pushing, publishing, sending messages, spending money).\n\
         Your own source code (zenbot) is at {repo}. Before changing yourself, read {repo}/AGENTS.md and follow it; \
         never restart your own service directly, use the upgrade script it describes.\n\
         \n\
         <tool_guidelines>\n\
         - Use read to look at files (not cat or sed), and read a file before editing it.\n\
         - Use edit for changes to existing files and write for new files or complete rewrites. Edits to the same file are applied one at a time, so several in one step are safe.\n\
         - Use bash for searching (rg, grep, find), git, builds, tests and running programs.\n\
         - Start servers and other long-running processes in the background with output redirected to a file.\n\
         - When output is cut, the result says where the full output was saved or which offset to read next.\n\
         - Messages in this session are numbered (#n). In a long session, older turns are replaced by a summary; the history tool reads any earlier message back by number or searches them.\n\
         </tool_guidelines>\n"
    );
    let files = context::always(workspace);
    if !files.is_empty() {
        s.push_str("\n<project_context>\nInstructions the owner keeps for agents. Follow them.\n");
        for (path, text) in files {
            s.push_str(&format!("<file path=\"{}\">\n{}\n</file>\n", path.display(), text.trim_end()));
        }
        s.push_str("</project_context>\n");
    }
    s.push_str(&format!(
        "\nWorking directory for tools: {} (paths are relative to it unless absolute; ~ is the home directory).",
        workspace.display()
    ));
    s
}

/// The session's base instructions, fixed for the session: written on its first turn (a `base`
/// block), or, for a session from before briefed work, the instructions it was already using.
pub async fn base_prompt(db: &PgPool, session: Uuid, workspace: &Path, repo: &str) -> Result<String> {
    let blocks = tape::load(db, session, &["base", "envelope"]).await?;
    if let Some(b) = blocks.iter().find(|b| b.kind == "base") {
        return Ok(b.payload["text"].as_str().unwrap_or("").to_string());
    }
    if let Some(e) = blocks.iter().rev().find(|b| b.kind == "envelope") {
        if let Some(env) = load_envelope(db, e.payload["hash"].as_str().unwrap_or("")).await? {
            return Ok(env.system);
        }
    }
    let text = system_prompt(workspace, repo);
    tape::append(db, session, "base", &json!({ "text": text })).await?;
    Ok(text)
}

/// The envelope for this turn: the session's latest one if it has exactly these instructions and
/// tools, else a new one (stored once per distinct pair). Returns it and, when new, why
/// (`new`, `instructions`: the session's state changed what the model is told, `tools`).
pub async fn envelope(db: &PgPool, session: Uuid, system: &str, tools: &Value) -> Result<(Envelope, Option<&'static str>)> {
    let current = match tape::load(db, session, &["envelope"]).await?.last() {
        Some(b) => load_envelope(db, b.payload["hash"].as_str().unwrap_or("")).await?,
        None => None,
    };
    let reason = match &current {
        Some(e) if e.system == system && &e.tools == tools => return Ok((current.unwrap(), None)),
        Some(e) if e.system == system => "tools",
        Some(_) => "instructions",
        None => "new",
    };
    let row = sqlx::query(
        "WITH h AS (SELECT encode(sha256(convert_to($1 || $2::jsonb::text, 'UTF8')), 'hex') AS hash)
         INSERT INTO envelopes (hash, system, tools) SELECT hash, $1, $2 FROM h
         ON CONFLICT (hash) DO UPDATE SET hash = EXCLUDED.hash
         RETURNING hash",
    )
    .bind(system)
    .bind(tools)
    .fetch_one(db)
    .await?;
    let hash: String = row.get("hash");
    tape::append(db, session, "envelope", &json!({ "hash": hash, "reason": reason })).await?;
    Ok((Envelope { hash, system: system.to_string(), tools: tools.clone() }, Some(reason)))
}

async fn load_envelope(db: &PgPool, hash: &str) -> Result<Option<Envelope>, sqlx::Error> {
    let row = sqlx::query("SELECT hash, system, tools FROM envelopes WHERE hash = $1").bind(hash).fetch_optional(db).await?;
    Ok(row.map(|r| Envelope { hash: r.get("hash"), system: r.get("system"), tools: r.get("tools") }))
}

/// The turn context sent after the prompt: the date, only when it differs from the last one this
/// session was given (goose's turn-context message, deduplicated). None when nothing changed.
pub fn turn_context(blocks: &[Block], today: &str) -> Option<String> {
    let text = format!("<turn_context>\nToday is {today}.\n</turn_context>");
    let last = blocks
        .iter()
        .rev()
        .filter(|b| b.kind == "message" && b.payload["role"] == "user")
        .find_map(|b| b.payload["context"].as_str());
    if last == Some(text.as_str()) {
        None
    } else {
        Some(text)
    }
}

/// A summary that replaced older blocks, as it appears in the history.
#[derive(Clone, Debug, PartialEq)]
pub struct SummaryRef {
    /// The `compaction` block's number.
    pub seq: i32,
    /// The blocks it covers.
    pub from: i32,
    pub to: i32,
}

/// The history to send: the latest summary (if any) as the first message, then every message after
/// the blocks it covers (or after the start of the current work context), word for word and each
/// with its number. Nothing already sent is rewritten,
/// so each turn's history starts with the previous turn's (until the next summary).
pub fn history(blocks: &[Block]) -> (Vec<Value>, Option<SummaryRef>) {
    let summary = blocks.iter().rev().find(|b| b.kind == "compaction").map(|b| {
        let r = SummaryRef {
            seq: b.seq,
            from: b.payload["covers"][0].as_i64().unwrap_or(0) as i32,
            to: b.payload["covers"][1].as_i64().unwrap_or(0) as i32,
        };
        (r, b.payload["text"].as_str().unwrap_or("").to_string(), b.payload["timestamp"].clone())
    });
    // A new work context (an approved brief) starts the history afresh; earlier blocks stay on the
    // tape for the history tool.
    // Only while that work lasts: a new request (back to framing) sees the whole session again.
    let last_state = blocks.iter().rev().find(|b| b.kind == "state");
    let fresh = match last_state {
        Some(b) if b.payload["state"] == "framing" => 0,
        _ => blocks.iter().rev().find(|b| b.kind == "state" && b.payload["fresh"] == true).map(|b| b.seq).unwrap_or(0),
    };
    let summary = summary.filter(|(r, _, _)| r.seq > fresh);
    let after = summary.as_ref().map(|(r, _, _)| r.to).unwrap_or(0).max(fresh);
    let mut out = Vec::new();
    if let Some((r, text, ts)) = &summary {
        out.push(json!({ "role": "user", "content": text, "summary": true, "seq": r.seq, "timestamp": ts }));
    }
    for b in blocks.iter().filter(|b| b.kind == "message" && b.seq > after) {
        let mut m = b.payload.clone();
        m["seq"] = json!(b.seq);
        out.push(m);
    }
    (out, summary.map(|(r, _, _)| r))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(seq: i32, role: &str, content: Value) -> Block {
        Block { seq, kind: "message".into(), payload: json!({ "role": role, "content": content }) }
    }

    #[test]
    fn system_prompt_includes_context_files_from_ancestors() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("zend-ctx-{nanos}"));
        let ws = root.join("proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "outer rule").unwrap();
        std::fs::write(ws.join("AGENTS.md"), "inner rule").unwrap();
        std::fs::write(ws.join("CLAUDE.md"), "shadowed by AGENTS.md").unwrap();
        let prompt = system_prompt(&ws, "/repo");
        let outer = prompt.find("outer rule").expect("parent CLAUDE.md loaded");
        let inner = prompt.find("inner rule").expect("workspace AGENTS.md loaded");
        assert!(outer < inner, "files are ordered from the root down");
        assert!(!prompt.contains("shadowed"));
        assert!(prompt.contains(&format!("Working directory for tools: {}", ws.display())));
        assert!(!prompt.contains("Today is"), "the date changes daily, so it is not in the instructions");
    }

    #[test]
    fn turn_context_only_when_it_changed() {
        let mut blocks = vec![msg(1, "user", json!("hi"))];
        let first = turn_context(&blocks, "2026-10-05 (Monday)").expect("first turn gets the date");
        blocks[0].payload["context"] = json!(first);
        assert_eq!(turn_context(&blocks, "2026-10-05 (Monday)"), None, "same day: nothing to add");
        assert!(turn_context(&blocks, "2026-10-06 (Tuesday)").unwrap().contains("2026-10-06"));
    }

    /// The prefix-invariance property (goose's test): as a session grows, each turn's history
    /// starts with the previous turn's, so the cached prefix stays valid. Only a summary breaks it.
    #[test]
    fn history_is_append_only_until_a_summary() {
        let mut blocks = Vec::new();
        let mut previous: Vec<Value> = Vec::new();
        for turn in 0..20 {
            let base = turn * 4;
            blocks.push(msg(base + 1, "user", json!(format!("request {turn}"))));
            blocks.push(msg(base + 2, "assistant", json!([{ "type": "toolCall", "id": format!("c{turn}"), "name": "bash", "arguments": {} }])));
            blocks.push(msg(base + 3, "toolResult", json!([{ "type": "text", "text": "x".repeat(5000) }])));
            blocks.push(msg(base + 4, "assistant", json!([{ "type": "text", "text": "done" }])));
            let (h, s) = history(&blocks);
            assert!(s.is_none());
            assert_eq!(&h[..previous.len()], &previous[..], "turn {turn} rewrote earlier history");
            previous = h;
        }
        blocks.push(Block { seq: 81, kind: "compaction".into(), payload: json!({ "covers": [1, 60], "text": "<summary/>" }) });
        let (h, s) = history(&blocks);
        assert_eq!(s, Some(SummaryRef { seq: 81, from: 1, to: 60 }));
        assert_eq!(h[0]["summary"], true);
        assert_eq!(h[1]["seq"], 61, "messages after the covered blocks follow the summary");
        assert_eq!(h.len(), 1 + 20);
        blocks.push(Block { seq: 82, kind: "state".into(), payload: json!({ "state": "working", "fresh": true }) });
        blocks.push(msg(83, "user", json!("The brief is approved.")));
        let (h, s) = history(&blocks);
        assert_eq!((h.len(), s), (1, None), "an approved brief starts a fresh work context");
        blocks.push(Block { seq: 84, kind: "state".into(), payload: json!({ "state": "framing", "fresh": false }) });
        let (h, _) = history(&blocks);
        assert_eq!(h.len(), 1 + 20 + 1, "a new request sees the session again (from the summary on)");
    }
}
