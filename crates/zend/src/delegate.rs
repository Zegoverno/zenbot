//! Delegation and model choice (DESIGN.md, "Model choice"; D-030, D-038; Phase 6).
//!
//! `delegate` hands a subtask to a subagent: a child session (kind `subagent`) with a fresh context
//! and the same instructions, memory and tools, except that it can't delegate further or ask the
//! owner. One call can hand out several tasks (`tasks`), which the kernel runs at the same time
//! (engines may run separate tool calls one after another). The parent gets the subagent's final answer; a subagent inherits the parent's
//! taint, and an answer from a subagent that read the web is untrusted (and taints the parent).
//!
//! Which model a subagent runs on is learned from real use, not fixed eval tasks. Unless the call
//! names one, System One classifies the kind of work, and the routing policy (`policies`, versioned;
//! `zen policy`) maps it to a model. A small share of subtasks (ZEN_EXPLORE, default 0.1) try another
//! of the route's candidates, and each choice is logged with its probability (`decisions`, point
//! `model`) so the comparison stays unbiased. Outcomes are the owner's verdict on the parent
//! session, and cost. The nightly sleep switches a route to another model only when the lower bound
//! of that model's acceptance beats the upper bound of the current one's (at least 20 judged
//! subtasks each); every change is a new policy version with its reason, and `zen policy undo`
//! reverts.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{tools, App, AppState};

pub const SUBAGENT: &str = "subagent";
const KINDS: [&str; 8] = ["understand", "shape", "bet", "build", "verify", "maintain", "reflect", "reach"];
const SUBAGENT_NOTE: &str = "You are a subagent: the main agent gave you one task, below. Do it, then answer with what it needs \
to carry on: the result, how you checked it, and anything it must know or decide. You can't ask the owner or delegate further; \
take sensible defaults and say what you assumed. Be concise.";

/// The instructions a subagent gets: the session's own, plus how to work as a subagent.
pub fn system_for(base: &str) -> String {
    format!("{base}\n\n{SUBAGENT_NOTE}")
}

/// The routing policy in force: `{ "routes": { "<kind>": { "model", "candidates": [..] } }, "explore": 0.1 }`.
pub async fn policy(db: &sqlx::PgPool) -> (i32, Value) {
    match sqlx::query("SELECT version, data FROM policies ORDER BY version DESC LIMIT 1").fetch_optional(db).await {
        Ok(Some(r)) => (r.get("version"), r.get("data")),
        _ => (0, json!({ "routes": {} })),
    }
}

/// Save a new policy version (the old ones stay; the latest is in force).
pub async fn set_policy(db: &sqlx::PgPool, data: &Value, reason: &str, by: &str) -> Result<i32> {
    Ok(sqlx::query_scalar("INSERT INTO policies (data, reason, created_by) VALUES ($1, $2, $3) RETURNING version").bind(data).bind(reason).bind(by).fetch_one(db).await?)
}

/// A number in [0, 1) from the clock: enough randomness to pick when to explore.
fn draw() -> f64 {
    // Random bits, not the clock: parallel tasks start in the same instant, and correlated draws
    // would bias exploration and the logged probabilities.
    (Uuid::new_v4().as_u128() >> 75) as f64 / (1u64 << 53) as f64
}

/// Pick the model for a subtask: `(model, kind, probability it had of being picked, how)`.
/// `served` is the models a worker serves now.
pub fn pick(policy: &Value, kind: &str, fallback: &str, served: &[String], r: f64) -> (String, f64, &'static str) {
    let route = if policy["routes"][kind].is_object() { &policy["routes"][kind] } else { &policy["routes"]["default"] };
    let base = route["model"].as_str().filter(|m| served.iter().any(|s| s == m)).unwrap_or(fallback).to_string();
    let others: Vec<String> = route["candidates"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|m| *m != base && served.iter().any(|s| s == m)).map(String::from).collect();
    let explore = policy["explore"].as_f64().unwrap_or_else(|| crate::env_num("ZEN_EXPLORE", 0.1)).clamp(0.0, 0.5);
    if others.is_empty() || explore == 0.0 {
        return (base, 1.0, "policy");
    }
    if r < explore {
        let i = ((r / explore) * others.len() as f64) as usize;
        (others[i.min(others.len() - 1)].clone(), explore / others.len() as f64, "explore")
    } else {
        (base, 1.0 - explore, "policy")
    }
}

