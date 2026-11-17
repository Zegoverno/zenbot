//! The agent's tools beyond files and the shell: `ask`,
//! `remember`, `find_skills` / `load_skill`, `verify`, `decide` and `history`, plus the verifier
//! session's `submit_verdict`.
//!
//! There is no workflow here: how to frame a job or check it is knowledge in skills (`work/brief`,
//! `work/verify`) the agent loads when they help. The kernel keeps only what can't be left to the
//! model: a fresh verifier never sees the maker's reasoning and can't change anything, the
//! criteria's commands are run by the kernel, and a question to the owner ends the turn.
//!
//! The tool list is the same in every turn of a session (it is part of the cached prefix), in a
//! fixed order.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::{tape, tools, AppState};

/// The kind of a child session that checks another session's work.
pub const VERIFIER: &str = "verifier";
const VERIFY_PROMPT: &str = include_str!("../steps/verify.md");
/// The kind of a session a chat room (the Matrix bridge) keeps open: the owner's own session.
pub const CHANNEL: &str = "channel";

/// Whether the owner drives this kind of session (a terminal session or a chat room's channel):
/// it gets the owner's whole tool list and never waits behind background work.
pub fn owner_driven(kind: Option<&str>) -> bool {
    matches!(kind, None | Some(CHANNEL))
}

/// Whether a session of this kind may only read (a verifier: writes refused, `bash` in a
/// read-only sandbox).
pub fn read_only(kind: Option<&str>) -> bool {
    kind == Some(VERIFIER)
}

/// Why a session of this kind may not call `name`, if it may not. The one place the kernel enforces
/// what each kind of session may do (what it is offered is `specs`, which follows the same rules).
/// Each kind is allowed what it needs; a kind not named here gets nothing.
pub fn refusal(kind: Option<&str>, name: &str) -> Option<String> {
    if matches!(name, "capture" | "save_tool") {
        return Some("This tool is retired: write wiki pages (~/.zenbot/global/wiki) for knowledge, or a reviewed skill with supporting scripts.".into());
    }
    match kind {
        None | Some(CHANNEL) => (name == "submit_verdict").then(|| "`submit_verdict` is only for a verifier session".into()),
        Some(VERIFIER) => (!matches!(name, "bash" | "read" | "submit_verdict")).then(|| format!("`{name}` isn't available to a verifier")),
        // A subagent (also the sleep's kernel sessions) or a scheduled job can't ask the owner,
        // delegate or schedule.
        Some(crate::delegate::SUBAGENT) if matches!(name, "ask" | "delegate" | "schedule") => {
            Some(format!("`{name}` isn't available to a subagent: decide yourself and say what you assumed"))
        }
        Some(crate::jobs::JOB) if matches!(name, "ask" | "delegate" | "schedule") => {
            Some(format!("`{name}` isn't available in a scheduled job: no one is in the conversation; decide yourself and say what you assumed in the report"))
        }
        Some(crate::delegate::SUBAGENT | crate::jobs::JOB) => (name == "submit_verdict").then(|| "`submit_verdict` is only for a verifier session".into()),
        Some(other) => Some(format!("a `{other}` session has no tools")),
    }
}

/// Whether the kernel knows this kind of session (an unknown kind is offered no tools).
fn known(kind: Option<&str>) -> bool {
    owner_driven(kind) || matches!(kind, Some(VERIFIER | crate::delegate::SUBAGENT | crate::jobs::JOB))
}

/// A file an `edit` or `write` call treats specially.
#[derive(Debug, PartialEq)]
pub enum Protected {
    /// A prompt file: backed up before it changes.
    PromptFile(PathBuf),
    /// Short-term memory and record files (long-term memory, entities, old knowledge): the agent
    /// changes them through `remember` and `brain`, which keep their names, links and index.
    Record(PathBuf),
    /// An agent's `agent.toml`: the owner's alone, never changed by an agent (agents.rs).
    AgentSettings(PathBuf),
}

/// Where the special files are: the zen home and the wiki (each can be moved by a setting).
pub struct Guarded {
    pub home: PathBuf,
    pub wiki: PathBuf,
}

impl Guarded {
    pub fn current() -> Self {
        Guarded { home: crate::zen_home(), wiki: crate::wiki::root() }
    }
}

/// The real path a tool would write: symlinks resolved as far as the path exists, the rest (folders
/// `write` would create) added after it, `..` and all.
pub(crate) fn real(p: &Path) -> PathBuf {
    use std::path::Component;
    let parts: Vec<Component> = p.components().collect();
    for split in (1..=parts.len()).rev() {
        let head: PathBuf = parts[..split].iter().collect();
        let Ok(mut out) = std::fs::canonicalize(&head) else { continue };
        let rest = &parts[split..];
        let climbs = rest.iter().any(|c| matches!(c, Component::ParentDir));
        for c in rest {
            match c {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                c => out.push(c),
            }
        }
        // A `..` past a missing folder lands somewhere that may exist (and be a symlink): resolve again.
        return if climbs { real(&out) } else { out };
    }
    p.to_path_buf()
}

