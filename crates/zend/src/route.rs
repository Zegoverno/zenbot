//! The learning router (D-074): which model does a piece of work.
//!
//! Every owner session whose model nobody chose (`sessions.model_source` empty: no `-m`, no
//! `/model`), every agent job run without a model the owner set, and every subtask `delegate` hands
//! out without one, starts with System One's `task_kind` decision: which of the routing policy's
//! kinds of task the request is (chat, recall, writing, coding-large, …), from the owner's words or
//! the job's or subtask's text. A confident kind takes its route (`policies`: a champion model and
//! candidates); an unsure answer, an `other` or an unavailable System One keeps the model the work
//! would have had (the default, the most capable). A small share of runs (the policy's `explore`,
//! 5% to start) try a candidate instead, never on a kind marked `explore: false` (planning, large
//! builds); each choice is logged with its probability (`decisions`, point `model`), so the nightly
//! `routes` job (`delegate::tune`) compares models on outcomes without bias, and switches a route
//! only on clear evidence.
//!
//! The policy starts with the kinds below and their starting routes, written once as a policy
//! version the owner can read and change (`zen policy`); kinds the owner adds there are offered to
//! System One the same way. A model the owner pins always wins: the router never changes it.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

/// How capable a kind's starting champion is.
#[derive(Clone, Copy)]
enum Tier {
    /// A lighter, cheaper model to start; the default model is a candidate.
    Light,
    /// The default model to start; a lighter one is a candidate.
    Mid,
    /// The default model, without exploring.
    Top,
}

const LIGHT: &str = "claude/claude-sonnet-5-5";
const LIGHTEST: &str = "claude/claude-haiku-5-5";

/// The kinds the router starts with: name, what it is (System One reads this), starting tier.
const KINDS: &[(&str, &str, Tier)] = &[
    ("chat", "A quick answer, explanation or conversation, with little to work out", Tier::Light),
    ("recall", "Finding or recalling something already known: memory, past sessions, notes, a lookup", Tier::Light),
    ("routine", "A recurring routine: a check-in, a digest, filling a fixed template", Tier::Light),
    ("writing", "Writing or editing a message, note, summary or document", Tier::Mid),
    ("research", "Researching a question across sources and weighing them", Tier::Mid),
    ("coding-small", "A small code change, bug fix or script", Tier::Mid),
    ("ops", "Operating systems and services: deploys, servers, configuration", Tier::Mid),
    ("coding-large", "A feature, refactor or multi-step build across a codebase", Tier::Top),
    ("planning", "Planning, design or a decision with trade-offs and stakes", Tier::Top),
];

/// The share of routed runs that try a candidate, unless the policy sets its own.
const EXPLORE: f64 = 0.05;

fn route_for(tier: Tier, default: &str) -> Value {
    match tier {
        Tier::Light => json!({ "model": LIGHT, "candidates": [LIGHTEST, default] }),
        Tier::Mid => json!({ "model": default, "candidates": [LIGHT] }),
        Tier::Top => json!({ "model": default, "candidates": [] }),
    }
}

/// The routing policy, with every starting kind and its route (added once, as a new version).
pub async fn policy(app: &AppState, default: &str) -> Value {
    let (_, mut p) = crate::delegate::policy(&app.db).await;
    let mut changed = false;
    for (name, what, tier) in KINDS {
        if p["kinds"][*name].is_null() {
            p["kinds"][*name] = json!({ "description": what, "explore": !matches!(tier, Tier::Top) });
            changed = true;
        }
        if p["routes"][*name].is_null() {
            p["routes"][*name] = route_for(*tier, default);
            changed = true;
        }
    }
    if p["explore"].is_null() {
        p["explore"] = json!(EXPLORE);
        changed = true;
    }
    if changed {
        if let Err(e) = crate::delegate::set_policy(&app.db, &p, "the learning router's kinds and starting routes (D-074)", "router").await {
            tracing::warn!("saving the router's starting policy: {e:#}");
        }
    }
    p
}

/// What the router chose.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub model: String,
    /// The kind of task, or `unsure`.
    pub kind: String,
    /// How sure System One was of the kind.
    pub p: f64,
    /// The probability this model had of being chosen (for unbiased comparison).
    pub propensity: f64,
    /// `policy`, `explore` or `unsure`.
    pub how: &'static str,
}

/// The kind of task `text` is: `(kind, p)` when System One is confident, None otherwise.
async fn classify(app: &AppState, session: Option<Uuid>, text: &str, policy: &Value) -> (Option<String>, f64) {
    let mut kinds = serde_json::Map::new();
    for (name, k) in policy["kinds"].as_object().into_iter().flatten() {
        kinds.insert(name.clone(), json!(k["description"].as_str().unwrap_or(name)));
    }
    kinds.insert("other".into(), json!("None of these fits well"));
    let facts = crate::s1::Facts::new(session).with("task", json!(zen_proto::head(text, 3000))).with("kinds", Value::Object(kinds));
    let d = crate::s1::decide(&app.db, "task_kind", facts).await;
    let kind = d.choice("kind").map(String::from);
    let p = kind.as_deref().and_then(|k| d.answers["kind"]["probabilities"][k].as_f64()).unwrap_or(0.0);
    (if d.is("known") { kind } else { None }, p)
}

