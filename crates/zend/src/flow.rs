//! Briefed work (docs/brief.md): frame → approve → work → verify → report → close.
//!
//! A session is one job. It starts in `framing`: read-only (writes refused, `bash` in a read-only
//! sandbox), where the model answers directly or proposes a brief. An approved brief (by the owner,
//! or automatically per route) starts the work: in the same context for small work, in a fresh one
//! with the brief in the instructions for architectural work. `submit_work` starts
//! verification: the kernel runs the criteria's commands, a fresh verifier (a child session) judges
//! the rest, failures go back to work a bounded number of times, then the session is reported and,
//! where allowed, closed with the model's verdict (the owner's always replaces it).
//!
//! The gates that matter are enforced here, not asked for in prompts: which tools exist in each
//! state, the brief's schema, that work starts only after approval, and the criteria checks.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{tape, tools, App, AppState};

const FRAME: &str = include_str!("../steps/frame.md");
const WORK: &str = include_str!("../steps/work.md");
const VERIFY: &str = include_str!("../steps/verify.md");

pub const ROUTES: [&str; 3] = ["quick", "bounded", "architectural"];
pub const WORK_KINDS: [&str; 8] = ["understand", "shape", "bet", "build", "verify", "maintain", "reflect", "reach"];

/// Briefed work is on unless ZEN_BRIEFS=0 (then sessions are `open`: today's single loop).
pub fn enabled() -> bool {
    std::env::var("ZEN_BRIEFS").map(|v| v.trim() != "0").unwrap_or(true)
}

fn list_setting(key: &str, default: &str) -> Vec<String> {
    std::env::var(key).unwrap_or_else(|_| default.into()).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Routes whose briefs are approved without the owner (ZEN_AUTO_APPROVE, default quick,bounded; `all`).
pub fn auto_approve(route: &str) -> bool {
    let l = list_setting("ZEN_AUTO_APPROVE", "quick,bounded");
    l.iter().any(|r| r == route || r == "all")
}

/// Routes the model may close with its own verdict (ZEN_AUTO_CLOSE, default quick,bounded; `all`).
pub fn auto_close(route: &str) -> bool {
    let l = list_setting("ZEN_AUTO_CLOSE", "quick,bounded");
    l.iter().any(|r| r == route || r == "all")
}

/// Routes whose work starts in a fresh context with the brief in the instructions
/// (ZEN_FRESH_CONTEXT, default architectural). Others continue in the framing context: the files
/// already read stay cached, and the brief arrives as a message.
pub fn fresh_context(route: &str) -> bool {
    list_setting("ZEN_FRESH_CONTEXT", "architectural").iter().any(|r| r == route || r == "all")
}

/// Share of verifications where the model verifier runs even though every criterion is a passing
/// command (ZEN_VERIFY_SAMPLE, default 0.2), so its value keeps being measured.
fn verify_sample() -> f64 {
    std::env::var("ZEN_VERIFY_SAMPLE").ok().and_then(|v| v.parse().ok()).unwrap_or(0.2)
}

fn verify_rounds() -> i64 {
    std::env::var("ZEN_VERIFY_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2)
}

/// The state a new session starts in.
pub fn initial_state() -> &'static str {
    if enabled() {
        "framing"
    } else {
        "open"
    }
}

/// A session's state. Sessions from before briefed work (no state) are `open`.
pub async fn state(db: &PgPool, session: Uuid) -> Result<String, sqlx::Error> {
    let s: Option<Option<String>> = sqlx::query_scalar("SELECT state FROM sessions WHERE id = $1").bind(session).fetch_optional(db).await?;
    Ok(s.flatten().unwrap_or_else(|| "open".into()))
}

/// Move a session to a state: a `state` block on the tape (who and why), cached on the session.
/// `fresh` marks the start of a new work context (history before it isn't replayed).
pub async fn set_state(app: &App, session: Uuid, to: &str, by: &str, reason: &str, fresh: bool) -> Result<i32> {
    sqlx::query("UPDATE sessions SET state = $2 WHERE id = $1").bind(session).bind(to).execute(&app.db).await?;
    let (seq, _) = tape::append(&app.db, session, "state", &json!({ "state": to, "by": by, "reason": reason, "fresh": fresh })).await?;
    app.emit(session, json!({ "type": "state", "state": to, "by": by, "reason": reason })).await;
    Ok(seq)
}

// ---------- tools per state ----------

fn spec(name: &str, description: &str, parameters: Value) -> Value {
    json!({ "name": name, "description": description, "parameters": parameters })
}

fn ask_spec() -> Value {
    spec(
        "ask",
        "Ask the owner up to 3 questions that block the work, then end your turn; the answers arrive as the next message. \
Each question has 2 to 4 options, your recommended one first. If a question goes unanswered, take your recommended option and record it as an assumption.",
        json!({ "type": "object", "properties": { "questions": { "type": "array", "description": "1 to 3 questions",
            "items": { "type": "object", "properties": {
                "question": { "type": "string" },
                "options": { "type": "array", "items": { "type": "string" }, "description": "2 to 4 options, recommended first" } },
                "required": ["question", "options"] } } }, "required": ["questions"] }),
    )
}

fn brief_spec() -> Value {
    let list = |d: &str| json!({ "type": "array", "items": { "type": "string" }, "description": d });
    spec(
        "propose_brief",
        "Propose the brief for this change (about 1,000 tokens), then end your turn. The kernel checks it; the owner approves it, or it is \
approved automatically for some routes. Criteria with `run` are shell commands the kernel runs after the work (from context.repo); \
`expect` is text the output must contain.",
        json!({ "type": "object", "properties": {
            "route": { "type": "string", "description": "quick, bounded or architectural" },
            "work": { "type": "string", "description": "understand, shape, bet, build, verify, maintain, reflect or reach" },
            "intent": { "type": "object", "properties": { "stated": { "type": "string", "description": "What the owner said, quoted" },
                "assumed": list("What you inferred") }, "required": ["stated"] },
            "goal": { "type": "string", "description": "Current state -> target state, in one or two lines" },
            "scope": { "type": "object", "properties": { "in": list("In scope"), "out": list("Out of scope, each with why") } },
            "must_not": list("Boundaries the work must not cross"),
            "questions": { "type": "array", "items": { "type": "object", "properties": { "q": { "type": "string" }, "a": { "type": "string" } } } },
            "assumptions": list("Defaults taken instead of asking"),
            "context": { "type": "object", "properties": { "repo": { "type": "string", "description": "Repository path the work changes" },
                "files": list("Files to load first") } },
            "criteria": { "type": "array", "items": { "type": "object", "properties": {
                "id": { "type": "string" }, "text": { "type": "string" },
                "run": { "type": "string", "description": "Shell command that checks it" },
                "expect": { "type": "string", "description": "Text the command's output must contain" } }, "required": ["id", "text"] } },
            "appetite": { "type": "object", "properties": { "time": { "type": "string" }, "cost_usd": { "type": "number" } } } },
          "required": ["route", "work", "intent", "goal", "criteria"] }),
    )
}

fn ruling_spec() -> Value {
    spec(
        "note_ruling",
        "Record a decision you made on an unclear point instead of stopping to ask. Shown to the owner in the report.",
        json!({ "type": "object", "properties": { "what": { "type": "string" }, "why": { "type": "string" },
            "cost_if_wrong": { "type": "string" } }, "required": ["what", "why"] }),
    )
}

fn submit_spec() -> Value {
    spec(
        "submit_work",
        "Submit the work for verification when you believe the brief's criteria pass, then end your turn.",
        json!({ "type": "object", "properties": { "summary": { "type": "string", "description": "What you changed and how you checked it" } }, "required": ["summary"] }),
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
            "notes": { "type": "string", "description": "Scope or must-not problems, anything else the owner should know" } },
          "required": ["criteria"] }),
    )
}