/// What an `edit` or `write` call targets, if it is protected (however it is named: `~`, relative
/// paths, `..`, symlinks such as the old `~/.zenbot/SOUL.md`, or folders that don't exist yet).
pub fn protected_target(g: &Guarded, workspace: &Path, name: &str, args: &Value) -> Option<Protected> {
    if !matches!(name, "edit" | "write") {
        return None;
    }
    let target = real(&tools::resolve(workspace, args["path"].as_str()?));
    let is = |p: PathBuf| real(&p) == target;
    let under = |p: PathBuf| target.starts_with(real(&p));
    let prompt_files = crate::agents::prompt_files(&g.home).into_iter().chain(["AGENTS.md".to_string(), "USER.md".to_string()]);
    if prompt_files.into_iter().any(|rel| is(g.home.join(rel))) {
        return Some(Protected::PromptFile(target));
    }
    if crate::agents::is_settings_file(&g.home, &target) {
        return Some(Protected::AgentSettings(target));
    }
    let global = g.home.join("global");
    let knowledge = ["concepts", "claims", "arguments", "questions", "syntheses", "sources", "events", "subjects"];
    if is(global.join("MEMORY.md")) || under(global.join("memory")) || under(global.join("entities")) || knowledge.iter().any(|k| under(g.wiki.join(k))) {
        return Some(Protected::Record(target));
    }
    None
}

/// The prompt file an `edit` or `write` call targets, if it targets one (it is backed up first).
pub fn prompt_file_target(workspace: &Path, name: &str, args: &Value) -> Option<PathBuf> {
    match protected_target(&Guarded::current(), workspace, name, args)? {
        Protected::PromptFile(p) => Some(p),
        _ => None,
    }
}

/// Why an `edit` or `write` call may not change this file, if it may not: memory and record files
/// change only through `remember` and `brain`.
pub fn record_refusal(workspace: &Path, name: &str, args: &Value) -> Option<String> {
    match protected_target(&Guarded::current(), workspace, name, args)? {
        Protected::Record(p) => Some(format!(
            "{} is a memory or record file: change it with `remember` (short-term memory) or `brain` put (records), which keep its names, links and index.",
            p.display()
        )),
        Protected::AgentSettings(p) => Some(format!(
            "{} is an agent's settings: only the owner changes it. Suggest the change to them instead.",
            p.display()
        )),
        Protected::PromptFile(_) => None,
    }
}

/// What to tell the agent after it changed a prompt file that is now over its size in the
/// instructions: compact it now.
pub fn over_cap_note(path: &Path) -> Option<String> {
    let n = std::fs::read_to_string(path).ok()?.chars().count();
    let cap = crate::compile::file_cap(&path.display().to_string());
    let file = path.file_name()?.to_string_lossy();
    (n > cap).then(|| {
        format!(
            "\n\n{file} is now {n} characters, over the {cap} that fit in your instructions (the middle gets cut there). Compact it now: \
rewrite it with `write` to under {} characters, keeping only what really matters, merging what overlaps and dropping what's stale. \
A backup of every version is in ~/.zenbot/backups/prompt-files/.",
            cap * 8 / 10
        )
    })
}

fn spec(name: &str, description: &str, parameters: Value) -> Value {
    json!({ "name": name, "description": description, "parameters": parameters })
}

fn ask_spec() -> Value {
    spec(
        "ask",
        "Ask only questions the owner must decide: preferences, public/costly/irreversible actions. Never ask what you can find or decide. End the turn for answers; wait=false allows independent work. Give 2–4 options, recommendation first; use and disclose the recommendation for unanswered questions.",
        json!({ "type": "object", "properties": { "questions": { "type": "array", "description": "1 to 3 questions",
            "items": { "type": "object", "properties": {
                "question": { "type": "string" },
                "options": { "type": "array", "items": { "type": "string" }, "description": "2 to 4 options, recommended first" } },
              "required": ["question", "options"] } },
            "wait": { "type": "boolean", "description": "End the turn and wait for the answers (default true)" } }, "required": ["questions"] }),
    )
}