/// The kind of work a task is, as System One judges it ("unknown" without it).
async fn kind_of(app: &App, task: &str) -> String {
    if crate::score::scorer().is_none() || !crate::score::private_ok() {
        return "unknown".into();
    }
    let q = json!({ "kind": crate::score::questions()["work"].clone() });
    match crate::score::decide(app, &json!({ "task": zen_proto::head(task, 4000) }), &q).await {
        Ok(v) if v["error"].is_null() => v["answers"]["kind"]["choice"].as_str().filter(|k| KINDS.contains(k)).unwrap_or("unknown").to_string(),
        _ => "unknown".into(),
    }
}

/// The last thing a session's agent said.
pub(crate) async fn final_answer(db: &sqlx::PgPool, session: Uuid) -> String {
    let rows = crate::tape::load(db, session, &["message"]).await.unwrap_or_default();
    rows.iter()
        .rev()
        .filter(|b| b.payload["role"] == "assistant")
        .map(|b| zen_proto::text_of(&b.payload["content"]))
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default()
}

async fn delegate_all(app: &AppState, session: Uuid, workspace: &std::path::Path, args: &Value) -> Result<(String, bool)> {
    let mut jobs: Vec<Value> = args["tasks"].as_array().cloned().unwrap_or_default();
    if let Some(t) = args["task"].as_str() {
        jobs.insert(0, json!({ "task": t, "model": args["model"], "dir": args["dir"] }));
    }
    let jobs: Vec<Value> = jobs.into_iter().map(|j| if j.is_string() { json!({ "task": j }) } else { j }).collect();
    anyhow::ensure!(!jobs.is_empty(), "delegate needs a `task` (or `tasks`): the goal, what to read first, and when it's done");
    anyhow::ensure!(jobs.len() <= 8, "at most 8 tasks at a time");
    // One model refresh for the whole call, not one per task.
    crate::collect_models(app).await;
    let served: Vec<String> = app.routes.lock().await.keys().cloned().collect();
    let runs = jobs.iter().map(|j| delegate(app, session, workspace, j, &served));
    let results = futures_util::future::join_all(runs).await;
    let mut out = Vec::new();
    let mut any_error = false;
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok((text, err)) => {
                any_error |= err;
                out.push(if jobs.len() > 1 { format!("## Task {}\n{text}", i + 1) } else { text });
            }
            Err(e) => {
                any_error = true;
                out.push(format!("## Task {}\nfailed: {e:#}", i + 1));
            }
        }
    }
    Ok((out.join("\n\n"), any_error))
}

async fn delegate(app: &AppState, session: Uuid, workspace: &std::path::Path, args: &Value, served: &[String]) -> Result<(String, bool)> {
    let task = args["task"].as_str().map(str::trim).filter(|t| !t.is_empty()).context("each task needs its text: the goal, what to read first, and when it's done")?;
    let row = sqlx::query("SELECT model, kind FROM sessions WHERE id = $1").bind(session).fetch_one(&app.db).await?;
    anyhow::ensure!(row.get::<Option<String>, _>("kind").as_deref() != Some(SUBAGENT), "a subagent can't delegate further");
    let parent_model: String = row.get("model");
    let (model, kind, propensity, how) = match args["model"].as_str().map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => {
            anyhow::ensure!(served.iter().any(|s| s == m), "no worker serves `{m}`");
            (m.to_string(), "asked".to_string(), 1.0, "asked")
        }
        None => {
            let kind = kind_of(app, task).await;
            let (_, p) = policy(&app.db).await;
            let (m, prob, how) = pick(&p, &kind, &parent_model, served, draw());
            (m, kind, prob, how)
        }
    };
    let dir = args["dir"].as_str().map(|d| tools::resolve(workspace, d)).unwrap_or_else(|| workspace.to_path_buf());
    app.emit(session, json!({ "type": "status", "text": format!("delegating to a subagent on {model}") })).await;
    let started = std::time::Instant::now();
    let child = tokio::time::timeout(Duration::from_secs(1900), crate::run_child(app, session, SUBAGENT, task, &dir, Some(&model))).await.context("the subagent took too long")??;
    let answer = final_answer(&app.db, child).await;
    let cost: f64 = sqlx::query_scalar("SELECT COALESCE(SUM(cost_usd), 0) FROM turns WHERE session_id = $1").bind(child).fetch_one(&app.db).await.unwrap_or(0.0);
    let error: Option<String> = sqlx::query_scalar("SELECT error FROM turns WHERE session_id = $1 ORDER BY started_at DESC LIMIT 1").bind(child).fetch_optional(&app.db).await?.flatten();
    crate::agent::log_decision(&app.db, Some(child), "model", &json!({ "kind": kind, "task": zen_proto::head(task, 300), "parent": session, "how": how }), &json!({ "model": model }), Some(&model), Some(propensity), true, error.as_deref()).await;
    let tainted = crate::taint::tainted(&app.db, child).await;
    let head = format!(
        "Subagent {} on {model} ({kind}, {how}) finished in {}s, ${cost:.3}{}:\n",
        &child.to_string()[..8],
        started.elapsed().as_secs(),
        error.as_deref().map(|e| format!(", with an error: {e}")).unwrap_or_default()
    );
    let body = if answer.trim().is_empty() { "(no answer)".to_string() } else { answer };
    if tainted {
        // Unmarked, the subagent's answer is withheld (the error goes to the model instead).
        crate::taint::taint(app, session, "delegate", &child.to_string()).await?;
        return Ok((format!("{head}{}", crate::taint::untrusted("subagent", &child.to_string(), &body)), error.is_some()));
    }
    Ok((format!("{head}{body}"), error.is_some()))
}

