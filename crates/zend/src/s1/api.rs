//! S1 for the owner: `GET /api/s1` (each decision's numbers), `/check`, `/models`, `POST /replay`
//! (changed rules over recorded answers, no model call), `POST /compare` (recorded states asked
//! again on approved models), `/proposals` (the learner's, applied only on the owner's approval).

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use super::{check, learn, policy, registry, run};
use crate::api::{ApiError, ApiResult};
use crate::AppState;

/// Per decision over the last 30 days: calls, how they ended, outcomes, latency, cost, agreement
/// with labelled outcomes.
pub(crate) async fn stats(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(
        "SELECT point, count(*) AS calls,
                count(*) FILTER (WHERE source = 'model') AS answered,
                count(*) FILTER (WHERE source = 'unavailable') AS unavailable,
                count(*) FILTER (WHERE source = 'blocked') AS blocked,
                NULL AS p50,
                NULL AS p95,
                COALESCE(sum(cost_usd), 0.0) AS cost,
                max(created_at) AS last
         FROM decisions WHERE source IS NOT NULL AND created_at > strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-30 days')
         GROUP BY point ORDER BY point",
    )
    .fetch_all(&app.db)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        let point: String = r.get("point");
        let mut latencies: Vec<f64> = sqlx::query_scalar("SELECT CAST(latency_ms AS REAL) FROM decisions WHERE point=$1 AND source IS NOT NULL AND latency_ms IS NOT NULL AND created_at>strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-30 days') ORDER BY latency_ms")
            .bind(&point).fetch_all(&app.db).await?;
        latencies.sort_by(f64::total_cmp);
        let percentile = |p:f64| -> Option<f64> {
            if latencies.is_empty() {return None;}
            let index=(latencies.len()-1) as f64*p;
            let lo=index.floor() as usize;
            let hi=index.ceil() as usize;
            Some(latencies[lo]+(latencies[hi]-latencies[lo])*(index-lo as f64))
        };
        let outcomes: Vec<(Option<String>, i64)> = sqlx::query_as(
            "SELECT chosen, count(*) FROM decisions WHERE point = $1 AND source = 'model' AND created_at > strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-30 days') GROUP BY chosen ORDER BY count(*) DESC LIMIT 8",
        )
        .bind(&point)
        .fetch_all(&app.db)
        .await?;
        let labelled: (i64, i64) = sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE o.correct) FROM s1_outcomes o JOIN decisions d ON d.id = o.decision_id WHERE d.point = $1",
        )
        .bind(&point)
        .fetch_one(&app.db)
        .await?;
        out.push(json!({
            "decision": point,
            "calls": r.get::<i64, _>("calls"),
            "answered": r.get::<i64, _>("answered"),
            "unavailable": r.get::<i64, _>("unavailable"),
            "blocked": r.get::<i64, _>("blocked"),
            "latency_ms": { "p50": percentile(0.5), "p95": percentile(0.95) },
            "cost_usd": r.get::<f64, _>("cost"),
            "outcomes": outcomes.into_iter().map(|(o, n)| json!({ "outcome": o, "count": n })).collect::<Vec<_>>(),
            "labelled": labelled.0,
            "correct": labelled.1,
            "last": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last"),
        }));
    }
    let proposals = learn::proposals(&app.db).await?;
    Ok(Json(json!({ "decisions": out, "proposals": proposals.as_array().map(|a| a.len()).unwrap_or(0), "files": super::status() })))
}

pub(crate) async fn check_files() -> Json<Value> {
    Json(super::status())
}

pub(crate) async fn models() -> Json<Value> {
    let r = registry();
    Json(json!({ "default": r.models.default, "list": r.models.list, "source": r.models.source }))
}

#[derive(Deserialize)]
pub(crate) struct Replay {
    decision: String,
    /// A decision file's text to try instead of the one in force.
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    last: Option<i64>,
}