fn verify_spec() -> Value {
    spec(
        "verify",
        "Check work before reporting completion when existing checks are insufficient or risk warrants it. Commands pass with exit 0 and optional expect text. Criteria without commands go to a fresh read-only verifier with goal, criteria, diff and summary; may take minutes. Returns pass/fail/uncertain with evidence. Skip when all criteria already pass commands.",
        json!({ "type": "object", "properties": {
            "goal": { "type": "string", "description": "What the job had to achieve" },
            "criteria": { "type": "array", "description": "What must be true", "items": { "type": "object", "properties": {
                "id": { "type": "string" }, "text": { "type": "string", "description": "The criterion" },
                "run": { "type": "string", "description": "Shell command that checks it" },
                "expect": { "type": "string", "description": "Text its output must contain" } }, "required": ["text"] } },
            "dir": { "type": "string", "description": "Directory the work changed (default: workspace)" },
            "base": { "type": "string", "description": "Commit the work started from (default HEAD)" },
            "summary": { "type": "string", "description": "What you did and how you checked it" },
            "fresh": { "type": "boolean", "description": "Run the verifier even if every criterion has a command" } },
          "required": ["goal", "criteria"] }),
    )
}

fn verdict_spec() -> Value {
    spec(
        "submit_verdict",
        "Give your verdict on each criterion, once, then end your turn.",
        json!({ "type": "object", "properties": {
            "criteria": { "type": "array", "items": { "type": "object", "properties": {
                "id": { "type": "string" }, "verdict": { "type": "string", "description": "pass, fail or uncertain" },
                "evidence": { "type": "string" } }, "required": ["id", "verdict", "evidence"] } },
            "notes": { "type": "string", "description": "Scope problems, broken must-nots, anything else the owner should know" } },
          "required": ["criteria"] }),
    )
}

fn judge_spec() -> Value {
    spec(
        "judge",
        "Classify up to 200 items with the same quick question using System One; returns calibrated probabilities. Use to triage results, files, logs or messages before reading selected items. Unsuitable for reasoning or exact facts.",
        json!({ "type": "object", "properties": {
            "question": { "type": "string", "description": "One question, asked about each item, e.g. \"Is this file about authentication?\"" },
            "type": { "type": "string", "description": "yes/no (default), choice or score" },
            "options": { "type": "array", "items": { "type": "string" }, "description": "choice: the options; score: the levels, lowest first" },
            "items": { "type": "array", "description": "The items to judge (strings or small objects), at most 200" },
            "context": { "type": "string", "description": "What the question is for, shared by every item (optional)" } },
          "required": ["question", "items"] }),
    )
}

/// Whether the `judge` tool is offered: a System One model is configured, private material may go
/// to it (ZEN_S1_PRIVATE), and ZEN_JUDGE_TOOL isn't 0.
fn judge_on() -> bool {
    crate::config::get().judge_tool && crate::s1::on() && crate::s1::private_ok()
}

/// The tools a session of this kind is offered, in a fixed order.
pub fn specs(kind: Option<&str>) -> Value {
    if !known(kind) {
        return json!([]);
    }
    let builtins = tools::specs().as_array().cloned().unwrap_or_default();
    if read_only(kind) {
        let mut picked: Vec<Value> = builtins.into_iter().filter(|t| matches!(t["name"].as_str(), Some("read" | "bash"))).collect();
        for t in picked.iter_mut().filter(|t| t["name"] == "bash") {
            let d = t["description"].as_str().unwrap_or("").to_string();
            t["description"] = json!(format!("{d} For a verifier the filesystem is read-only: writes fail."));
        }
        picked.push(verdict_spec());
        return Value::Array(picked);
    }
    // A subagent or a scheduled job can't ask the owner, delegate or schedule.
    let subagent = kind == Some(crate::delegate::SUBAGENT) || kind == Some(crate::jobs::JOB);
    let mut all = builtins;
    all.push(crate::compact::tool_spec());
    all.push(crate::search::spec());
    if !subagent {
        all.push(ask_spec());
    }
    all.push(crate::memory::spec());
    all.push(crate::brain::spec());
    all.push(crate::web::search_spec());
    all.push(crate::web::fetch_spec());
    all.push(crate::skills::find_spec());
    all.push(crate::skills::load_spec());
    all.push(crate::workshop::save_skill_spec());
    all.push(crate::mcp::find_spec());
    all.push(crate::mcp::load_spec());
    all.push(crate::mcp::call_spec());
    all.push(verify_spec());
    if !subagent {
        all.push(crate::delegate::spec());
        all.push(crate::jobs::spec());
    }
    if judge_on() {
        all.push(judge_spec());
    }
    Value::Array(all)
}

/// The tools a session of this kind offers agent `agent`: `specs` with only what the agent may use.
pub fn specs_for(kind: Option<&str>, agent: &crate::agents::Agent) -> Value {
    agent.filter(specs(kind))
}

