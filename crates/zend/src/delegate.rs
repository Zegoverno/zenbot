//! Delegation and model choice (DESIGN.md, "Model choice"; D-030, D-038; Phase 6).
//!
//! `delegate` hands a subtask to a subagent: a child session (kind `subagent`) with a fresh context
//! and the same instructions, memory and tools, except that it can't delegate further or ask the
//! owner. One call can hand out several tasks (`tasks`), which the kernel runs at the same time
//! (engines may run separate tool calls one after another). The parent gets the subagent's final answer; a subagent inherits the parent's
//! taint, and an answer from a subagent that read the web is untrusted (and taints the parent).
//!
//! Which model a subagent runs on is the learning router's choice (route.rs, D-074) unless the
//! call names one: System One's `task_kind` names the kind of work, the routing policy (`policies`,
//! versioned; `zen policy`) maps it to a model, and a small share tries a candidate. This module
//! keeps the policy, `pick`, and the learning every routed piece of work feeds (owner sessions and
//! job runs too): `stats` per kind and model (outcomes are the owner's verdict on the session or
//! its parent, else how a job run ended; mean cost and time), `suggest` (quality is a gate, then a
//! challenger wins on clearly better quality or on cost and time together, cost weighted 2:1, with
//! at least 20 judged outcomes each) and `tune`, the nightly `routes` job: it applies a change as a
//! new policy version with its reason (`zen policy undo` reverts), and leaves a switch to another
//! provider or to a model over twice the cost to the owner, as a note.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::{tools, AppState};

pub const SUBAGENT: &str = "subagent";
const SUBAGENT_NOTE: &str = "You are a subagent: the main agent gave you one task, below. Do it, then answer with what it needs \
to carry on: the result, how you checked it, and anything it must know or decide. You can't ask the owner or delegate further; \
take sensible defaults and say what you assumed. Be concise.";

/// The instructions a subagent gets: the session's own, plus how to work as a subagent.
pub fn system_for(base: &str) -> String {
    format!("{base}\n\n{SUBAGENT_NOTE}")
}

/// The routing policy in force: `{ "routes": { "<kind>": { "model", "candidates": [..] } }, "explore": 0.1 }`.
pub async fn policy(db: &sqlx::SqlitePool) -> (i32, Value) {
    match sqlx::query("SELECT version, data FROM policies ORDER BY version DESC LIMIT 1").fetch_optional(db).await {
        Ok(Some(r)) => (r.get("version"), r.get("data")),
        _ => (0, json!({ "routes": {} })),
    }
}

/// Save a new policy version (the old ones stay; the latest is in force).
pub async fn set_policy(db: &sqlx::SqlitePool, data: &Value, reason: &str, by: &str) -> Result<i32> {
    Ok(sqlx::query_scalar("INSERT INTO policies (data, reason, created_by) VALUES ($1, $2, $3) RETURNING version").bind(data).bind(reason).bind(by).fetch_one(db).await?)
}

