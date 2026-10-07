//! The agent's tools beyond files and the shell (DESIGN.md, "System tools"; D-026, D-027): `ask`,
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
use sqlx::PgPool;
use uuid::Uuid;

use crate::{tape, tools, AppState};

/// The kind of a child session that checks another session's work.
pub const VERIFIER: &str = "verifier";
const VERIFY_PROMPT: &str = include_str!("../steps/verify.md");

/// Whether a session of this kind may only read (a verifier: writes refused, `bash` in a
/// read-only sandbox).
pub fn read_only(kind: Option<&str>) -> bool {
    kind == Some(VERIFIER)
}

fn spec(name: &str, description: &str, parameters: Value) -> Value {
    json!({ "name": name, "description": description, "parameters": parameters })
}

fn ask_spec() -> Value {
    spec(
        "ask",
        "Ask the owner up to 3 questions only they can answer, then end your turn; their answers arrive as the next message. \
Ask about taste, the bet, or anything public, security-sensitive, costly or hard to reverse, or a goal you can't infer; \
never what you can look up or decide yourself. Each question has 2 to 4 options, your recommended one first. \
If a question goes unanswered, take your recommended option and say it was an assumption.",
        json!({ "type": "object", "properties": { "questions": { "type": "array", "description": "1 to 3 questions",
            "items": { "type": "object", "properties": {
                "question": { "type": "string" },
                "options": { "type": "array", "items": { "type": "string" }, "description": "2 to 4 options, recommended first" } },
              "required": ["question", "options"] } } }, "required": ["questions"] }),
    )
}