/// The instructions of a session of this kind: the session's base prompt, or the verifier's own.
pub fn system_for(base: &str, kind: Option<&str>) -> String {
    if read_only(kind) {
        VERIFY_PROMPT.trim_end().to_string()
    } else if kind == Some(crate::delegate::SUBAGENT) {
        crate::delegate::system_for(base)
    } else {
        base.to_string()
    }
}

/// Run one of the agent's tools (anything not a file or shell built-in). None for other tools.
/// `ending` is set when the tool ends the model's turn (further calls are refused).
pub async fn run_tool(app: &AppState, session: Uuid, workspace: &Path, who: &crate::agents::Agent, name: &str, args: &Value, ending: &mut Option<&'static str>) -> Option<tools::ToolOutput> {
    let out = |content: String, is_error: bool| Some(tools::ToolOutput::text(content, is_error));
    match name {
        "history" => {
            let (content, is_error) = crate::compact::history_tool(&app.db, session, args).await;
            out(content, is_error)
        }
        "ask" => {
            let qs = args["questions"].as_array().cloned().unwrap_or_default();
            let bad = qs.is_empty()
                || qs.len() > 3
                || qs.iter().any(|q| q["question"].as_str().unwrap_or("").is_empty() || !(2..=4).contains(&q["options"].as_array().map_or(0, Vec::len)));
            if bad {
                return out("Ask 1 to 3 questions, each with 2 to 4 options (your recommended one first).".into(), true);
            }
            if let Err(e) = tape::append(&app.db, session, "questions", &json!({ "questions": qs })).await {
                return out(format!("couldn't record the questions: {e}"), true);
            }
            app.emit(session, json!({ "type": "questions", "questions": qs })).await;
            if args["wait"] == false {
                return out("The questions are shown to the owner. Carry on with what doesn't depend on them; their answers arrive as \
a later message. Until then, don't act on anything that does."
                    .into(), false);
            }
            *ending = Some("questions asked");
            out("The questions are shown to the owner. End your turn now; the answers arrive as the next message. \
If a question goes unanswered, take your recommended option and say it was an assumption."
                .into(), false)
        }
        "submit_verdict" => {
            if let Err(e) = tape::append(&app.db, session, "verdict", args).await {
                return out(format!("couldn't record the verdict: {e}"), true);
            }
            *ending = Some("verdict given");
            out("Verdict recorded. End your turn now.".into(), false)
        }
        "brain" => {
            let (content, is_error) = crate::brain::run_tool(app, session, args).await;
            out(content,is_error)
        }
        "remember" => {
            let (content, is_error) = crate::memory::run_tool(app, session, args).await;
            out(content, is_error)
        }
        "find_skills" | "load_skill" => {
            let (content, is_error) = crate::skills::run_tool(&app.db, session, &who.skill_roots(), name, args).await;
            out(content, is_error)
        }
        "verify" => Some(verify(app, session, workspace, who, args).await),
        "web_search" | "web_fetch" => crate::web::run_tool(app, name, args).await,
        "search" => crate::search::run_tool(app, session, who, name, args).await,
        "save_skill" => crate::workshop::run_tool(app, session, who, name, args).await,
        "delegate" => crate::delegate::run_tool(app, session, workspace, who, name, args).await,
        "schedule" => crate::jobs::run_tool(app, session, name, args).await,
        "find_tools" | "load_tool" | "call_tool" => crate::mcp::run_tool(app, session, who, name, args).await,
        "judge" => Some(judge(app, session, args).await),
        _ => None,
    }
}

/// The `judge` tool: S1's answers, one line per item.
async fn judge(app: &crate::App, session: Uuid, args: &Value) -> tools::ToolOutput {
    let out = |content: String, is_error: bool| tools::ToolOutput::text(content, is_error);
    let Some(question) = args["question"].as_str().map(str::trim).filter(|q| !q.is_empty()) else { return out("judge needs a `question`".into(), true) };
    let kind = args["type"].as_str().unwrap_or("yes/no");
    let items: Vec<Value> = args["items"].as_array().cloned().unwrap_or_default();
    let n = items.len();
    match crate::s1::run::judge(&app.db, session, question, kind, args.get("options"), items, args["context"].as_str()).await {
        Err(e) => out(format!("judge: {e}"), true),
        Ok(ds) => {
            let model = ds.iter().find_map(|d| d.model.clone()).unwrap_or_default();
            let mut text = format!("judge: \"{question}\" ({kind}, {n} items, {model})\n");
            for (i, d) in ds.iter().enumerate() {
                let a = &d.answers["answer"];
                let line = match a["type"].as_str() {
                    Some("bool") => a["probability"].as_f64().map(|p| format!("{} (p yes {p:.2})", if p >= 0.5 { "yes" } else { "no" })),
                    Some("choice") => a["choice"].as_str().map(|c| match crate::s1::rules::choice_p(a) { Some(p) => format!("{c} ({p:.2})"), None => c.to_string() }),
                    Some("score") => a["score"].as_f64().map(|s| {
                        let levels: Vec<&str> = args["options"].as_array().into_iter().flatten().filter_map(|l| l.as_str()).collect();
                        let level = levels.get(s.round().clamp(0.0, levels.len().saturating_sub(1) as f64) as usize).copied().unwrap_or("");
                        match a["confidence"].as_f64() { Some(c) => format!("{level} (score {s:.1}, confidence {c:.2})"), None => format!("{level} (score {s:.1})") }
                    }),
                    _ => None,
                };
                text.push_str(&format!("[{}] {}\n", i + 1, line.unwrap_or_else(|| "no answer".into())));
            }
            out(text, false)
        }
    }
}