fn decide_spec() -> Value {
    spec(
        "decide",
        "Ask a fast System One model typed questions about some text: classify, score or answer yes/no, with probabilities. \
Good for bulk triage or ranking and cheap second opinions; not for reasoning. `questions` maps a key to \
{type: choice, instructions, criteria: {option: description}} or {type: score, instructions, criteria: [lowest, …, highest]} \
or {type: bool, instructions, criteria: {true: description, false: description}}.",
        json!({ "type": "object", "properties": {
            "state": { "type": "object", "description": "The material to judge, as a JSON object (keep it under ~20,000 characters)" },
            "questions": { "type": "object", "description": "Typed questions by key" } }, "required": ["state", "questions"] }),
    )
}

fn decide_tool_on() -> bool {
    std::env::var("ZEN_DECIDE_TOOL").map(|v| v.trim() != "0").unwrap_or(true) && crate::score::scorer().is_some()
}

/// States where the shell is read-only and nothing can be written.
pub fn read_only(state: &str) -> bool {
    matches!(state, "framing" | "verifier")
}

/// The tools the model is offered: one list for every phase (so a session's instructions and tools
/// never change, and its prompt cache holds across phases), the verifier's own small list. What may
/// run in each phase is enforced when a tool runs (`refuse`).
pub fn tools_for(state: &str) -> Value {
    let base = tools::specs();
    let mut picked: Vec<Value> = if state == "verifier" {
        base.as_array().into_iter().flatten().filter(|t| matches!(t["name"].as_str(), Some("read" | "bash"))).cloned().collect()
    } else {
        base.as_array().cloned().unwrap_or_default()
    };
    for t in picked.iter_mut().filter(|t| t["name"] == "bash") {
        let d = t["description"].as_str().unwrap_or("").to_string();
        t["description"] = json!(format!("{d} While framing (and for a verifier) the filesystem is read-only: writes fail."));
    }
    if state == "verifier" {
        picked.push(verdict_spec());
    } else {
        picked.extend([ask_spec(), brief_spec(), ruling_spec(), submit_spec()]);
        if decide_tool_on() {
            picked.push(decide_spec());
        }
    }
    Value::Array(picked)
}

/// The tools that may run in a state.
fn allowed(state: &str, name: &str) -> bool {
    match state {
        "framing" => matches!(name, "read" | "bash" | "history" | "ask" | "propose_brief" | "decide"),
        "working" => !matches!(name, "propose_brief" | "submit_verdict"),
        "verifier" => matches!(name, "read" | "bash" | "submit_verdict"),
        _ => !matches!(name, "propose_brief" | "submit_work" | "submit_verdict"),
    }
}

/// The instructions: the session's base prompt plus both procedures (the turn context says which
/// phase applies), the same in every phase so the cache holds. Work in a fresh context
/// (`fresh_context`) also carries its brief here; the verifier has its own.
pub fn system_for(base: &str, state: &str, fresh_brief: Option<&Value>) -> String {
    match (state, fresh_brief) {
        ("open", _) => base.to_string(),
        ("verifier", _) => VERIFY.trim_end().to_string(),
        ("working" | "verifying" | "reported", Some(b)) => format!(
            "{base}\n\n{}\n\n{}\n\n<brief version=\"{}\">\n{}\n</brief>",
            FRAME.trim_end(),
            WORK.trim_end(),
            b["version"],
            render_brief(&b["brief"])
        ),
        _ => format!("{base}\n\n{}\n\n{}", FRAME.trim_end(), WORK.trim_end()),
    }
}