pub fn spec() -> Value {
    json!({
        "name": "delegate",
        "description": "Hand a well-defined subtask to a subagent: a fresh session with your instructions, memory and tools (but it \
can't ask the owner or delegate). Use it to keep your context clean (a broad search, a long investigation, a side task) or to work \
in parallel: give several `tasks` in one call and they run at the same time. Write each task like a brief: the goal, what to read \
first, what to avoid, and what to report. Returns each subagent's final answer. The model is chosen by the routing policy unless you \
name one.",
        "parameters": { "type": "object", "properties": {
            "task": { "type": "string", "description": "The subtask, self-contained: the subagent sees nothing of this conversation" },
            "tasks": { "type": "array", "description": "Several subtasks to run at the same time, each {task, model?, dir?}", "items": { "type": "object", "properties": {
                "task": { "type": "string" }, "model": { "type": "string" }, "dir": { "type": "string" } }, "required": ["task"] } },
            "model": { "type": "string", "description": "Optional: a model id (default: the routing policy)" },
            "dir": { "type": "string", "description": "Optional: the directory to work in (default: your workspace)" } },
          "required": [] }
    })
}

/// Run `delegate`. None for other tools.
pub async fn run_tool(app: &AppState, session: Uuid, workspace: &std::path::Path, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    if name != "delegate" {
        return None;
    }
    Some(match delegate_all(app, session, workspace, args).await {
        Ok((content, is_error)) => tools::ToolOutput { content, is_error },
        Err(e) => tools::ToolOutput { content: format!("delegate failed: {e:#}"), is_error: true },
    })
}

// ---------- learning from outcomes ----------

/// Per kind of work and model: subtasks, judged ones (the parent got a verdict), accepted ones and
/// mean cost.
pub async fn stats(db: &sqlx::PgPool) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        "WITH picks AS (SELECT d.session_id AS child, d.input->>'kind' AS kind, d.chosen AS model, (d.input->>'parent')::uuid AS parent
                        FROM decisions d WHERE d.point = 'model' AND d.session_id IS NOT NULL),
              verdicts AS (SELECT DISTINCT ON (session_id) session_id, decision FROM session_decisions ORDER BY session_id, created_at DESC),
              costs AS (SELECT session_id, SUM(cost_usd) AS cost FROM turns GROUP BY session_id)
         SELECT p.kind, p.model, count(*) AS n, count(v.decision) AS judged, count(*) FILTER (WHERE v.decision = 'accept') AS accepted,
                avg(c.cost) AS cost
         FROM picks p LEFT JOIN verdicts v ON v.session_id = p.parent LEFT JOIN costs c ON c.session_id = p.child
         GROUP BY p.kind, p.model ORDER BY p.kind, p.model",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({ "kind": r.get::<Option<String>, _>("kind"), "model": r.get::<Option<String>, _>("model"), "subtasks": r.get::<i64, _>("n"),
                    "judged": r.get::<i64, _>("judged"), "accepted": r.get::<i64, _>("accepted"), "mean_cost": r.get::<Option<f64>, _>("cost") })
        })
        .collect())
}

/// The upper end of the one-sided Wilson interval.
fn wilson_upper(k: u64, n: u64, z: f64) -> f64 {
    1.0 - crate::memory::wilson_lower(n - k.min(n), n, z)
}