/// A number in [0, 1) from the clock: enough randomness to pick when to explore.
pub(crate) fn draw() -> f64 {
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
    let explore = policy["explore"].as_f64().unwrap_or(crate::config::get().explore).clamp(0.0, 0.5);
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

/// The last thing a session's agent said.
pub(crate) async fn final_answer(db: &sqlx::SqlitePool, session: Uuid) -> String {
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
            // The learning router (route.rs): the subtask's kind, then its route.
            let c = crate::route::choose(app, Some(session), task, &parent_model).await;
            (c.model, c.kind, c.propensity, c.how)
        }
    };
    let dir = args["dir"].as_str().map(|d| tools::resolve(workspace, d)).unwrap_or_else(|| workspace.to_path_buf());
    app.emit(session, json!({ "type": "status", "text": format!("delegating to a subagent on {model}") })).await;
    let started = std::time::Instant::now();
    let child = tokio::time::timeout(Duration::from_secs(1900), crate::run_child(app, session, SUBAGENT, task, &dir, Some(&model))).await.context("the subagent took too long")??;
    let answer = final_answer(&app.db, child).await;
    let cost: f64 = sqlx::query_scalar("SELECT COALESCE(SUM(cost_usd), 0.0) FROM turns WHERE session_id = $1").bind(child).fetch_one(&app.db).await.unwrap_or(0.0);
    let error: Option<String> = sqlx::query_scalar("SELECT error FROM turns WHERE session_id = $1 ORDER BY started_at DESC LIMIT 1").bind(child).fetch_optional(&app.db).await?.flatten();
    crate::agent::log_decision(&app.db, Some(child), "model", &json!({ "kind": kind, "task": zen_proto::head(task, 300), "parent": session, "how": how }), &json!({ "model": model }), Some(&model), Some(propensity), true, error.as_deref()).await;
    let risk: i16 = sqlx::query_scalar("SELECT CASE WHEN COALESCE(id IN (SELECT id FROM zen_contaminated_sessions),false) THEN 5 ELSE risk END FROM sessions WHERE id=$1")
        .bind(child).fetch_one(&app.db).await?;
    let head = format!(
        "Subagent {} on {model} ({kind}, {how}) finished in {}s, ${cost:.3}{}:\n",
        &child.to_string()[..8],
        started.elapsed().as_secs(),
        error.as_deref().map(|e| format!(", with an error: {e}")).unwrap_or_default()
    );
    let body = if answer.trim().is_empty() { "(no answer)".to_string() } else { answer };
    // The parent now holds the child's words: a later denial of the child reaches it (D-062).
    if risk < 5 {
        crate::sources::depend(&app.db, session, &[child]).await?;
    }
    let body = crate::taint::hand_over_rating(app,session,risk,"subagent",&child.to_string(),&body).await?;
    Ok((format!("{head}{body}"), error.is_some()))
}

pub fn spec() -> Value {
    json!({
        "name": "delegate",
        "description": "Give a well-defined subtask to a subagent: a fresh session with your instructions, memory and tools, \
without ask or delegate. Use it to keep your context clean or, with several `tasks`, to work in parallel. Brief each task: goal, \
what to read first, what to avoid, what to report. Returns each final answer; the routing policy picks the model unless you name one.",
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
        Ok((content, is_error)) => tools::ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), rating: None, content, is_error },
        Err(e) => tools::ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), rating: None, content: format!("delegate failed: {e:#}"), is_error: true },
    })
}

// ---------- learning from outcomes ----------

/// Per kind of work and model: subtasks, judged ones (the parent got a verdict), accepted ones and
/// mean cost.
pub async fn stats(db: &sqlx::SqlitePool) -> Result<Vec<Value>> {
    // A routed piece of work's outcome (D-074): the owner's verdict on its session, else on its
    // parent (a subtask), else how its job run ended (ok or silent is a success, error a failure).
    let rows = sqlx::query(
        "WITH picks AS (SELECT d.session_id AS s, d.input->>'kind' AS kind, d.chosen AS model, uuid_blob(d.input->>'parent') AS parent
                        FROM decisions d WHERE d.point = 'model' AND d.session_id IS NOT NULL),
              verdicts AS (SELECT session_id,decision FROM (SELECT session_id,decision,row_number() OVER(PARTITION BY session_id ORDER BY created_at DESC,id DESC) AS rn FROM session_decisions) WHERE rn=1),
              runs AS (SELECT session_id, status FROM job_runs WHERE session_id IS NOT NULL AND status IN ('ok', 'silent', 'error')),
              costs AS (SELECT session_id, SUM(cost_usd) AS cost, SUM((julianday(ended_at)-julianday(started_at))*86400.0) AS secs FROM turns GROUP BY session_id),
              outcomes AS (SELECT p.kind, p.model, c.cost, c.secs,
                                  CASE WHEN COALESCE(vs.decision, vp.decision) IS NOT NULL THEN COALESCE(vs.decision, vp.decision) = 'accept'
                                       WHEN r.status IS NOT NULL THEN r.status <> 'error' END AS good
                           FROM picks p LEFT JOIN verdicts vs ON vs.session_id = p.s LEFT JOIN verdicts vp ON vp.session_id = p.parent
                           LEFT JOIN runs r ON r.session_id = p.s LEFT JOIN costs c ON c.session_id = p.s)
         SELECT kind, model, count(*) AS n, count(good) AS judged, count(*) FILTER (WHERE good) AS accepted, avg(cost) AS cost, avg(secs) AS secs
         FROM outcomes GROUP BY kind, model ORDER BY kind, model",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({ "kind": r.get::<Option<String>, _>("kind"), "model": r.get::<Option<String>, _>("model"), "subtasks": r.get::<i64, _>("n"),
                    "judged": r.get::<i64, _>("judged"), "accepted": r.get::<i64, _>("accepted"), "mean_cost": r.get::<Option<f64>, _>("cost"),
                    "mean_secs": r.get::<Option<f64>, _>("secs") })
        })
        .collect())
}

