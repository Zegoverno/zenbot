//! What a turn sends the model (docs/context.md), in an order that keeps the provider's prompt cache:
//! tools and instructions (fixed for the session) → summary → history (append-only) → the prompt
//! with its turn context (what changes per turn, at the end).

use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::{SqlitePool, Row};
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
    let c = crate::config::get();
    match path.rsplit('/').next().unwrap_or(path) {
        "SOUL.md" => c.soul_chars,
        "IDENTITY.md" => c.identity_chars,
        "AGENTS.md" => c.agents_chars,
        _ => c.user_chars,
    }
}

/// Text over `cap` characters, cut as OpenClaw does: the first 70% and the last 20% kept, with a
/// marker saying where to read the rest. Counted in characters, like every size the kernel reports,
/// and cut on character boundaries.
pub fn cut_middle(text: &str, cap: usize, path: &str) -> String {
    let n = text.chars().count();
    if n <= cap {
        return text.to_string();
    }
    let (head, tail) = (cap * 7 / 10, n - cap * 2 / 10);
    let at = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(i, _)| i);
    format!(
        "{}\n\n[... {} characters left out here (the file is over {cap}); read {path} for all of it ...]\n\n{}",
        &text[..at(head)],
        tail - head,
        &text[at(tail)..]
    )
}

/// The prompt files under the zen home, in the order they appear.
const PROMPT_FILES: [&str; 4] = [crate::layout::SOUL, crate::layout::IDENTITY, "AGENTS.md", "USER.md"];

/// The instruction texts a prompt may include, each the exact snapshot that was rated (D-062):
/// only text rated 0–3 is admitted, and the session depends on its writers. What was left out is
/// named, never shown.
#[derive(Clone, Debug, Default)]
pub struct Admitted {
    /// Prompt files by their path under the zen home.
    files: std::collections::HashMap<&'static str, String>,
    /// The workspace's instruction files, from `/` down (whole text; cut when shown).
    project: Vec<(std::path::PathBuf, String)>,
    /// Files left out: rated 4 or more, or written by a session since denied.
    withheld: Vec<String>,
}

impl Admitted {
    /// Every file as it is on disk, unrated: for tests of the prompt's shape only.
    #[cfg(test)]
    pub fn unrated(workspace: &Path) -> Self {
        let files = PROMPT_FILES.iter().filter_map(|rel| std::fs::read_to_string(crate::zen_home().join(rel)).ok().map(|t| (*rel, t))).collect();
        Admitted { files, project: context::always(workspace), withheld: Vec::new() }
    }
}

/// Rate the prompt files and the workspace's instruction files by their exact bytes and admit
/// those rated 0–3 (bytes no session registered count as 1: the owner's own edits). Recorded
/// dependencies come first, so a later denial of a writer stops this session. A database error
/// fails the whole prompt: nothing unrated gets in.
pub async fn admitted(db: &SqlitePool, session: Uuid, workspace: &Path) -> Result<Admitted> {
    let mut out = Admitted::default();
    for rel in PROMPT_FILES {
        let path = crate::zen_home().join(rel);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        match context::admit(db, session, &text, 1).await? {
            Some(risk) => {
                out.files.insert(rel, context::label(&format!("~/.zenbot/{rel}"), risk, &text));
            }
            None => out.withheld.push(format!("~/.zenbot/{rel}")),
        }
    }
    for (path, text) in context::always(workspace) {
        match context::admit(db, session, &text, 1).await? {
            Some(risk) => {
                let text = context::label(&path.display().to_string(), risk, &text);
                out.project.push((path, text));
            },
            None => out.withheld.push(path.display().to_string()),
        }
    }
    Ok(out)
}

/// A prompt file's text (`rel` under the zen home), from its admitted snapshot, with its
/// placeholders filled in, cut to its size.
fn prompt_file(rel: &str, vars: &[(&str, String)], admitted: &Admitted) -> Option<String> {
    let path = crate::zen_home().join(rel);
    let mut text = admitted.files.get(rel)?.clone();
    for (k, v) in vars {
        text = text.replace(&format!("{{{{{k}}}}}"), v);
    }
    Some(cut_middle(text.trim(), file_cap(rel), &path.display().to_string()))
}

/// The system prompt, fixed for the session: who the agent is (SOUL.md, then IDENTITY.md), its environment
/// (AGENTS.md), the owner (USER.md), its short-term memory (MEMORY.md, rendered by the kernel), the
/// skills index, and the instruction files of the workspace's projects (from `/` down). How to use
/// each tool is in the tool's own description; how to do a kind of work, in skills.
pub fn system_prompt(workspace: &Path, repo: &str, memory: &str, sleep_note: Option<&str>, skills_index: &str, admitted: &Admitted) -> String {
    compose(&PARTS, workspace, repo, memory, sleep_note, skills_index, admitted)
}

/// The parts of the instructions a session can have, in order. A scheduled job picks some (its
/// `context`, jobs.rs); the soul is always in.
pub const PARTS: [&str; 7] = ["soul", "identity", "agents", "user", "memory", "skills", "project"];