/// The phase line of the turn context (compile::turn_context), so the model knows what applies.
pub fn phase_line(state: &str, brief_version: Option<i64>) -> Option<String> {
    match state {
        "framing" => Some("Phase: framing. Nothing can be changed: answer, ask, or propose a brief.".into()),
        "working" => Some(format!("Phase: working on brief v{}.", brief_version.unwrap_or(0))),
        _ => None,
    }
}

/// The approved brief when its work runs in a fresh context (it is then in the instructions).
pub fn fresh_brief_in(blocks: &[tape::Block]) -> Option<Value> {
    let a = blocks.iter().rev().find(|b| b.kind == "approval")?;
    if a.payload["fresh"] != true {
        return None;
    }
    blocks.iter().rev().find(|b| b.kind == "brief" && b.payload["version"] == a.payload["version"]).map(|b| b.payload.clone())
}

/// Why a tool call is refused in this state, if it is.
pub fn refuse(state: &str, name: &str, ending: Option<&str>) -> Option<String> {
    if let Some(e) = ending {
        return Some(format!("You already ended this step ({e}). End your turn now, without further tool calls."));
    }
    if allowed(state, name) {
        return None;
    }
    Some(match state {
        "framing" => format!("`{name}` isn't available while framing: nothing can be changed yet. Propose a brief; the work starts once it is approved."),
        "verifier" => format!("`{name}` isn't available to the verifier."),
        _ => format!("`{name}` isn't available now ({state})."),
    })
}

// ---------- the brief ----------

/// Check a proposed brief. Returns the problems, if any.
pub fn validate_brief(b: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    let s = |k: &str| b[k].as_str().map(str::trim).filter(|v| !v.is_empty());
    match s("route") {
        Some(r) if ROUTES.contains(&r) => {}
        _ => errs.push("route must be quick, bounded or architectural".into()),
    }
    match s("work") {
        Some(w) if WORK_KINDS.contains(&w) => {}
        _ => errs.push(format!("work must be one of {}", WORK_KINDS.join(", "))),
    }
    if b["intent"]["stated"].as_str().map(str::trim).unwrap_or("").is_empty() {
        errs.push("intent.stated must quote what the owner asked".into());
    }
    if s("goal").is_none() {
        errs.push("goal must say current -> target".into());
    }
    let criteria = b["criteria"].as_array().cloned().unwrap_or_default();
    if criteria.is_empty() && s("route") != Some("quick") {
        errs.push("criteria: give at least one, as a command with expected output where possible".into());
    }
    for (i, c) in criteria.iter().enumerate() {
        if c["id"].as_str().unwrap_or("").is_empty() || c["text"].as_str().unwrap_or("").is_empty() {
            errs.push(format!("criteria[{i}] needs an id and a text"));
        }
    }
    let size = b.to_string().len();
    if size > 8000 {
        errs.push(format!("the brief is {size} characters (about {} tokens); keep it near 1,000 tokens: cut detail or split the work", size / 4));
    }
    errs
}