/// Why a tool call is refused, if it is: nothing more once the model ended its turn with a tool
/// (a question to the owner, a verdict).
pub fn refuse(ending: Option<&str>) -> Option<String> {
    ending.map(|e| format!("You already ended this turn ({e}). End your turn now, without further tool calls."))
}

// ---------- verify ----------

/// A criterion's check, run by the kernel like the bash tool (its own process group, killed on a
/// timeout or an abort, output capped and masked).
async fn run_check(dir: &Path, run: &str, expect: Option<&str>, session: Uuid, sandbox: Option<&crate::sandbox::Sandbox>) -> Value {
    let id = session.to_string();
    match tools::run_shell(dir, run, &[("ZEN_SESSION_ID", id.as_str())], Duration::from_secs(600), sandbox).await {
        Ok(tools::Shell { text, status: Some(Ok(status)) }) => {
            let text = crate::secrets::mask_off_thread(text).await;
            let tail = zen_proto::tail(&text, 3000);
            let found = expect.is_none_or(|e| text.contains(e));
            json!({ "ok": status.success() && found, "exit": status.code(), "expect_found": found, "output": tail })
        }
        Ok(tools::Shell { status: Some(Err(e)), .. }) => json!({ "ok": false, "output": format!("couldn't run it: {e}") }),
        Ok(tools::Shell { status: None, .. }) => json!({ "ok": false, "output": "timed out after 600s" }),
        Err(e) => json!({ "ok": false, "output": format!("couldn't run it: {e}") }),
    }
}

/// The diff from `base` (and untracked files), masked and capped.
async fn diff_from(repo: &Path, base: &str, sandbox: Option<&crate::sandbox::Sandbox>) -> String {
    const CAP: usize = 60_000;
    let quoted = format!("'{}'", base.replace('\'', "'\\''"));
    let output = |command: String| async move {
        tools::run_shell(repo, &command, &[], Duration::from_secs(30), sandbox)
            .await.map(|s| s.text).unwrap_or_default()
    };
    let mut d = output(format!("git diff --end-of-options {quoted}")).await;
    let untracked = output("git ls-files --others --exclude-standard".into()).await;
    if !untracked.trim().is_empty() {
        d.push_str(&format!("\nUntracked files:\n{untracked}"));
    }
    let d = crate::secrets::mask(&d);
    if d.len() > CAP {
        format!("{}\n[diff cut at 60 KB; read files for the rest]", &d[..d.floor_char_boundary(CAP)])
    } else if d.trim().is_empty() {
        "(no changes)".into()
    } else {
        d
    }
}

/// The criteria as given, each with an id.
fn criteria_of(args: &Value) -> Vec<Value> {
    args["criteria"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| {
            let mut c = if c.is_string() { json!({ "text": c }) } else { c.clone() };
            if c["id"].as_str().is_none_or(str::is_empty) {
                c["id"] = json!(format!("c{}", i + 1));
            }
            c
        })
        .collect()
}

/// An id that more than one criterion has, if any.
fn duplicate_id(criteria: &[Value]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    criteria.iter().filter_map(|c| c["id"].as_str()).find(|id| !seen.insert(*id)).map(String::from)
}

/// Combine the kernel's checks and the verifier's verdict: a failed command can't be overridden;
/// otherwise the verifier's judgment, or uncertain when there was none.
pub fn combine(criteria: &[Value], checks: &serde_json::Map<String, Value>, verdict: &Value) -> Vec<Value> {
    let judged = |id: &str| verdict["criteria"].as_array().into_iter().flatten().find(|v| v["id"] == id).cloned().unwrap_or(Value::Null);
    criteria
        .iter()
        .map(|c| {
            let id = c["id"].as_str().unwrap_or("");
            let check = checks.get(id).cloned();
            let v = judged(id);
            let verifier = v["verdict"].as_str().unwrap_or("");
            let result = match (&check, verifier) {
                (Some(ch), _) if ch["ok"] != true => "fail",
                (_, "fail") => "fail",
                (Some(_), _) => "pass",
                (None, "pass") => "pass",
                _ => "uncertain",
            };
            json!({ "id": id, "text": c["text"], "result": result, "check": check, "verifier": verifier, "evidence": v["evidence"] })
        })
        .collect()
}