/// The policy of `file` (or the one in force) over recorded answers: which outcomes would change.
pub(crate) async fn replay(State(app): State<AppState>, Json(r): Json<Replay>) -> ApiResult<Json<Value>> {
    let spec = match &r.file {
        Some(src) => {
            let models = registry().models.clone();
            let (spec, diags) = check::check(&r.decision, src, &models);
            spec.ok_or_else(|| ApiError(axum::http::StatusCode::BAD_REQUEST, check::render(&format!("{}.toml", r.decision), &diags)))?
        }
        None => registry().specs.get(&r.decision).map(|l| l.spec.clone()).ok_or_else(|| ApiError(axum::http::StatusCode::NOT_FOUND, format!("no decision `{}`", r.decision)))?,
    };
    if spec.item.is_some() {
        return Err(ApiError(axum::http::StatusCode::BAD_REQUEST, format!("`{}` judges items; replay works on single decisions", r.decision)));
    }
    let compiled = policy::compile(&spec).map_err(|e| ApiError(axum::http::StatusCode::BAD_REQUEST, e))?
        .ok_or_else(|| ApiError(axum::http::StatusCode::BAD_REQUEST, format!("`{}` is information only: nothing to replay", r.decision)))?;
    let rows = sqlx::query(
        "SELECT d.id, d.answer, d.chosen, d.created_at, o.correct, o.truth FROM decisions d
         LEFT JOIN s1_outcomes o ON o.decision_id = d.id
         WHERE d.point = $1 AND d.source = 'model' AND d.answer IS NOT NULL ORDER BY d.id DESC LIMIT $2",
    )
    .bind(&r.decision)
    .bind(r.last.unwrap_or(500).clamp(1, 5000))
    .fetch_all(&app.db)
    .await?;
    let (mut same, mut changed, mut fixed, mut broke) = (0, 0, 0, 0);
    let mut examples = Vec::new();
    for row in &rows {
        let answer: Value = row.get("answer");
        let was: Option<String> = row.get("chosen");
        let now = policy::apply(&compiled, &answer, &policy::raw).map(|j| j.outcome).or_else(|| spec.unavailable.clone());
        if now == was {
            same += 1;
            continue;
        }
        changed += 1;
        let truth: Option<String> = row.get("truth");
        let correct: Option<bool> = row.get("correct");
        match (correct, &truth, &now) {
            (Some(false), Some(t), Some(n)) if t == n => fixed += 1,
            (Some(true), _, _) => broke += 1,
            _ => {}
        }
        if examples.len() < 10 {
            examples.push(json!({ "id": row.get::<i64, _>("id"), "was": was, "now": now, "truth": truth, "at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at") }));
        }
    }
    Ok(Json(json!({ "decision": r.decision, "replayed": rows.len(), "same": same, "changed": changed,
                    "now_right": fixed, "now_wrong": broke, "examples": examples })))
}

#[derive(Deserialize)]
pub(crate) struct Compare {
    decision: String,
    models: Vec<String>,
    #[serde(default)]
    last: Option<i64>,
}

/// Ask recorded states again on each approved model: agreement with what was decided and with
/// labelled outcomes, latency, cost. The answers become weak labels for the original decisions.
pub(crate) async fn compare(State(app): State<AppState>, Json(c): Json<Compare>) -> ApiResult<Json<Value>> {
    let ids: Vec<(String, String)> = {
        let r = registry();
        let mut out = Vec::new();
        for m in &c.models {
            let id = r.models.id(m).ok_or_else(|| ApiError(axum::http::StatusCode::BAD_REQUEST, format!("model `{m}` isn't on the approved list (zen s1 models)")))?;
            out.push((m.clone(), id.to_string()));
        }
        out
    };
    let rows = sqlx::query(
        "SELECT d.id, d.chosen, s.state, o.correct, o.truth FROM decisions d JOIN s1_states s ON s.decision_id = d.id
         LEFT JOIN s1_outcomes o ON o.decision_id = d.id AND o.source <> 'compare'
         WHERE d.point = $1 AND d.source = 'model' ORDER BY d.id DESC LIMIT $2",
    )
    .bind(&c.decision)
    .bind(c.last.unwrap_or(20).clamp(1, 200))
    .fetch_all(&app.db)
    .await?;
    let mut results = Vec::new();
    for (name, id) in &ids {
        let (mut asked, mut agree, mut right, mut labelled, mut failed, mut ms, mut cost) = (0, 0, 0, 0, 0, 0u128, 0.0);
        for row in &rows {
            let state: Value = row.get("state");
            match run::reask(&c.decision, id, &state).await {
                Ok((_, outcome, took, c)) => {
                    asked += 1;
                    ms += took.as_millis();
                    cost += c;
                    let chosen: Option<String> = row.get("chosen");
                    if outcome == chosen {
                        agree += 1;
                    }
                    let correct: Option<bool> = row.get("correct");
                    let truth: Option<String> = row.get("truth");
                    if let Some(correct) = correct {
                        labelled += 1;
                        let right_outcome = if correct { chosen.clone() } else { truth.clone() };
                        if right_outcome.is_some() && outcome == right_outcome {
                            right += 1;
                        }
                    }
                }
                Err(_) => failed += 1,
            }
        }
        results.push(json!({ "model": name, "id": id, "asked": asked, "failed": failed, "agree_with_decided": agree,
            "labelled": labelled, "right": right, "mean_ms": if asked > 0 { ms / asked as u128 } else { 0 }, "cost_usd": cost }));
    }
    Ok(Json(json!({ "decision": c.decision, "states": rows.len(), "models": results })))
}

pub(crate) async fn proposals(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(learn::proposals(&app.db).await?))
}

#[derive(Deserialize)]
pub(crate) struct Answer {
    decision: String,
}

pub(crate) async fn decide_proposal(State(app): State<AppState>, Path(id): Path<i64>, Json(a): Json<Answer>) -> ApiResult<Json<Value>> {
    let approve = match a.decision.as_str() {
        "approve" => true,
        "reject" => false,
        _ => return Err(ApiError(axum::http::StatusCode::BAD_REQUEST, "decision is approve or reject".into())),
    };
    learn::decide_proposal(&app.db, id, approve).await.map(Json).map_err(|e| ApiError(axum::http::StatusCode::CONFLICT, format!("{e:#}")))
}