/// The brief as the model and the owner read it.
pub fn render_brief(b: &Value) -> String {
    let strs = |v: &Value| -> Vec<String> { v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect() };
    let mut out = format!(
        "Route: {} · Work: {}\nGoal: {}\n\nIntent (stated): {}\n",
        b["route"].as_str().unwrap_or(""),
        b["work"].as_str().unwrap_or(""),
        b["goal"].as_str().unwrap_or(""),
        b["intent"]["stated"].as_str().unwrap_or("")
    );
    let mut section = |title: &str, items: Vec<String>| {
        if !items.is_empty() {
            out.push_str(&format!("\n{title}:\n{}\n", items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n")));
        }
    };
    section("Intent (assumed)", strs(&b["intent"]["assumed"]));
    section("In scope", strs(&b["scope"]["in"]));
    section("Out of scope", strs(&b["scope"]["out"]));
    section("Must not", strs(&b["must_not"]));
    section(
        "Questions",
        b["questions"].as_array().into_iter().flatten().map(|q| format!("{} → {}", q["q"].as_str().unwrap_or(""), q["a"].as_str().unwrap_or(""))).collect(),
    );
    section("Assumptions", strs(&b["assumptions"]));
    let mut ctx = strs(&b["context"]["files"]);
    if let Some(r) = b["context"]["repo"].as_str() {
        ctx.insert(0, format!("repo: {r}"));
    }
    section("Context", ctx);
    section(
        "Criteria",
        b["criteria"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let mut line = format!("[{}] {}", c["id"].as_str().unwrap_or(""), c["text"].as_str().unwrap_or(""));
                if let Some(r) = c["run"].as_str() {
                    line.push_str(&format!("\n  run: `{r}`"));
                }
                if let Some(e) = c["expect"].as_str() {
                    line.push_str(&format!("\n  expect: {e}"));
                }
                line
            })
            .collect(),
    );
    if b["appetite"].is_object() {
        section("Appetite", vec![b["appetite"].to_string()]);
    }
    out.trim_end().to_string()
}

/// The newest brief block, and whether it has been approved since.
pub async fn latest_brief(db: &PgPool, session: Uuid) -> Result<Option<(Value, bool)>> {
    Ok(brief_in(&tape::load(db, session, &["brief", "approval"]).await?))
}

/// The newest brief among a session's blocks, and whether it has been approved since.
pub fn brief_in(blocks: &[tape::Block]) -> Option<(Value, bool)> {
    let brief = blocks.iter().rev().find(|b| b.kind == "brief")?;
    let approved = blocks.iter().any(|b| b.kind == "approval" && b.seq > brief.seq);
    Some((brief.payload.clone(), approved))
}

// ---------- the workflow's tools ----------

/// Run one of the workflow's tools. Returns None for other tools. `ending` is set when the tool
/// ends the model's step (the kernel refuses further calls in the turn).
pub async fn run_tool(app: &AppState, session: Uuid, name: &str, args: &Value, ending: &mut Option<&'static str>) -> Option<tools::ToolOutput> {
    let out = |content: String, is_error: bool| Some(tools::ToolOutput { content, is_error });
    match name {
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
If a question goes unanswered, take your recommended option and record it as an assumption."
                .into(), false)
        }
        "propose_brief" => {
            let errs = validate_brief(args);
            if !errs.is_empty() {
                return out(format!("The brief wasn't accepted:\n- {}\nFix these and call propose_brief again.", errs.join("\n- ")), true);
            }
            let version = match tape::load(&app.db, session, &["brief"]).await {
                Ok(b) => b.len() as i64 + 1,
                Err(e) => return out(format!("couldn't read earlier briefs: {e}"), true),
            };
            let payload = json!({ "version": version, "brief": args });
            if let Err(e) = tape::append(&app.db, session, "brief", &payload).await {
                return out(format!("couldn't record the brief: {e}"), true);
            }
            resolve_shadow(&app.db, session, "route", args["route"].as_str(), "model").await;
            resolve_shadow(&app.db, session, "work", args["work"].as_str(), "model").await;
            app.emit(session, json!({ "type": "brief", "version": version, "brief": args, "text": render_brief(args) })).await;
            *ending = Some("brief proposed");
            let route = args["route"].as_str().unwrap_or("");
            let next = if auto_approve(route) { "it is approved automatically" } else { "the owner approves it" };
            let context = if fresh_context(route) { "in a fresh context with the brief in the instructions" } else { "here, with the brief as a message" };
            out(format!("Brief v{version} recorded; {next} and the work continues {context}. End your turn now with one line."), false)
        }
        "note_ruling" => {
            let r = json!({ "what": args["what"], "why": args["why"], "cost_if_wrong": args["cost_if_wrong"] });
            match tape::append(&app.db, session, "ruling", &r).await {
                Ok(_) => out("Recorded.".into(), false),
                Err(e) => out(format!("couldn't record the ruling: {e}"), true),
            }
        }
        "submit_work" => {
            let summary = args["summary"].as_str().unwrap_or("").to_string();
            if let Err(e) = tape::append(&app.db, session, "submission", &json!({ "summary": summary })).await {
                return out(format!("couldn't record the submission: {e}"), true);
            }
            shadow_claim(app, session, &summary).await;
            *ending = Some("work submitted");
            out("Submitted. End your turn now: the kernel runs the criteria and a fresh verifier reviews the work.".into(), false)
        }
        "submit_verdict" => {
            if let Err(e) = tape::append(&app.db, session, "verdict", args).await {
                return out(format!("couldn't record the verdict: {e}"), true);
            }
            *ending = Some("verdict given");
            out("Verdict recorded. End your turn now.".into(), false)
        }
        "decide" => {
            let res = s1(app, &args["state"], &args["questions"]).await;
            let (answer, error) = match &res {
                Ok(v) => (v.clone(), v["error"].as_str().map(String::from)),
                Err(e) => (Value::Null, Some(format!("{e:#}"))),
            };
            log_decision(&app.db, session, "tool", &json!({ "questions": args["questions"] }), &answer, None, None, false, error.as_deref()).await;
            match error {
                Some(e) => out(format!("decide failed: {e}"), true),
                None => out(serde_json::to_string_pretty(&answer["answers"]).unwrap_or_default(), false),
            }
        }
        _ => None,
    }
}

// ---------- System One ----------

/// Ask the configured System One model (ZEN_S1_MODEL) typed questions.
async fn s1(app: &App, state: &Value, questions: &Value) -> Result<Value> {
    let model = crate::score::scorer().context("no System One model is configured (ZEN_S1_MODEL)")?;
    let w = crate::worker_for_classifier(app, &model).await.with_context(|| format!("no worker serves `{model}`"))?;
    app.workers[w].mind().request("s1.decide", json!({ "model": model, "state": state, "questions": questions })).await
}