/// Choose the model for work described by `text`; `default` is what it gets when the router isn't
/// sure (the model it would have had).
pub async fn choose(app: &AppState, session: Option<Uuid>, text: &str, default: &str) -> Choice {
    let policy = policy(app, default).await;
    let (kind, p) = classify(app, session, text, &policy).await;
    let Some(kind) = kind else {
        return Choice { model: default.to_string(), kind: "unsure".into(), p, propensity: 1.0, how: "unsure" };
    };
    let mut served: Vec<String> = app.routes.lock().await.keys().cloned().collect();
    let wanted = policy["routes"][&kind]["model"].as_str().unwrap_or(default);
    if !served.iter().any(|m| m == wanted) {
        // The workers' model lists may not have been read yet (a fresh kernel's first prompt).
        crate::collect_models(app).await;
        served = app.routes.lock().await.keys().cloned().collect();
    }
    let (model, propensity, how) = pick(&policy, &kind, default, &served, crate::delegate::draw());
    Choice { model, kind, p, propensity, how }
}

/// `delegate::pick`, but never exploring a kind marked `explore: false`.
fn pick(policy: &Value, kind: &str, default: &str, served: &[String], r: f64) -> (String, f64, &'static str) {
    if policy["kinds"][kind]["explore"] == false {
        let mut fixed = policy.clone();
        fixed["explore"] = json!(0.0);
        return crate::delegate::pick(&fixed, kind, default, served, r);
    }
    crate::delegate::pick(policy, kind, default, served, r)
}

/// Record a choice for the session that runs on it (the outcomes `delegate::stats` reads).
pub async fn log(app: &AppState, session: Uuid, choice: &Choice, via: &str, text: &str, parent: Option<Uuid>) {
    let input = json!({ "kind": choice.kind, "p": choice.p, "how": choice.how, "via": via, "task": zen_proto::head(text, 300), "parent": parent });
    crate::agent::log_decision(&app.db, Some(session), "model", &input, &json!({ "model": choice.model }), Some(&choice.model), Some(choice.propensity), true, None).await;
}

/// At an owner session's first prompt: route it, unless its model was chosen or it has run before.
pub async fn session_start(app: &AppState, session: Uuid, text: &str) {
    let row: Option<(Option<String>, Option<String>, String, bool)> = sqlx::query_as(
        "SELECT kind, model_source, model, EXISTS (SELECT 1 FROM turns WHERE session_id = $1) FROM sessions WHERE id = $1",
    )
    .bind(session)
    .fetch_optional(&app.db)
    .await
    .unwrap_or(None);
    let Some((None, None, model, false)) = row else { return };
    let choice = choose(app, Some(session), text, &model).await;
    let moved = choice.model != model;
    let r = sqlx::query("UPDATE sessions SET model = $2, model_source = 'router', effort = CASE WHEN $3 THEN NULL ELSE effort END WHERE id = $1 AND model_source IS NULL")
        .bind(session)
        .bind(&choice.model)
        .bind(moved)
        .execute(&app.db)
        .await;
    if let Err(e) = r {
        tracing::warn!("routing session {session}: {e:#}");
        return;
    }
    log(app, session, &choice, "session", text, None).await;
    if moved {
        app.emit(session, json!({ "type": "model", "model": choice.model, "effort": null })).await;
        app.emit(session, json!({ "type": "status", "text": format!("routed to {} ({}, {:.2}); /model to choose another", choice.model, choice.kind, choice.p) })).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_routes_follow_the_tier_and_planning_never_explores() {
        let default = "claude/claude-opus-5-5";
        let mut p = json!({});
        for (name, what, tier) in KINDS {
            p["kinds"][*name] = json!({ "description": what, "explore": !matches!(tier, Tier::Top) });
            p["routes"][*name] = route_for(*tier, default);
        }
        p["explore"] = json!(0.5);
        let served = vec![default.to_string(), LIGHT.to_string(), LIGHTEST.to_string()];
        assert_eq!(pick(&p, "chat", default, &served, 0.9).0, LIGHT, "a light kind starts on the lighter model");
        assert_eq!(pick(&p, "writing", default, &served, 0.9).0, default);
        assert_eq!(pick(&p, "writing", default, &served, 0.1), (LIGHT.to_string(), 0.5, "explore"), "a candidate, now and then");
        assert_eq!(pick(&p, "planning", default, &served, 0.0), (default.to_string(), 1.0, "policy"), "never explored");
        let only_default = vec![default.to_string()];
        assert_eq!(pick(&p, "chat", default, &only_default, 0.9).0, default, "an unserved route falls back");
    }
}