/// The instructions made of the given `parts` only (names from `PARTS`), in the usual order.
pub fn compose(parts: &[&str], workspace: &Path, repo: &str, memory: &str, sleep_note: Option<&str>, skills_index: &str, admitted: &Admitted) -> String {
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
    let soul = prompt_file(soul_rel, &vars, admitted).unwrap_or_else(|| "You are zenbot, the owner's agent on their Linux VM.".into());
    s.push_str(&format!("<soul file=\"~/.zenbot/{soul_rel}\">\n{soul}\n</soul>\n"));
    let identity_rel = crate::layout::IDENTITY;
    if let Some(identity) = prompt_file(identity_rel, &vars, admitted).filter(|_| has("identity")) {
        s.push_str(&format!("\n<identity file=\"~/.zenbot/{identity_rel}\">\n{identity}\n</identity>\n"));
    }
    if let Some(env) = prompt_file("AGENTS.md", &vars, admitted).filter(|_| has("agents")) {
        s.push_str(&format!("\n<environment file=\"~/.zenbot/AGENTS.md\">\n{env}\n</environment>\n"));
    }
    if let Some(user) = prompt_file("USER.md", &vars, admitted).filter(|_| has("user")) {
        s.push_str(&format!("\n<owner file=\"~/.zenbot/USER.md\">\n{user}\n</owner>\n"));
    }
    if has("memory") {
        s.push_str(&format!(
            "\n<memory size=\"{}/{}\">\nYour short-term memory as of this session's start (entries by id; change them with the remember tool). Long-term memories, knowledge and people/projects records are found with search.\n{}{}</memory>\n",
            memory.chars().count(),
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
    let files: &[(std::path::PathBuf, String)] = if has("project") { &admitted.project } else { &[] };
    if !files.is_empty() {
        s.push_str("\n<project_context>\nInstructions the owner keeps for agents in these projects. Follow them.\n");
        for (path, text) in files {
            s.push_str(&format!("<file path=\"{}\">\n{}\n</file>\n", path.display(), context::capped(text.clone()).trim_end()));
        }
        s.push_str("</project_context>\n");
    }
    if !admitted.withheld.is_empty() {
        s.push_str(&format!(
            "\nLeft out of these instructions: {} (the current version was written by a session that read outside content, or one the owner stopped; the owner reviews it).\n",
            admitted.withheld.join(", ")
        ));
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
pub async fn base_prompt(db: &SqlitePool, session: Uuid, blocks: &[Block], workspace: &Path, repo: &str) -> Result<String> {
    if let Some(b) = blocks.iter().find(|b| b.kind == "base") {
        return Ok(b.payload["text"].as_str().unwrap_or("").to_string());
    }
    if let Some(e) = blocks.iter().rev().find(|b| b.kind == "envelope") {
        if let Some(env) = load_envelope(db, e.payload["hash"].as_str().unwrap_or("")).await? {
            return Ok(env.system);
        }
    }
    let memory = crate::memory::render(db, session).await?;
    // The morning note: what the last sleep did, and what the nightly upkeep jobs found.
    let note: Vec<String> = [crate::memory::morning_note(db).await?, crate::jobs::upkeep_note(db).await?].into_iter().flatten().collect();
    let note = (!note.is_empty()).then(|| note.join("\n"));
    let skills = crate::skills::instruction_index(db, session).await?;
    let admitted = admitted(db, session, workspace).await?;
    // A writer denied while this was assembled reaches the session through the dependencies
    // just recorded: never fix instructions for a session that is already stopped.
    anyhow::ensure!(!crate::taint::stopped(db, session).await?, "instructions withheld: a source of them was stopped by the owner");
    let text = system_prompt(workspace, repo, &memory, note.as_deref(), &skills, &admitted);
    tape::append(db, session, "base", &json!({ "text": text })).await?;
    Ok(text)
}

/// The envelope for this turn: the session's latest one if it has exactly these instructions and
/// tools, else a new one (stored once per distinct pair). Returns it and, when new, why
/// (`new`, `instructions`: the session's state changed what the model is told, `tools`).
pub async fn envelope(db: &SqlitePool, session: Uuid, blocks: &[Block], system: &str, tools: &Value) -> Result<(Envelope, Option<&'static str>)> {
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
        "WITH h AS (SELECT sha256_hex($1 || $2) AS hash)
         INSERT INTO envelopes (hash, system, tools) SELECT hash, $1, $2 FROM h WHERE true
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

async fn load_envelope(db: &SqlitePool, hash: &str) -> Result<Option<Envelope>, sqlx::Error> {
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
    let after = summary.as_ref().map(|(r, _, _)| r.to).unwrap_or(0);
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
        let prompt = system_prompt(&ws, "/repo", "- [m1] a memory\n", Some("Last sleep: 1 kept."), "- work/verify: Check work.\n", &Admitted::unrated(&ws));
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
        // Sizes are characters, not bytes: 100 two-byte characters fit a cap of 100.
        let accents = "é".repeat(100);
        assert_eq!(cut_middle(&accents, 100, "/p"), accents);
        let cut = cut_middle(&"é".repeat(200), 100, "/p");
        assert!(cut.starts_with(&"é".repeat(70)) && cut.ends_with(&format!("\n\n{}", "é".repeat(20))) && cut.contains("[... 110 characters left out"), "{cut}");
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
        assert_eq!((h.len(), s.is_some()), (1 + 20 + 1, true), "legacy workflow state cannot hide history");
        blocks.push(Block { seq: 84, kind: "state".into(), payload: json!({ "state": "framing", "fresh": false }) });
        let (h, _) = history(&blocks);
        assert_eq!(h.len(), 1 + 20 + 1, "a new request sees the session again (from the summary on)");
    }
}
