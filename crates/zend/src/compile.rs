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

/// Largest size of each prompt file in the instructions (characters): SOUL.md 4000
/// (ZEN_SOUL_CHARS), IDENTITY.md 4000 (ZEN_IDENTITY_CHARS), AGENTS.md 12000 (ZEN_AGENTS_CHARS), USER.md 3000 (ZEN_USER_CHARS). Sizes from
/// Hermes and OpenClaw (DESIGN.md, Target design).
pub(crate) fn file_cap(path: &str) -> usize {
    let (key, default) = match path.rsplit('/').next().unwrap_or(path) {
        "SOUL.md" => ("ZEN_SOUL_CHARS", 4000.0),
        "IDENTITY.md" => ("ZEN_IDENTITY_CHARS", 4000.0),
        "AGENTS.md" => ("ZEN_AGENTS_CHARS", 12000.0),
        _ => ("ZEN_USER_CHARS", 3000.0),
    };
    crate::env_num(key, default) as usize
}

/// Text over `cap` characters, cut as OpenClaw does: the first 70% and the last 20% kept, with a
/// marker saying where to read the rest.
pub fn cut_middle(text: &str, cap: usize, path: &str) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let head = text.floor_char_boundary(cap * 7 / 10);
    let tail = text.ceil_char_boundary(text.len() - cap * 2 / 10);
    format!(
        "{}\n\n[... {} characters left out here (the file is over {cap}); read {path} for all of it ...]\n\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
}

/// A prompt file's text (`rel` under the zen home) with its placeholders filled in, cut to its size.
fn prompt_file(rel: &str, vars: &[(&str, String)]) -> Option<String> {
    let path = crate::zen_home().join(rel);
    let mut text = std::fs::read_to_string(&path).ok()?;
    for (k, v) in vars {
        text = text.replace(&format!("{{{{{k}}}}}"), v);
    }
    Some(cut_middle(text.trim(), file_cap(rel), &path.display().to_string()))
}

/// The system prompt, fixed for the session: who the agent is (SOUL.md, then IDENTITY.md), its environment
/// (AGENTS.md), the owner (USER.md), its short-term memory (MEMORY.md, rendered by the kernel), the
/// skills index, and the instruction files of the workspace's projects (from `/` down). How to use
/// each tool is in the tool's own description; how to do a kind of work, in skills.
pub fn system_prompt(workspace: &Path, repo: &str, memory: &str, sleep_note: Option<&str>, skills_index: &str) -> String {
    compose(&PARTS, workspace, repo, memory, sleep_note, skills_index)
}

/// The parts of the instructions a session can have, in order. A scheduled job picks some (its
/// `context`, jobs.rs); the soul is always in.
pub const PARTS: [&str; 7] = ["soul", "identity", "agents", "user", "memory", "skills", "project"];

/// The instructions made of the given `parts` only (names from `PARTS`), in the usual order.
pub fn compose(parts: &[&str], workspace: &Path, repo: &str, memory: &str, sleep_note: Option<&str>, skills_index: &str) -> String {
    let has = |p: &str| parts.contains(&p);
    let home = std::env::var("HOME").unwrap_or_default();
    let vars = [
        ("workspace", workspace.display().to_string()),
        ("home", home),
        ("zen_home", crate::zen_home().display().to_string()),
        ("repo", repo.to_string()),
    ];
    let mut s = String::new();
    // The agent's own soul; the environment and the owner are system-wide (layout.rs).
    let soul_rel = crate::layout::SOUL;
    let soul = prompt_file(soul_rel, &vars).unwrap_or_else(|| "You are zenbot, the owner's agent on their Linux VM.".into());
    s.push_str(&format!("<soul file=\"~/.zenbot/{soul_rel}\">\n{soul}\n</soul>\n"));
    let identity_rel = crate::layout::IDENTITY;
    if let Some(identity) = prompt_file(identity_rel, &vars).filter(|_| has("identity")) {
        s.push_str(&format!("\n<identity file=\"~/.zenbot/{identity_rel}\">\n{identity}\n</identity>\n"));
    }
    if let Some(env) = prompt_file("AGENTS.md", &vars).filter(|_| has("agents")) {
        s.push_str(&format!("\n<environment file=\"~/.zenbot/AGENTS.md\">\n{env}\n</environment>\n"));
    }
    if let Some(user) = prompt_file("USER.md", &vars).filter(|_| has("user")) {
        s.push_str(&format!("\n<owner file=\"~/.zenbot/USER.md\">\n{user}\n</owner>\n"));
    }
    if has("memory") {
        s.push_str(&format!(
            "\n<memory size=\"{}/{}\">\nYour short-term memory as of this session's start (entries by id; change them with the remember tool).\n{}{}</memory>\n",
            memory.len(),
            crate::memory::cap(),
            if memory.is_empty() { "(empty)\n".to_string() } else { memory.to_string() },
            sleep_note.map(|n| format!("\n{n}\n")).unwrap_or_default()
        ));
    }
    if !skills_index.is_empty() && has("skills") {
        s.push_str(&format!(
            "\n<skills>\nHow to do kinds of work well. When a job matches one, load it with load_skill and follow it; find_skills searches them.\n{skills_index}</skills>\n"
        ));
    }
    let files = if has("project") { context::always(workspace) } else { Vec::new() };
    if !files.is_empty() {
        s.push_str("\n<project_context>\nInstructions the owner keeps for agents in these projects. Follow them.\n");
        for (path, text) in files {
            s.push_str(&format!("<file path=\"{}\">\n{}\n</file>\n", path.display(), text.trim_end()));
        }
        s.push_str("</project_context>\n");
    }
    s.push_str(&format!(
        "\nWorking directory for tools: {} (paths are relative to it unless absolute; ~ is the home directory). \
Every command already starts there, so don't cd into it first.",
        workspace.display()
    ));
    // Claude Code runs from a fixed engine folder (so its sessions resume and stay cached) and appends
    // an environment block naming that folder; left unexplained, the model cds into the workspace on
    // every command and trusts the block's git status. Codex gets the same note from its worker.
    s.push_str(
        "\nAn environment block after these instructions, if there is one, describes the engine's own empty folder, \
not this workspace: ignore its working directory and git status, and check the workspace with git yourself.",
    );
    s
}