#[allow(clippy::too_many_arguments)]
async fn log_decision(db: &PgPool, session: Uuid, point: &str, input: &Value, answer: &Value, chosen: Option<&str>, probability: Option<f64>, acted: bool, error: Option<&str>) {
    let model = crate::score::scorer();
    let r = sqlx::query(
        "INSERT INTO decisions (session_id, point, model, input, answer, chosen, probability, acted, error) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(session)
    .bind(point)
    .bind(model)
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

/// Record what actually happened for a shadow decision (the model's route, the owner's override).
pub async fn resolve_shadow(db: &PgPool, session: Uuid, point: &str, actual: Option<&str>, by: &str) {
    let Some(actual) = actual else { return };
    let _ = sqlx::query(
        "UPDATE decisions SET actual = $3, actual_by = $4, resolved_at = now()
         WHERE id = (SELECT id FROM decisions WHERE session_id = $1 AND point = $2 ORDER BY id DESC LIMIT 1)",
    )
    .bind(session)
    .bind(point)
    .bind(actual)
    .bind(by)
    .execute(db)
    .await;
}

/// Shadow decisions on a new request: its route and kind of work, logged, not acted on.
pub fn shadow_request(app: &AppState, session: Uuid, text: &str) {
    if crate::score::scorer().is_none() {
        return;
    }
    let (app, text) = (app.clone(), text.to_string());
    tokio::spawn(async move {
        let questions = json!({
            "route": { "type": "choice", "instructions": "How much framing does this request to an AI agent need before work starts?",
                "criteria": { "quick": "A question or a look: answer it directly, nothing to change",
                              "bounded": "A change with a clear shape",
                              "architectural": "Several approaches or a design to choose before changing anything" } },
            "work": crate::score::questions()["work"].clone(),
        });
        let state = json!({ "request": text.chars().take(8000).collect::<String>() });
        let res = s1(&app, &state, &questions).await;
        for point in ["route", "work"] {
            let (answer, error) = match &res {
                Ok(v) => (v["answers"][point].clone(), v["error"].as_str().map(String::from)),
                Err(e) => (Value::Null, Some(format!("{e:#}"))),
            };
            let chosen = answer["choice"].as_str();
            let p = chosen.and_then(|c| answer["probabilities"][c].as_f64()).or(answer["confidence"].as_f64());
            log_decision(&app.db, session, point, &state, &answer, chosen, p, false, error.as_deref()).await;
        }
        // The model may have proposed its brief before this answer came back.
        if let Ok(Some((b, _))) = latest_brief(&app.db, session).await {
            resolve_shadow(&app.db, session, "route", b["brief"]["route"].as_str(), "model").await;
            resolve_shadow(&app.db, session, "work", b["brief"]["work"].as_str(), "model").await;
        }
    });
}

/// Shadow triage at submission: does the summary claim success without evidence?
async fn shadow_claim(app: &AppState, session: Uuid, summary: &str) {
    if crate::score::scorer().is_none() {
        return;
    }
    let (app, summary) = (app.clone(), summary.to_string());
    tokio::spawn(async move {
        let questions = json!({ "claim": crate::score::questions()["unverified_claim"].clone() });
        let state = json!({ "agent_summary": summary });
        let res = s1(&app, &state, &questions).await;
        let (answer, error) = match &res {
            Ok(v) => (v["answers"]["claim"].clone(), v["error"].as_str().map(String::from)),
            Err(e) => (Value::Null, Some(format!("{e:#}"))),
        };
        let p = answer["probability"].as_f64();
        let chosen = p.map(|p| if p >= 0.5 { "unverified" } else { "evidenced" });
        log_decision(&app.db, session, "claim", &state, &answer, chosen, p, false, error.as_deref()).await;
        // Verification may have finished before this answer came back.
        if let Ok(blocks) = tape::load(&app.db, session, &["submission", "verification"]).await {
            let submitted = blocks.iter().rev().find(|b| b.kind == "submission").map(|b| b.seq).unwrap_or(0);
            if let Some(v) = blocks.iter().rev().find(|b| b.kind == "verification" && b.seq > submitted) {
                let all = v.payload["results"].as_array().is_some_and(|r| r.iter().all(|x| x["result"] == "pass"));
                resolve_shadow(&app.db, session, "claim", Some(if all { "evidenced" } else { "unverified" }), "verification").await;
            }
        }
    });
}

// ---------- routing policy ----------

/// The model and thinking level the policy names for a kind of work, with the policy's version.
async fn route_for(db: &PgPool, work: &str) -> Result<Option<(String, Option<String>, i32)>> {
    let row = sqlx::query("SELECT version, data FROM policies ORDER BY version DESC LIMIT 1").fetch_optional(db).await?;
    let Some(row) = row else { return Ok(None) };
    let data: Value = row.get("data");
    let r = data["routes"].get(work).or_else(|| data["routes"].get("*"));
    Ok(r.and_then(|r| r["model"].as_str().map(|m| (m.to_string(), r["effort"].as_str().map(String::from), row.get("version")))))
}

// ---------- approval and handoff ----------

/// Is an owner's message an approval of a waiting brief?
pub fn is_approval(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['.', '!']).to_lowercase();
    t.len() <= 30 && ["yes", "y", "go", "ok", "okay", "approve", "approved", "lgtm", "sim", "do it", "go ahead", "/go"].contains(&t.as_str())
}

/// The git state of a repository (the diff baseline), if it is one.
fn git_head(repo: &Path) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn repo_of(app: &App, brief: &Value) -> std::path::PathBuf {
    brief["context"]["repo"].as_str().map(|r| tools::resolve(&app.workspace, r)).unwrap_or_else(|| app.workspace.clone())
}

/// Approve the waiting brief and start the work: in the same context with the brief as a message,
/// or (architectural work) in a fresh context with the brief in the instructions, where the
/// framing chat isn't replayed (the history tool still reads it). The routing policy may pick the
/// model.
pub async fn approve(app: AppState, session: Uuid, by: &'static str) {
    app.background.lock().await.insert(session);
    let res = approve_inner(&app, session, by).await;
    app.background.lock().await.remove(&session);
    if let Err(e) = res {
        tracing::error!("approving the brief for {session}: {e:#}");
        app.emit(session, json!({ "type": "error", "error": format!("couldn't start the work: {e:#}") })).await;
        app.emit(session, json!({ "type": "idle", "state": "framing" })).await;
    }
}

async fn approve_inner(app: &AppState, session: Uuid, by: &str) -> Result<()> {
    let (brief, approved) = latest_brief(&app.db, session).await?.context("there is no brief to approve")?;
    anyhow::ensure!(!approved, "the latest brief is already approved");
    let repo = repo_of(app, &brief["brief"]);
    let head = git_head(&repo);
    let fresh = fresh_context(brief["brief"]["route"].as_str().unwrap_or(""));
    tape::append(&app.db, session, "approval", &json!({ "version": brief["version"], "by": by, "repo": repo, "head": head, "fresh": fresh })).await?;
    if by == "owner" {
        resolve_shadow(&app.db, session, "route", brief["brief"]["route"].as_str(), "model").await;
    }
    // The routing policy picks the model for this kind of work, if it names one.
    let work = brief["brief"]["work"].as_str().unwrap_or("");
    if let Some((model, effort, version)) = route_for(&app.db, work).await? {
        if crate::worker_for(app, &model).await.is_some() {
            sqlx::query("UPDATE sessions SET model = $2, effort = $3 WHERE id = $1").bind(session).bind(&model).bind(&effort).execute(&app.db).await?;
            log_decision(&app.db, session, "model", &json!({ "work": work, "policy": version }), &json!({ "model": model, "effort": effort }), Some(&model), None, true, None).await;
        }
    }
    set_state(app, session, "working", by, &format!("brief v{} approved", brief["version"]), fresh).await?;
    let message = if fresh {
        "The brief is approved. Do the work it describes; when its criteria should pass, call submit_work.".to_string()
    } else {
        format!(
            "Brief v{} is approved:\n<brief version=\"{}\">\n{}\n</brief>\nTreat it as the source of intent: its intent, goal, scope and must-nots are fixed. Do the work; when its criteria should pass, call submit_work.",
            brief["version"], brief["version"], render_brief(&brief["brief"])
        )
    };
    crate::start_kernel_turn(app, session, message).await
}

// ---------- verification ----------

/// A criterion's check, run by the kernel.
async fn run_check(dir: &Path, run: &str, expect: Option<&str>) -> Value {
    let cmd = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(run)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(600), cmd).await {
        Ok(Ok(o)) => {
            let text = crate::secrets::mask(&format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)));
            let tail: String = text.chars().rev().take(3000).collect::<Vec<_>>().into_iter().rev().collect();
            let found = expect.is_none_or(|e| text.contains(e));
            json!({ "ok": o.status.success() && found, "exit": o.status.code(), "expect_found": found, "output": tail })
        }
        Ok(Err(e)) => json!({ "ok": false, "output": format!("couldn't run it: {e}") }),
        Err(_) => json!({ "ok": false, "output": "timed out after 600s" }),
    }
}