/// The verify tool's answer to the model.
pub fn render_results(results: &[Value], notes: Option<&str>, verifier: &str) -> String {
    let count = |r: &str| results.iter().filter(|x| x["result"] == r).count();
    let mut text = format!("Verification: {} passed, {} failed, {} uncertain (verifier {verifier}).\n", count("pass"), count("fail"), count("uncertain"));
    for r in results {
        let mark = match r["result"].as_str() {
            Some("pass") => "✓",
            Some("fail") => "✗",
            _ => "?",
        };
        text.push_str(&format!("{mark} [{}] {}", r["id"].as_str().unwrap_or(""), r["text"].as_str().unwrap_or("")));
        if r["check"]["ok"] == false {
            let exit = r["check"]["exit"].as_i64().map(|c| format!("exit {c}")).unwrap_or_else(|| "didn't run".into());
            let missing = if r["check"]["expect_found"] == false { ", expected text not found" } else { "" };
            let out = r["check"]["output"].as_str().map(|o| zen_proto::tail(o, 800)).unwrap_or_default();
            text.push_str(&format!(" — check failed ({exit}{missing}):\n{}", out.trim_end()));
        } else if let Some(e) = r["evidence"].as_str().filter(|e| !e.is_empty()) {
            text.push_str(&format!(" — {e}"));
        }
        text.push('\n');
    }
    if let Some(n) = notes.filter(|n| !n.trim().is_empty()) {
        text.push_str(&format!("\nVerifier's notes: {n}\n"));
    }
    text
}