/// The session's base instructions, fixed for the session: written on its first turn (a `base`
/// block), or, for a session from before briefed work, the instructions it was already using.
pub async fn base_prompt(db: &PgPool, session: Uuid, blocks: &[Block], workspace: &Path, repo: &str) -> Result<String> {
    if let Some(b) = blocks.iter().find(|b| b.kind == "base") {
        return Ok(b.payload["text"].as_str().unwrap_or("").to_string());
    }
    if let Some(e) = blocks.iter().rev().find(|b| b.kind == "envelope") {
        if let Some(env) = load_envelope(db, e.payload["hash"].as_str().unwrap_or("")).await? {
            return Ok(env.system);
        }
    }
    let memory = crate::memory::render(db).await?;
    let note = crate::memory::morning_note(db).await?;
    let skills = crate::skills::index_text(&crate::skills::scan(&crate::skills::root()));
    let text = system_prompt(workspace, repo, &memory, note.as_deref(), &skills);
    tape::append(db, session, "base", &json!({ "text": text })).await?;
    Ok(text)
}

/// The envelope for this turn: the session's latest one if it has exactly these instructions and
/// tools, else a new one (stored once per distinct pair). Returns it and, when new, why
/// (`new`, `instructions`: the session's state changed what the model is told, `tools`).
pub async fn envelope(db: &PgPool, session: Uuid, blocks: &[Block], system: &str, tools: &Value) -> Result<(Envelope, Option<&'static str>)> {
    let current = match blocks.iter().rev().find(|b| b.kind == "envelope") {
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
        let root = crate::test_util::TestDir::new("ctx");
        let ws = root.join("proj");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "outer rule").unwrap();
        std::fs::write(ws.join("AGENTS.md"), "inner rule").unwrap();
        std::fs::write(ws.join("CLAUDE.md"), "shadowed by AGENTS.md").unwrap();
        let prompt = system_prompt(&ws, "/repo", "- [m1] a memory\n", Some("Last sleep: 1 kept."), "- work/verify: Check work.\n");
        let outer = prompt.find("outer rule").expect("parent CLAUDE.md loaded");
        let inner = prompt.find("inner rule").expect("workspace AGENTS.md loaded");
        assert!(outer < inner, "files are ordered from the root down");
        assert!(!prompt.contains("shadowed"));
        assert!(prompt.contains(&format!("Working directory for tools: {}", ws.display())));
        assert!(prompt.contains("don't cd into it") && prompt.contains("ignore its working directory"));
        assert!(!prompt.contains("Today is"), "the date changes daily, so it is not in the instructions");
        assert!(prompt.contains("[m1] a memory") && prompt.contains("- work/verify: Check work."));
        assert!(prompt.find("Last sleep: 1 kept.").unwrap() < prompt.find("</memory>").unwrap());
        assert!(prompt.find("<memory").unwrap() < prompt.find("<skills>").unwrap());
        assert!(prompt.find("<skills>").unwrap() < prompt.find("<project_context>").unwrap());
    }

    #[test]
    fn long_files_keep_their_start_and_end() {
        let text = format!("START{}END", "x".repeat(1000));
        let cut = cut_middle(&text, 100, "/p");
        assert!(cut.starts_with("START") && cut.ends_with("END") && cut.contains("read /p"));
        assert_eq!(cut_middle("short", 100, "/p"), "short");
    }

    #[test]
    fn turn_context_only_when_it_changed() {
        let mut blocks = vec![msg(1, "user", json!("hi"))];
        let first = turn_context(&blocks, "2026-10-05 (Monday)").expect("first turn gets the date");
        assert_eq!(first, "<turn_context>\nToday is 2026-10-05 (Monday).\n</turn_context>", "unchanged format: earlier turns' context stays comparable");
        blocks[0].payload["context"] = json!(first);
        assert_eq!(turn_context(&blocks, "2026-10-05 (Monday)"), None, "nothing changed: nothing to add");
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