/// The lower end of the one-sided Wilson score interval for `k` successes in `n` trials (model
/// routing's evidence).
fn wilson_lower(k: u64, n: u64, z: f64) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let (n, p) = (n as f64, k as f64 / n as f64);
    let z2 = z * z;
    ((p + z2 / (2.0 * n)) - z * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt()) / (1.0 + z2 / n)
}

/// The upper end of the one-sided Wilson interval.
fn wilson_upper(k: u64, n: u64, z: f64) -> f64 {
    1.0 - wilson_lower(n - k.min(n), n, z)
}

/// One change the evidence supports.
#[derive(Debug, PartialEq)]
pub struct Change {
    pub kind: String,
    pub model: String,
    pub why: String,
    /// Left to the owner: another provider, or over twice the cost (D-074).
    pub owner: bool,
}

/// How much worse a challenger's acceptance may be (its lower bound against the champion's rate)
/// and still win on cost and speed.
const QUALITY_MARGIN: f64 = 0.05;

/// What the evidence says to change, per kind with a route, comparing each model with at least
/// `min` judged outcomes against the champion (D-074): quality is a gate (the challenger's
/// acceptance, at its lower bound, within QUALITY_MARGIN of the champion's), then a challenger wins
/// on clearly better quality, or on cost and time together (cost weighted 2:1) at least 15% better.
pub fn suggest(policy: &Value, stats: &[Value], min: u64) -> Vec<Change> {
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
        let Some(champ) = stats.iter().find(|s| s["kind"] == kind.as_str() && s["model"] == current) else { continue };
        let num = |s: &Value, k: &str| s[k].as_u64().unwrap_or(0);
        let (ck, cn) = (num(champ, "accepted"), num(champ, "judged"));
        if cn < min {
            continue;
        }
        let rate = ck as f64 / cn as f64;
        let upper = wilson_upper(ck, cn, z);
        let cost = |s: &Value| s["mean_cost"].as_f64().unwrap_or(0.0);
        let secs = |s: &Value| s["mean_secs"].as_f64().unwrap_or(0.0);
        let mut best: Option<(f64, Change)> = None;
        for s in stats.iter().filter(|s| s["kind"] == kind.as_str() && s["model"] != current && num(s, "judged") >= min) {
            let model = s["model"].as_str().unwrap_or("").to_string();
            let lower = wilson_lower(num(s, "accepted"), num(s, "judged"), z);
            if lower < rate - QUALITY_MARGIN {
                continue;
            }
            let ratio = |a: f64, b: f64| if b > 0.0 { a / b } else { 1.0 };
            let value = (2.0 * ratio(cost(s), cost(champ)) + ratio(secs(s), secs(champ))) / 3.0;
            let (score, why) = if lower > upper {
                (2.0 + lower, format!("accepted at ≥{lower:.2} vs ≤{upper:.2} for {current}"))
            } else if value <= 0.85 {
                (1.0 - value, format!("as good (≥{lower:.2} vs {rate:.2}) at {:.0}% of {current}'s cost and time", value * 100.0))
            } else {
                continue;
            };
            let provider = |m: &str| m.split('/').next().unwrap_or("").to_string();
            let owner = provider(&model) != provider(current) || cost(s) > 2.0 * cost(champ);
            if best.as_ref().is_none_or(|(b, _)| score > *b) {
                best = Some((score, Change { kind: kind.clone(), model, why, owner }));
            }
        }
        out.extend(best.map(|(_, c)| c));
    }
    out
}