/// The diff since the approval's baseline (and untracked files), capped.
fn diff_since(repo: &Path, head: Option<&str>) -> String {
    let git = |args: &[&str]| {
        std::process::Command::new("git").arg("-C").arg(repo).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    };
    let Some(head) = head else { return "(not a git repository: no diff)".into() };
    let mut d = git(&["diff", head]);
    let untracked = git(&["ls-files", "--others", "--exclude-standard"]);
    if !untracked.trim().is_empty() {
        d.push_str(&format!("\nUntracked files:\n{untracked}"));
    }
    let d = crate::secrets::mask(&d);
    if d.len() > 60_000 {
        let mut end = 60_000;
        while !d.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n[diff cut at 60 KB; read files for the rest]", &d[..end])
    } else if d.trim().is_empty() {
        "(no changes)".into()
    } else {
        d
    }
}

/// Verify the submitted work, then send it back to work or report it.
pub async fn verify(app: AppState, session: Uuid) {
    app.background.lock().await.insert(session);
    let res = verify_inner(&app, session).await;
    app.background.lock().await.remove(&session);
    if let Err(e) = res {
        tracing::error!("verifying session {session}: {e:#}");
        let _ = report(&app, session, &json!([]), None, Some(&format!("verification failed to run: {e:#}"))).await;
    }
}