/// What the evidence says to change: for each kind with a route, a model whose acceptance's lower
/// bound beats the current model's upper bound, both with at least `min` judged subtasks.
pub fn suggest(policy: &Value, stats: &[Value], min: u64) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let z = 1.645;
    let kinds: Vec<String> = stats.iter().filter_map(|s| s["kind"].as_str().map(String::from)).fold(Vec::new(), |mut v, k| {
        if !v.contains(&k) {
            v.push(k);
        }
        v
    });
    for kind in kinds {
        let Some(current) = policy["routes"][&kind]["model"].as_str() else { continue };
        let row = |m: &str| stats.iter().find(|s| s["kind"] == kind.as_str() && s["model"] == m).map(|s| (s["accepted"].as_u64().unwrap_or(0), s["judged"].as_u64().unwrap_or(0)));
        let Some((ck, cn)) = row(current) else { continue };
        if cn < min {
            continue;
        }
        let cur_upper = wilson_upper(ck, cn, z);
        let best = stats
            .iter()
            .filter(|s| s["kind"] == kind.as_str() && s["model"] != current && s["judged"].as_u64().unwrap_or(0) >= min)
            .map(|s| (s["model"].as_str().unwrap_or("").to_string(), crate::memory::wilson_lower(s["accepted"].as_u64().unwrap_or(0), s["judged"].as_u64().unwrap_or(0), z)))
            .filter(|(_, lower)| *lower > cur_upper)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((m, lower)) = best {
            out.push((kind.clone(), m, format!("accepted at ≥{lower:.2} (lower bound) vs ≤{cur_upper:.2} for {current}")));
        }
    }
    out
}

/// The sleep's part: apply what the evidence clearly says (a new policy version per change).
pub async fn tune(db: &sqlx::PgPool) -> Vec<String> {
    let (version, mut p) = policy(db).await;
    let Ok(st) = stats(db).await else { return Vec::new() };
    let changes = suggest(&p, &st, crate::env_num("ZEN_POLICY_MIN_JUDGED", 20.0) as u64);
    let mut notes = Vec::new();
    for (kind, model, why) in changes {
        let old = p["routes"][&kind]["model"].as_str().unwrap_or("").to_string();
        p["routes"][&kind]["model"] = json!(model);
        let mut cands: Vec<Value> = p["routes"][&kind]["candidates"].as_array().cloned().unwrap_or_default();
        if !cands.iter().any(|c| c == old.as_str()) {
            cands.push(json!(old));
        }
        p["routes"][&kind]["candidates"] = Value::Array(cands);
        let reason = format!("{kind}: {old} -> {model}, {why} (from policy v{version})");
        if set_policy(db, &p, &reason, "sleep").await.is_ok() {
            notes.push(format!("policy: {reason}; `zen policy undo` reverts"));
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_follows_the_route_and_explores_a_small_share() {
        let served = vec!["claude/a".to_string(), "claude/b".to_string(), "codex/c".to_string()];
        let p = json!({ "routes": { "build": { "model": "claude/a", "candidates": ["claude/b", "codex/c", "gone/x"] } }, "explore": 0.2 });
        assert_eq!(pick(&p, "build", "claude/z", &served, 0.5), ("claude/a".into(), 0.8, "policy"));
        let (m, prob, how) = pick(&p, "build", "claude/z", &served, 0.05);
        assert_eq!((m.as_str(), how), ("claude/b", "explore"));
        assert!((prob - 0.1).abs() < 1e-9, "two other candidates share the 0.2");
        assert_eq!(pick(&p, "build", "claude/z", &served, 0.15).0, "codex/c");
        assert_eq!(pick(&p, "verify", "claude/z", &served, 0.01), ("claude/z".into(), 1.0, "policy"), "no route: the parent's model");
    }

    #[test]
    fn a_route_changes_only_on_clear_evidence() {
        let p = json!({ "routes": { "build": { "model": "claude/a" } } });
        let s = |m: &str, k: u64, n: u64| json!({ "kind": "build", "model": m, "accepted": k, "judged": n });
        assert!(suggest(&p, &[s("claude/a", 10, 20), s("claude/b", 15, 20)], 20).is_empty(), "better, but not clearly");
        let r = suggest(&p, &[s("claude/a", 5, 40), s("claude/b", 38, 40)], 20);
        assert_eq!(r[0].1, "claude/b");
        assert!(suggest(&p, &[s("claude/a", 0, 5), s("claude/b", 5, 5)], 20).is_empty(), "too few judged");
    }
}