/// The nightly `routes` job (jobs.rs): apply what the evidence clearly says (a new policy version per change).
pub async fn tune(db: &sqlx::SqlitePool) -> Vec<String> {
    let (version, mut p) = policy(db).await;
    let Ok(st) = stats(db).await else { return Vec::new() };
    let mut notes = Vec::new();
    for c in suggest(&p, &st, crate::config::get().policy_min_judged) {
        let old = p["routes"][&c.kind]["model"].as_str().unwrap_or("").to_string();
        if c.owner {
            notes.push(format!("routes: {}: {} looks better than {old} ({}); it's another provider or costs over twice as much, so it's yours to decide: `zen policy set {} {}`", c.kind, c.model, c.why, c.kind, c.model));
            continue;
        }
        p["routes"][&c.kind]["model"] = json!(c.model);
        let mut cands: Vec<Value> = p["routes"][&c.kind]["candidates"].as_array().cloned().unwrap_or_default();
        cands.retain(|m| m != c.model.as_str());
        if !cands.iter().any(|m| m == old.as_str()) {
            cands.push(json!(old));
        }
        p["routes"][&c.kind]["candidates"] = Value::Array(cands);
        let reason = format!("{}: {old} -> {}, {} (from policy v{version})", c.kind, c.model, c.why);
        if set_policy(db, &p, &reason, "routes").await.is_ok() {
            notes.push(format!("policy: {reason}; `zen policy undo` reverts"));
        }
    }
    // Work no kind fitted, to grow the list: the owner adds a kind to the policy (`zen policy`).
    if let Ok(n) = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM decisions WHERE point = 'model' AND input->>'kind' = 'unsure' AND created_at > strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-7 days')").fetch_one(db).await {
        if n >= 10 {
            notes.push(format!("routes: {n} pieces of work in the last week fitted no kind of task (the default model ran them); `zen policy` shows the kinds"));
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wilson_lower_needs_many_agreeing_outcomes() {
        assert_eq!(wilson_lower(0, 0, 1.645), 0.0);
        assert!(wilson_lower(10, 10, 1.645) < 0.95, "ten agreements aren't enough");
        assert!(wilson_lower(52, 52, 1.645) >= 0.95);
        assert!(wilson_lower(51, 52, 1.645) < 0.95, "one rejection costs a lot");
    }

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
        let s = |m: &str, k: u64, n: u64| json!({ "kind": "build", "model": m, "accepted": k, "judged": n, "mean_cost": 1.0, "mean_secs": 60.0 });
        assert!(suggest(&p, &[s("claude/a", 10, 20), s("claude/b", 15, 20)], 20).is_empty(), "better, but not clearly");
        let r = suggest(&p, &[s("claude/a", 5, 40), s("claude/b", 38, 40)], 20);
        assert_eq!((r[0].model.as_str(), r[0].owner), ("claude/b", false));
        assert!(suggest(&p, &[s("claude/a", 0, 5), s("claude/b", 5, 5)], 20).is_empty(), "too few judged");
    }

    #[test]
    fn quality_is_a_gate_then_cost_and_time_decide() {
        let p = json!({ "routes": { "chat": { "model": "claude/opus" } } });
        let s = |m: &str, k: u64, cost: f64, secs: f64| json!({ "kind": "chat", "model": m, "accepted": k, "judged": 60, "mean_cost": cost, "mean_secs": secs });
        let r = suggest(&p, &[s("claude/opus", 57, 1.0, 30.0), s("claude/sonnet", 58, 0.3, 20.0)], 20);
        assert_eq!((r[0].model.as_str(), r[0].owner), ("claude/sonnet", false), "as good and much cheaper: switch on its own");
        assert!(suggest(&p, &[s("claude/opus", 57, 1.0, 30.0), s("claude/haiku", 40, 0.1, 10.0)], 20).is_empty(), "cheaper but worse: never");
        assert!(suggest(&p, &[s("claude/opus", 57, 1.0, 30.0), s("claude/sonnet", 58, 0.95, 29.0)], 20).is_empty(), "barely cheaper: not worth a switch");
        let r = suggest(&p, &[s("claude/opus", 57, 1.0, 30.0), s("codex/gpt", 58, 0.3, 20.0)], 20);
        assert!(r[0].owner, "another provider is the owner's call");
        let r = suggest(&p, &[s("claude/opus", 30, 1.0, 30.0), s("claude/max", 59, 3.0, 40.0)], 20);
        assert!(r[0].owner && r[0].model == "claude/max", "clearly better but three times the cost: the owner's call");
    }
}