async fn verify(app: &AppState, session: Uuid, workspace: &Path, who: &crate::agents::Agent, args: &Value) -> tools::ToolOutput {
    // A sandboxed agent's checks and diff run in its sandbox, as its bash does.
    let sandbox = who.sandbox();
    let err = |content: String| tools::ToolOutput::error(content);
    let goal = args["goal"].as_str().unwrap_or("").trim().to_string();
    let criteria = criteria_of(args);
    if goal.is_empty() || criteria.is_empty() || criteria.iter().any(|c| c["text"].as_str().is_none_or(|t| t.trim().is_empty())) {
        return err("verify needs a `goal` and at least one criterion with `text`.".into());
    }
    // Each check is kept by its criterion's id: two criteria with one id would hide one's result.
    if let Some(id) = duplicate_id(&criteria) {
        return err(format!("two criteria have the id `{id}`: give each its own (or leave ids out)."));
    }
    let dir: PathBuf = args["dir"].as_str().map(|d| tools::resolve(workspace, d)).unwrap_or_else(|| workspace.to_path_buf());
    if !dir.is_dir() {
        return err(format!("`{}` is not a directory", dir.display()));
    }
    let base = args["base"].as_str().filter(|b| !b.trim().is_empty()).unwrap_or("HEAD").to_string();
    app.emit(session, json!({ "type": "status", "text": format!("verifying: running {} check(s)", criteria.iter().filter(|c| c["run"].is_string()).count()) })).await;
    let mut checks = serde_json::Map::new();
    for c in &criteria {
        if let Some(run) = c["run"].as_str() {
            checks.insert(c["id"].as_str().unwrap_or("").to_string(), run_check(&dir, run, c["expect"].as_str(), session, sandbox.as_ref()).await);
        }
    }
    let any_failed = checks.values().any(|c| c["ok"] != true);
    let judgment = criteria.iter().any(|c| !c["run"].is_string());
    let wanted = args["fresh"] == true;
    // The verifier runs when it adds something: criteria only judgment can check, or when asked.
    // A failed command needs no verifier: the command's output is the evidence.
    let run_verifier = !any_failed && (judgment || wanted);
    let verifier = if run_verifier {
        "ran"
    } else if any_failed {
        "skipped: a command failed"
    } else {
        "skipped: every criterion is a passing command"
    };
    let verdict = if run_verifier {
        app.emit(session, json!({ "type": "status", "text": "verifying: a fresh verifier is reviewing the work" })).await;
        let crit: Vec<String> = criteria.iter().map(|c| format!("- [{}] {}", c["id"].as_str().unwrap_or(""), c["text"].as_str().unwrap_or(""))).collect();
        let prompt = format!(
            "Goal of the work:\n{goal}\n\nCriteria:\n{}\n\nCommand checks run by the kernel (criterion id -> result):\n{}\n\nThe worker's summary (a claim, not evidence):\n{}\n\nDiff from {base} in {}:\n{}",
            crit.join("\n"),
            serde_json::to_string_pretty(&Value::Object(checks.clone())).unwrap_or_default(),
            args["summary"].as_str().unwrap_or("(none)"),
            dir.display(),
            diff_from(&dir, &base, sandbox.as_ref()).await
        );
        match crate::run_child(app, session, VERIFIER, &prompt, &dir, crate::turns::Child::default()).await {
            Ok(child) => crate::tape::load(&app.db, child, &["verdict"]).await.ok().and_then(|b| b.last().map(|b| b.payload.clone())).unwrap_or(Value::Null),
            Err(e) => {
                tracing::warn!("verifier for {session}: {e:#}");
                Value::Null
            }
        }
    } else {
        Value::Null
    };
    let results = combine(&criteria, &checks, &verdict);
    let notes = verdict["notes"].as_str();
    let record = json!({ "goal": goal, "results": results, "notes": notes, "verifier": verifier, "dir": dir });
    if let Err(e) = tape::append(&app.db, session, "verification", &record).await {
        tracing::error!("recording a verification for {session}: {e:#}");
    }
    let failed = results.iter().any(|r| r["result"] == "fail");
    tools::ToolOutput::text(render_results(&results, notes, verifier), failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_files_are_recognized_however_they_are_named() {
        let home = crate::test_util::TestDir::new("prompt-files");
        std::fs::create_dir_all(home.join("agents/zenbot")).unwrap();
        std::fs::create_dir_all(home.join("global/skills/work/verify")).unwrap();
        std::fs::create_dir_all(home.join("global/tools")).unwrap();
        std::fs::write(home.join("agents/zenbot/SOUL.md"), "soul").unwrap();
        std::fs::write(home.join("USER.md"), "user").unwrap();
        std::os::unix::fs::symlink("agents/zenbot/SOUL.md", home.join("SOUL.md")).unwrap();
        std::os::unix::fs::symlink("global/skills", home.join("skills")).unwrap();
        let g = Guarded { home: home.to_path_buf(), wiki: home.join("global/wiki") };
        let ws = home.join("agents");
        let at = |name: &str, path: &str| protected_target(&g, &ws, name, &json!({ "path": path }));
        let prompt = |name: &str, path: &str| matches!(at(name, path), Some(Protected::PromptFile(_)));
        assert!(prompt("edit", &home.join("USER.md").display().to_string()));
        assert!(prompt("write", "../USER.md"), "relative with ..");
        assert!(prompt("edit", &home.join("SOUL.md").display().to_string()), "the old path is a symlink to the soul");
        assert!(prompt("write", "zenbot/IDENTITY.md"), "a prompt file that doesn't exist yet");
        assert!(at("read", "../USER.md").is_none(), "reading is fine");
        assert!(at("edit", "zenbot/notes.md").is_none());
        assert!(at("edit", "/home/nobody/USER.md").is_none());
        assert!(at("write", "../global/wiki/page.md").is_none());
        let record = |path: &str| matches!(at("write", path), Some(Protected::Record(_)));
        for p in ["../global/MEMORY.md", "../global/memory/2026-10-21-pilot.md", "../global/entities/people/ana.md", "../global/wiki/concepts/x.md"] {
            assert!(record(p), "{p}");
        }
        // Another agent's soul and identity are prompt files too (backed up); its settings are refused.
        std::fs::create_dir_all(home.join("agents/garry")).unwrap();
        assert!(prompt("edit", "garry/SOUL.md") && prompt("write", "garry/IDENTITY.md"));
        let settings = |path: &str| matches!(at("write", path), Some(Protected::AgentSettings(_)));
        assert!(settings("garry/agent.toml") && settings("zenbot/agent.toml") && settings("./garry/../garry/agent.toml"));
        assert!(!settings("garry/skills/agent.toml"), "only the agent's own settings file");
        assert!(at("write", "garry/roles.toml").is_none() && at("edit", "zenbot/roles.toml").is_none(), "an agent keeps its roles");
        assert!(at("read", "garry/agent.toml").is_none(), "reading is fine");
    }

    #[test]
    fn tool_schema_budget() {
        let mut tools = specs(None).as_array().unwrap().clone();
        // Include optional judge independently of process configuration.
        tools.retain(|tool| tool["name"] != "judge");
        tools.push(judge_spec());
        let bytes = serde_json::to_vec(&tools).unwrap().len();
        for tool in &tools {
            println!("{:>6} {}", serde_json::to_vec(tool).unwrap().len(), tool["name"].as_str().unwrap_or(""));
        }
        println!("Owner tool schemas including judge: {bytes} JSON bytes; approximately {} tokens at 4 bytes/token (estimate, not tokenizer measurement)", bytes.div_ceil(4));
        // A guard against regrowth (about 19 KB before a trim), plus script jobs
        // and the work-contract tool (about 1.2 KB). Raise only for deliberate capabilities.
        assert!(bytes <= 17_500, "tool schemas exceed the 17,500-byte budget: {bytes}");
    }

    #[test]
    fn verifier_sessions_get_their_own_small_tool_list() {
        let names = |v: Value| v.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(names(specs(Some(VERIFIER))), ["bash", "read", "submit_verdict"]);
        // What each kind is offered is exactly what it may call.
        for kind in [None, Some(CHANNEL), Some(VERIFIER), Some(crate::delegate::SUBAGENT), Some(crate::jobs::JOB)] {
            let offered = names(specs(kind));
            for t in names(specs(None)).into_iter().chain(["submit_verdict".to_string()]) {
                assert_eq!(refusal(kind, &t).is_none(), offered.contains(&t), "{kind:?} {t}");
            }
        }
        let all = names(specs(None));
        for retired in ["capture", "save_tool"] {
            assert!(!all.iter().any(|name| name == retired));
            for kind in [None, Some(VERIFIER), Some(crate::delegate::SUBAGENT), Some(crate::jobs::JOB)] {
                assert!(refusal(kind, retired).is_some());
            }
        }
        for t in ["bash", "read", "write", "edit", "history", "search", "ask", "remember", "brain", "web_search", "web_fetch", "find_skills", "load_skill", "save_skill", "find_tools", "load_tool", "call_tool", "verify", "delegate", "schedule"] {
            assert!(all.contains(&t.to_string()), "{t} offered");
        }
        assert!(!all.contains(&"move".to_string()) && !all.contains(&"propose_brief".to_string()));
        assert_eq!(specs(None), specs(None), "the same list every turn");
        assert_eq!(specs(Some(CHANNEL)), specs(None), "a chat room's session is the owner's: all the tools");
        assert!(owner_driven(Some(CHANNEL)) && !owner_driven(Some(crate::jobs::JOB)));
        assert!(read_only(Some(VERIFIER)) && !read_only(None));
        // A kind the kernel doesn't know gets nothing.
        assert_eq!(specs(Some("mystery")), json!([]));
        assert!(refusal(Some("mystery"), "read").is_some());
        for t in ["ask", "delegate", "schedule", "submit_verdict"] {
            assert!(refusal(Some(crate::delegate::SUBAGENT), t).is_some() && refusal(Some(crate::jobs::JOB), t).is_some(), "{t}");
        }
    }

    #[test]
    fn a_failed_command_beats_the_verifier_and_judgment_needs_a_verdict() {
        let criteria = criteria_of(&json!({ "criteria": [{ "text": "tests pass", "run": "cargo test" }, "docs updated", { "id": "x", "text": "fast", "run": "true" }] }));
        assert_eq!(criteria[1]["id"], "c2");
        assert_eq!(duplicate_id(&criteria), None);
        // A failing check under an id another criterion also has would be overwritten: refused.
        let twice = criteria_of(&json!({ "criteria": [{ "id": "a", "text": "x", "run": "false" }, { "id": "a", "text": "y", "run": "true" }] }));
        assert_eq!(duplicate_id(&twice).as_deref(), Some("a"));
        let clash = criteria_of(&json!({ "criteria": ["first", { "id": "c1", "text": "named like the first" }] }));
        assert_eq!(duplicate_id(&clash).as_deref(), Some("c1"));
        let mut checks = serde_json::Map::new();
        checks.insert("c1".into(), json!({ "ok": false, "exit": 1, "output": "1 failed" }));
        checks.insert("x".into(), json!({ "ok": true }));
        let verdict = json!({ "criteria": [{ "id": "c1", "verdict": "pass" }, { "id": "c2", "verdict": "pass", "evidence": "README changed" }] });
        let r = combine(&criteria, &checks, &verdict);
        let results: Vec<&str> = r.iter().map(|x| x["result"].as_str().unwrap()).collect();
        assert_eq!(results, ["fail", "pass", "pass"]);
        let r = combine(&criteria, &checks, &Value::Null);
        assert_eq!(r[1]["result"], "uncertain");
        let text = render_results(&r, Some("scope ok"), "ran");
        assert!(text.contains("1 passed, 1 failed, 1 uncertain") && text.contains("✗ [c1] tests pass — check failed (exit 1)") && text.contains("scope ok"));
    }
}