async fn verify_inner(app: &AppState, session: Uuid) -> Result<()> {
    set_state(app, session, "verifying", "kernel", "work submitted", false).await?;
    let (brief, approved) = latest_brief(&app.db, session).await?.context("no brief to verify against")?;
    anyhow::ensure!(approved, "the brief was never approved");
    let blocks = tape::load(&app.db, session, &["approval", "ruling", "submission", "verification"]).await?;
    let approval = blocks.iter().rev().find(|b| b.kind == "approval").context("no approval")?;
    let since = approval.seq;
    let repo = approval.payload["repo"].as_str().map(std::path::PathBuf::from).unwrap_or_else(|| app.workspace.clone());
    let criteria = brief["brief"]["criteria"].as_array().cloned().unwrap_or_default();

    app.emit(session, json!({ "type": "status", "text": format!("verifying: running {} check(s)", criteria.iter().filter(|c| c["run"].is_string()).count()) })).await;
    let mut checks = serde_json::Map::new();
    for c in &criteria {
        if let Some(run) = c["run"].as_str() {
            checks.insert(c["id"].as_str().unwrap_or("").to_string(), run_check(&repo, run, c["expect"].as_str()).await);
        }
    }
    if criteria.is_empty() {
        tape::append(&app.db, session, "verification", &json!({ "round": 1, "results": [], "verifier": "skipped: no criteria" })).await?;
        return report(app, session, &json!([]), None, None).await;
    }
    let rulings: Vec<Value> = blocks.iter().filter(|b| b.kind == "ruling" && b.seq > since).map(|b| b.payload.clone()).collect();
    let summary = blocks.iter().rev().find(|b| b.kind == "submission").map(|b| b.payload["summary"].clone()).unwrap_or(Value::Null);
    let diff = diff_since(&repo, approval.payload["head"].as_str());

    // The model verifier runs when it adds something: criteria only judgment can check,
    // architectural work, or a sample (to keep measuring it). A failed command needs no verifier:
    // the work goes back with the command's output.
    let route = brief["brief"]["route"].as_str().unwrap_or("");
    let any_failed = checks.values().any(|c| c["ok"] != true);
    let judgment = criteria.iter().any(|c| !c["run"].is_string());
    let sampled = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) % 1000) as f64 / 1000.0 < verify_sample();
    let why = if any_failed {
        None
    } else if judgment {
        Some("criteria that need judgment")
    } else if route == "architectural" {
        Some("architectural work")
    } else if sampled {
        Some("sample")
    } else {
        None
    };
    let verifier = match why {
        Some(w) => format!("ran: {w}"),
        None if any_failed => "skipped: a command failed".into(),
        None => "skipped: every criterion is a passing command".into(),
    };
    // A fresh verifier: a child session that sees none of the work's history.
    if why.is_some() {
        app.emit(session, json!({ "type": "status", "text": "verifying: a fresh verifier is reviewing the work" })).await;
    }
    let prompt = format!(
        "Brief (v{}):\n{}\n\nCommand checks run by the kernel (criterion id -> result):\n{}\n\nWorker's summary (a claim, not evidence):\n{}\n\nRulings the worker made:\n{}\n\nDiff since the brief was approved, in {}:\n{}",
        brief["version"],
        render_brief(&brief["brief"]),
        serde_json::to_string_pretty(&Value::Object(checks.clone())).unwrap_or_default(),
        summary.as_str().unwrap_or("(none)"),
        serde_json::to_string_pretty(&rulings).unwrap_or_default(),
        repo.display(),
        diff
    );
    let verdict = if why.is_some() {
        crate::run_child(app, session, "verifier", &prompt, &repo).await.unwrap_or_else(|e| {
            tracing::warn!("verifier for {session}: {e:#}");
            Value::Null
        })
    } else {
        Value::Null
    };

    // A failed command can't be overridden; otherwise the verifier's judgment, or uncertain.
    let judged = |id: &str| verdict["criteria"].as_array().into_iter().flatten().find(|v| v["id"] == id).cloned().unwrap_or(Value::Null);
    let results: Vec<Value> = criteria
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
        .collect();
    let round = blocks.iter().filter(|b| b.kind == "verification" && b.seq > since).count() as i64 + 1;
    tape::append(&app.db, session, "verification", &json!({ "round": round, "results": results, "notes": verdict["notes"], "verifier": verifier })).await?;
    resolve_shadow(&app.db, session, "claim", Some(if results.iter().all(|r| r["result"] == "pass") { "evidenced" } else { "unverified" }), "verification").await;

    let failed: Vec<&Value> = results.iter().filter(|r| r["result"] == "fail").collect();
    if !failed.is_empty() && round <= verify_rounds() {
        let list: Vec<String> = failed
            .iter()
            .map(|r| {
                let why = r["evidence"].as_str().map(String::from).or_else(|| r["check"]["output"].as_str().map(|o| o.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect())).unwrap_or_default();
                format!("- [{}] {}: {}", r["id"].as_str().unwrap_or(""), r["text"].as_str().unwrap_or(""), why)
            })
            .collect();
        let notes = verdict["notes"].as_str().map(|n| format!("\nVerifier's notes: {n}")).unwrap_or_default();
        set_state(app, session, "working", "kernel", &format!("verification round {round} failed"), false).await?;
        return crate::start_kernel_turn(
            app,
            session,
            format!("Verification round {round} found problems:\n{}{notes}\nFix them, then call submit_work again.", list.join("\n")),
        )
        .await;
    }
    report(app, session, &Value::Array(results), verdict["notes"].as_str(), None).await
}

// ---------- report and close ----------