fn verify_spec() -> Value {
    spec(
        "verify",
        "Get a job's result checked before you report it done. The kernel runs each criterion's `run` command itself (in `dir`), \
then, when some criteria need judgment (or you ask with fresh=true), a fresh verifier that never sees your reasoning reads the \
diff and the files and judges them; it can't change anything. Returns pass, fail or uncertain per criterion with evidence. \
Use it for work whose correctness isn't settled by commands you already ran, or that is risky or architectural; skip it when \
every criterion is a command that passes. The work/verify skill says how to use it well. Takes minutes when a verifier runs.",
        json!({ "type": "object", "properties": {
            "goal": { "type": "string", "description": "What the job had to achieve, in one or two lines" },
            "criteria": { "type": "array", "description": "What must be true for the job to be done", "items": { "type": "object", "properties": {
                "id": { "type": "string" }, "text": { "type": "string", "description": "The criterion" },
                "run": { "type": "string", "description": "A shell command that checks it (exit 0 = pass)" },
                "expect": { "type": "string", "description": "Text the command's output must contain" } }, "required": ["text"] } },
            "dir": { "type": "string", "description": "The repository or directory the work changed (default: the workspace)" },
            "base": { "type": "string", "description": "The git commit the work started from, if you committed along the way (default HEAD: uncommitted changes)" },
            "summary": { "type": "string", "description": "What you did and how you checked it (a claim the verifier weighs, not evidence)" },
            "fresh": { "type": "boolean", "description": "Run the fresh verifier even when every criterion is a command" } },
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

fn decide_spec() -> Value {
    spec(
        "decide",
        "Ask a fast, cheap System One model typed questions and get answers with probabilities in about a second. Use it \
whenever the answer is one of known options and there's a lot to judge, or a cheap second opinion helps: triage 200 search \
results or log lines for relevance, rank candidates, classify files, check whether a text claims success without evidence. \
Not for reasoning, writing or anything you'd need to explain. Put the material in `state` (keep it under ~20,000 characters; \
for many items, give a list and ask per item or ask which ones). `questions` maps a key to {type: choice, instructions, \
criteria: {option: description}}, {type: score, instructions, criteria: [lowest, …, highest]} or {type: bool, instructions, \
criteria: {true: description, false: description}}. Answers: choice → {choice, probabilities, confidence}; score → {score \
(level index), confidence}; bool → {probability}. Set your own threshold, e.g. keep items above 0.8.",
        json!({ "type": "object", "properties": {
            "state": { "type": "object", "description": "The material to judge, as a JSON object" },
            "questions": { "type": "object", "description": "Typed questions by key" } }, "required": ["state", "questions"] }),
    )
}

/// Whether the `decide` tool is offered: a System One model is configured and ZEN_DECIDE_TOOL isn't 0.
fn decide_on() -> bool {
    std::env::var("ZEN_DECIDE_TOOL").map(|v| v.trim() != "0").unwrap_or(true) && crate::score::scorer().is_some()
}

/// The tools a session of this kind is offered, in a fixed order.
pub fn specs(kind: Option<&str>) -> Value {
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
    let mut all = builtins;
    all.push(crate::compact::tool_spec());
    all.push(ask_spec());
    all.push(crate::memory::spec());
    all.push(crate::web::search_spec());
    all.push(crate::web::fetch_spec());
    all.push(crate::skills::find_spec());
    all.push(crate::skills::load_spec());
    all.push(crate::mcp::find_spec());
    all.push(crate::mcp::load_spec());
    all.push(crate::mcp::call_spec());
    all.push(verify_spec());
    if decide_on() {
        all.push(decide_spec());
    }
    Value::Array(all)
}

/// The instructions of a session of this kind: the session's base prompt, or the verifier's own.
pub fn system_for(base: &str, kind: Option<&str>) -> String {
    if read_only(kind) {
        VERIFY_PROMPT.trim_end().to_string()
    } else {
        base.to_string()
    }
}

/// Run one of the agent's tools (anything not a file or shell built-in). None for other tools.
/// `ending` is set when the tool ends the model's turn (further calls are refused).
pub async fn run_tool(app: &AppState, session: Uuid, workspace: &Path, name: &str, args: &Value, ending: &mut Option<&'static str>) -> Option<tools::ToolOutput> {
    let out = |content: String, is_error: bool| Some(tools::ToolOutput { content, is_error });
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
        "remember" => {
            let (content, is_error) = crate::memory::run_tool(app, session, args).await;
            out(content, is_error)
        }
        "find_skills" | "load_skill" => {
            let (content, is_error) = crate::skills::run_tool(name, args)?;
            out(content, is_error)
        }
        "verify" => Some(verify(app, session, workspace, args).await),
        "web_search" | "web_fetch" => crate::web::run_tool(app, session, name, args).await,
        "find_tools" | "load_tool" | "call_tool" => crate::mcp::run_tool(app, session, name, args).await,
        "decide" => {
            let res = crate::score::decide(app, &args["state"], &args["questions"]).await;
            let (answer, error) = match &res {
                Ok(v) => (v.clone(), v["error"].as_str().map(String::from)),
                Err(e) => (Value::Null, Some(format!("{e:#}"))),
            };
            log_decision(&app.db, Some(session), "tool", &json!({ "questions": args["questions"] }), &answer, None, None, false, error.as_deref()).await;
            match error {
                Some(e) => out(format!("decide failed: {e}"), true),
                None => out(serde_json::to_string_pretty(&answer["answers"]).unwrap_or_default(), false),
            }
        }
        _ => None,
    }
}

/// Why a tool call is refused, if it is: nothing more once the model ended its turn with a tool
/// (a question to the owner, a verdict).
pub fn refuse(ending: Option<&str>) -> Option<String> {
    ending.map(|e| format!("You already ended this turn ({e}). End your turn now, without further tool calls."))
}

/// Record a System One decision (`decisions`): what was asked, the answer and its probability,
/// whether it was acted on.
#[allow(clippy::too_many_arguments)]
pub async fn log_decision(db: &PgPool, session: Option<Uuid>, point: &str, input: &Value, answer: &Value, chosen: Option<&str>, probability: Option<f64>, acted: bool, error: Option<&str>) {
    let r = sqlx::query(
        "INSERT INTO decisions (session_id, point, model, input, answer, chosen, probability, acted, error) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(session)
    .bind(point)
    .bind(crate::score::scorer())
    .bind(input)
    .bind(answer)
    .bind(chosen)
    .bind(probability)
    .bind(acted)
    .bind(error)
    .execute(db)
    .await;
    if let Err(e) = r {
        tracing::warn!("logging a {point} decision: {e}");
    }
}

// ---------- verify ----------

/// A criterion's check, run by the kernel like the bash tool (its own process group, killed on a
/// timeout or an abort, output capped and masked).
async fn run_check(dir: &Path, run: &str, expect: Option<&str>) -> Value {
    match tools::run_shell(dir, run, &[], false, Duration::from_secs(600)).await {
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
async fn diff_from(repo: &Path, base: &str) -> String {
    const CAP: usize = 60_000;
    if crate::git::git(repo, &["rev-parse", "--git-dir"]).await.is_none() {
        return "(not a git repository: no diff; read the files)".into();
    }
    // Read a little past the cap, so a secret at the cut is still whole when it is masked.
    let read = |out: Option<(bool, String)>| out.map(|(_, text)| text).unwrap_or_default();
    let mut d = read(crate::git::output(repo, &["diff", base], CAP + 4096).await);
    let untracked = read(crate::git::output(repo, &["ls-files", "--others", "--exclude-standard"], CAP + 4096).await);
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

async fn verify(app: &AppState, session: Uuid, workspace: &Path, args: &Value) -> tools::ToolOutput {
    let err = |content: String| tools::ToolOutput { content, is_error: true };
    let goal = args["goal"].as_str().unwrap_or("").trim().to_string();
    let criteria = criteria_of(args);
    if goal.is_empty() || criteria.is_empty() || criteria.iter().any(|c| c["text"].as_str().is_none_or(|t| t.trim().is_empty())) {
        return err("verify needs a `goal` and at least one criterion with `text`.".into());
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
            checks.insert(c["id"].as_str().unwrap_or("").to_string(), run_check(&dir, run, c["expect"].as_str()).await);
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
            diff_from(&dir, &base).await
        );
        match crate::run_child(app, session, VERIFIER, &prompt, &dir).await {
            Ok(v) => v,
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
    tools::ToolOutput { content: render_results(&results, notes, verifier), is_error: failed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_sessions_get_their_own_small_tool_list() {
        let names = |v: Value| v.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(names(specs(Some(VERIFIER))), ["bash", "read", "submit_verdict"]);
        let all = names(specs(None));
        for t in ["bash", "read", "write", "edit", "history", "ask", "remember", "web_search", "web_fetch", "find_skills", "load_skill", "find_tools", "load_tool", "call_tool", "verify"] {
            assert!(all.contains(&t.to_string()), "{t} offered");
        }
        assert!(!all.contains(&"move".to_string()) && !all.contains(&"propose_brief".to_string()));
        assert_eq!(specs(None), specs(None), "the same list every turn");
        assert!(read_only(Some(VERIFIER)) && !read_only(None));
    }

    #[test]
    fn a_failed_command_beats_the_verifier_and_judgment_needs_a_verdict() {
        let criteria = criteria_of(&json!({ "criteria": [{ "text": "tests pass", "run": "cargo test" }, "docs updated", { "id": "x", "text": "fast", "run": "true" }] }));
        assert_eq!(criteria[1]["id"], "c2");
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