/// Report the session's work and, where the route allows, close it with the model's verdict.
async fn report(app: &AppState, session: Uuid, results: &Value, notes: Option<&str>, error: Option<&str>) -> Result<()> {
    let brief = latest_brief(&app.db, session).await?.map(|(b, _)| b).unwrap_or(Value::Null);
    let blocks = tape::load(&app.db, session, &["approval", "ruling"]).await?;
    let since = blocks.iter().rev().find(|b| b.kind == "approval").map(|b| b.seq).unwrap_or(0);
    let rulings: Vec<String> = blocks
        .iter()
        .filter(|b| b.kind == "ruling" && b.seq > since)
        .map(|b| format!("- {} ({}){}", b.payload["what"].as_str().unwrap_or(""), b.payload["why"].as_str().unwrap_or(""),
            b.payload["cost_if_wrong"].as_str().map(|c| format!("; if wrong: {c}")).unwrap_or_default()))
        .collect();
    let rows = results.as_array().cloned().unwrap_or_default();
    let count = |r: &str| rows.iter().filter(|x| x["result"] == r).count();
    let (pass, fail, unsure) = (count("pass"), count("fail"), count("uncertain"));
    let mut text = match error {
        Some(e) => format!("Couldn't verify the work: {e}\n"),
        None => format!("Verification: {pass} passed, {fail} failed, {unsure} uncertain.\n"),
    };
    for r in &rows {
        let mark = match r["result"].as_str() {
            Some("pass") => "✓",
            Some("fail") => "✗",
            _ => "?",
        };
        text.push_str(&format!("{mark} [{}] {}", r["id"].as_str().unwrap_or(""), r["text"].as_str().unwrap_or("")));
        if r["check"]["ok"] == false {
            // The command's own result decides; show its last line.
            let last = r["check"]["output"].as_str().and_then(|o| o.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("").trim();
            let exit = r["check"]["exit"].as_i64().map(|c| format!("exit {c}")).unwrap_or_else(|| "didn't run".into());
            let missing = if r["check"]["expect_found"] == false { ", expected text not found" } else { "" };
            text.push_str(&format!(" — check failed ({exit}{missing}){}", if last.is_empty() { String::new() } else { format!(": {last}") }));
        } else if let Some(e) = r["evidence"].as_str().filter(|e| !e.is_empty()) {
            text.push_str(&format!(" — {e}"));
        }
        text.push('\n');
    }
    if let Some(n) = notes.filter(|n| !n.trim().is_empty()) {
        text.push_str(&format!("\nVerifier's notes: {n}\n"));
    }
    if !rulings.is_empty() {
        text.push_str(&format!("\nDecisions made during the work:\n{}\n", rulings.join("\n")));
    }
    let assumptions: Vec<String> = brief["brief"]["assumptions"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(|a| format!("- {a}"))).collect();
    if !assumptions.is_empty() {
        text.push_str(&format!("\nAssumptions:\n{}\n", assumptions.join("\n")));
    }
    tape::append(&app.db, session, "report", &json!({ "text": text, "results": results, "error": error })).await?;
    set_state(app, session, "reported", "kernel", "work verified", false).await?;
    app.emit(session, json!({ "type": "report", "text": text, "results": results })).await;

    // The model's verdict, where the route allows it: accept when everything passed, more when
    // something failed after the last round. Uncertain work waits for the owner.
    let route = brief["brief"]["route"].as_str().unwrap_or("");
    let verdict = if error.is_some() || unsure > 0 {
        None
    } else if fail == 0 {
        Some("accept")
    } else {
        Some("more")
    };
    if let (Some(v), true) = (verdict, auto_close(route)) {
        sqlx::query(
            "INSERT INTO session_decisions (session_id, turn_id, decision, note, source)
             SELECT $1, (SELECT t.id FROM turns t WHERE t.session_id = $1 ORDER BY t.started_at DESC LIMIT 1), $2, $3, 'model'",
        )
        .bind(session)
        .bind(v)
        .bind(format!("{pass} passed, {fail} failed"))
        .execute(&app.db)
        .await?;
        if v == "accept" {
            set_state(app, session, "closed", "model", "all criteria passed", false).await?;
        }
    }
    let s = state(&app.db, session).await.unwrap_or_default();
    app.emit(session, json!({ "type": "idle", "state": s })).await;
    Ok(())
}

// ---------- after a turn ----------

/// What happens when a turn of a session ends: whether the kernel continues on its own (clients
/// keep waiting for an `idle` event), and what the session waits for otherwise.
pub struct After {
    pub next: bool,
    pub waiting: Option<&'static str>,
}

pub async fn after_turn(app: &AppState, session: Uuid, ending: Option<&str>, ok: bool) -> After {
    let done = After { next: false, waiting: None };
    if !ok {
        return done;
    }
    match ending {
        Some("brief proposed") => {
            let route = latest_brief(&app.db, session).await.ok().flatten().map(|(b, _)| b["brief"]["route"].as_str().unwrap_or("").to_string()).unwrap_or_default();
            if auto_approve(&route) {
                tokio::spawn(approve(app.clone(), session, "auto"));
                After { next: true, waiting: None }
            } else {
                After { next: false, waiting: Some("approval") }
            }
        }
        Some("questions asked") => After { next: false, waiting: Some("answers") },
        Some("work submitted") => {
            tokio::spawn(verify(app.clone(), session));
            After { next: true, waiting: None }
        }
        _ => done,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief() -> Value {
        json!({ "route": "bounded", "work": "build", "intent": { "stated": "add a --json flag", "assumed": ["same fields as the table"] },
                "goal": "zen models prints a table -> it can also print JSON",
                "criteria": [{ "id": "c1", "text": "tests pass", "run": "cargo test -q", "expect": "test result: ok" }],
                "context": { "repo": "~/zenbot" } })
    }

    #[test]
    fn briefs_are_checked() {
        assert!(validate_brief(&brief()).is_empty());
        let mut b = brief();
        b["route"] = json!("huge");
        b["criteria"] = json!([]);
        b["intent"]["stated"] = json!("");
        let errs = validate_brief(&b);
        assert_eq!(errs.len(), 3, "{errs:?}");
        let mut quick = brief();
        quick["route"] = json!("quick");
        quick["criteria"] = json!([]);
        assert!(validate_brief(&quick).is_empty(), "quick briefs may skip criteria");
        let mut big = brief();
        big["goal"] = json!("x".repeat(9000));
        assert!(validate_brief(&big)[0].contains("keep it near 1,000 tokens"));
    }

    #[test]
    fn framing_can_not_write_and_the_tools_never_change() {
        assert_eq!(tools_for("framing"), tools_for("working"), "one tool list across phases keeps the cache");
        for w in ["write", "edit", "move", "submit_work", "note_ruling"] {
            assert!(refuse("framing", w, None).is_some(), "{w} allowed while framing");
        }
        for r in ["read", "bash", "history", "ask", "propose_brief"] {
            assert!(refuse("framing", r, None).is_none(), "{r} refused while framing");
        }
        assert!(refuse("framing", "write", None).unwrap().contains("Propose a brief"));
        assert!(refuse("working", "write", None).is_none());
        assert!(refuse("working", "propose_brief", None).is_some());
        assert!(refuse("working", "write", Some("work submitted")).is_some(), "nothing more once the step has ended");
        let verifier: Vec<String> = tools_for("verifier").as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(verifier, ["bash", "read", "submit_verdict"]);
    }

    #[test]
    fn approvals_and_rendering() {
        assert!(is_approval("Yes") && is_approval("go ahead.") && is_approval("/go"));
        assert!(!is_approval("yes but change the flag name to --format"));
        let text = render_brief(&brief());
        assert!(text.contains("Route: bounded · Work: build"));
        assert!(text.contains("run: `cargo test -q`"));
        let sys = system_for("BASE", "working", Some(&json!({ "version": 2, "brief": brief() })));
        assert!(sys.starts_with("BASE") && sys.contains("<brief version=\"2\">") && sys.contains("source of intent"));
        assert_eq!(system_for("BASE", "framing", None), system_for("BASE", "working", None), "same instructions across phases");
        assert_eq!(system_for("BASE", "open", None), "BASE");
    }
}
