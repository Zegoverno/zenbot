//! What S1 learns from what really happened. Outcomes (`s1_outcomes`) come from sources of truth
//! zenbot already has: the owner approving or refusing an action a task check held, resuming or
//! removing a job S1 paused, and a second model's answer in `zen s1 compare` (a weak label).
//!
//! Nightly (inside the sleep job): for each yes/no decision with a cost policy and enough labelled
//! outcomes, fit a calibration curve (isotonic regression of "was yes right" on the probability)
//! and store it. A decision file uses its curve only when it says `calibrate = true`; otherwise the
//! learner proposes turning it on, with how many past outcomes it would have changed. Proposals
//! (`s1_proposals`) apply only when the owner approves them (`zen s1 proposals`, `zen s1 approve`).

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use super::policy::Compiled;

/// Labelled outcomes a curve needs before it's fitted.
pub const MIN_LABELLED: usize = 30;

type Key = (String, String, String);
/// (raw, calibrated) points of one curve.
type Curve = Vec<(f64, f64)>;
static CURVES: LazyLock<RwLock<HashMap<Key, Curve>>> = LazyLock::new(Default::default);

/// `p` through the decision's curve for this model and question (linear between points); `p`
/// itself without a curve.
pub fn calibrated(point: &str, model: &str, question: &str, p: f64) -> f64 {
    let curves = CURVES.read().unwrap_or_else(|e| e.into_inner());
    match curves.get(&(point.to_string(), model.to_string(), question.to_string())) {
        Some(c) => interpolate(c, p),
        None => p,
    }
}

fn interpolate(curve: &[(f64, f64)], p: f64) -> f64 {
    match curve {
        [] => p,
        [only] => only.1,
        _ => {
            if p <= curve[0].0 {
                return curve[0].1;
            }
            for w in curve.windows(2) {
                let ((x0, y0), (x1, y1)) = (w[0], w[1]);
                if p <= x1 {
                    return if x1 > x0 { y0 + (y1 - y0) * (p - x0) / (x1 - x0) } else { y1 };
                }
            }
            curve[curve.len() - 1].1
        }
    }
}

/// Isotonic regression (pool adjacent violators) of 0/1 truths on probabilities: a monotone curve
/// of (mean probability, observed rate) points.
pub fn isotonic(mut data: Vec<(f64, bool)>) -> Vec<(f64, f64)> {
    data.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Blocks: (sum of x, sum of y, count).
    let mut blocks: Vec<(f64, f64, f64)> = Vec::new();
    for (x, y) in data {
        blocks.push((x, if y { 1.0 } else { 0.0 }, 1.0));
        while blocks.len() > 1 {
            let n = blocks.len();
            let (a, b) = (blocks[n - 2], blocks[n - 1]);
            if a.1 / a.2 <= b.1 / b.2 {
                break;
            }
            blocks.truncate(n - 2);
            blocks.push((a.0 + b.0, a.1 + b.1, a.2 + b.2));
        }
    }
    blocks.into_iter().map(|(sx, sy, n)| (sx / n, sy / n)).collect()
}

/// Load every stored curve (at start and after fitting).
pub async fn load(db: &PgPool) {
    let rows = sqlx::query("SELECT point, model, question, curve FROM s1_calibration").fetch_all(db).await.unwrap_or_default();
    let mut map = HashMap::new();
    for r in rows {
        let curve: Value = r.get("curve");
        let points: Vec<(f64, f64)> = curve.as_array().into_iter().flatten().filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?))).collect();
        map.insert((r.get("point"), r.get("model"), r.get("question")), points);
    }
    *CURVES.write().unwrap_or_else(|e| e.into_inner()) = map;
}

/// Record what really happened to a decision. One label per source; a later one replaces it.
pub async fn label(db: &PgPool, decision: i64, correct: bool, truth: Option<&str>, source: &str) {
    let r = sqlx::query(
        "INSERT INTO s1_outcomes (decision_id, correct, truth, source) VALUES ($1, $2, $3, $4)
         ON CONFLICT (decision_id, source) DO UPDATE SET correct = $2, truth = $3, created_at = now()",
    )
    .bind(decision)
    .bind(correct)
    .bind(truth)
    .bind(source)
    .execute(db)
    .await;
    if let Err(e) = r {
        tracing::warn!("recording a System One outcome: {e}");
    }
}

/// The owner answered an action a task check held: approving it says the hold was wrong (the
/// action should have run), refusing or denying it says the hold was right.
pub async fn label_gate(db: &PgPool, gate: i64, approved: bool) {
    let decision: Option<i64> = sqlx::query_scalar("SELECT s1_decision FROM action_gates WHERE id = $1").bind(gate).fetch_optional(db).await.ok().flatten().flatten();
    if let Some(d) = decision {
        label(db, d, !approved, Some(if approved { "run" } else { "hold" }), "owner_queue").await;
    }
}

/// The owner resumed or removed a job S1 kept paused: resuming says it should have gone live.
pub async fn label_job(db: &PgPool, approval: &serde_json::Value, resumed: bool) {
    if approval["outcome"] != "paused" {
        return;
    }
    if let Some(d) = approval["decision"].as_i64() {
        label(db, d, !resumed, Some(if resumed { "live" } else { "paused" }), "owner_job").await;
    }
}

/// The nightly pass: fit curves, propose calibration where it would change outcomes, prune old
/// states. Returns a one-line summary for the sleep's note.
pub async fn nightly(db: &PgPool) -> Result<String> {
    let pruned = sqlx::query("DELETE FROM s1_states WHERE created_at < now() - interval '30 days'").execute(db).await?.rows_affected();
    let specs: Vec<(String, Compiled, bool)> = {
        let r = super::registry();
        r.specs.values().filter_map(|l| l.policy.clone().map(|p| (l.spec.name.clone(), p, l.spec.calibrate))).collect()
    };
    let (mut fitted, mut proposed) = (0, 0);
    for (point, policy, calibrate) in specs {
        let Compiled::Costs { question, yes, false_yes, false_no, .. } = &policy else { continue };
        // Raw probabilities (answers keep the model's own number) with whether yes was right.
        let rows = sqlx::query(
            "SELECT d.model, (d.answer -> $2 ->> 'probability')::float8 AS p, d.chosen, o.correct
             FROM decisions d JOIN s1_outcomes o ON o.decision_id = d.id
             WHERE d.point = $1 AND d.source = 'model' AND d.answer -> $2 ->> 'probability' IS NOT NULL",
        )
        .bind(&point)
        .bind(question)
        .fetch_all(db)
        .await?;
        let mut by_model: HashMap<String, Vec<(f64, bool)>> = HashMap::new();
        for r in rows {
            let (Some(model), Some(p), Some(chosen)) = (r.get::<Option<String>, _>("model"), r.get::<Option<f64>, _>("p"), r.get::<Option<String>, _>("chosen")) else { continue };
            let correct: bool = r.get("correct");
            let yes_was_right = if chosen == *yes { correct } else { !correct };
            by_model.entry(model).or_default().push((p, yes_was_right));
        }
        for (model, data) in by_model {
            if data.len() < MIN_LABELLED {
                continue;
            }
            let curve = isotonic(data.clone());
            sqlx::query(
                "INSERT INTO s1_calibration (point, model, question, curve, labelled) VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (point, model, question) DO UPDATE SET curve = $4, labelled = $5, fitted_at = now()",
            )
            .bind(&point)
            .bind(&model)
            .bind(question)
            .bind(json!(curve.iter().map(|(x, y)| [x, y]).collect::<Vec<_>>()))
            .bind(data.len() as i32)
            .execute(db)
            .await?;
            fitted += 1;
            if !calibrate {
                let threshold = false_yes / (false_yes + false_no);
                let changed = data.iter().filter(|(p, _)| (*p >= threshold) != (interpolate(&curve, *p) >= threshold)).count();
                let better = data.iter().filter(|(p, truth)| ((interpolate(&curve, *p) >= threshold) == *truth) && ((*p >= threshold) != *truth)).count();
                if changed > 0 {
                    let summary = format!("{point}: calibrating {model} would have changed {changed} of {} labelled decisions ({better} of them to the right outcome)", data.len());
                    let r = sqlx::query("INSERT INTO s1_proposals (point, kind, summary, change) VALUES ($1, 'calibrate', $2, $3) ON CONFLICT DO NOTHING")
                        .bind(&point)
                        .bind(&summary)
                        .bind(json!({ "set": { "calibrate": true } }))
                        .execute(db)
                        .await?;
                    proposed += r.rows_affected();
                }
            }
        }
    }
    load(db).await;
    Ok(format!("System One: {fitted} curves fitted, {proposed} proposals, {pruned} old states pruned"))
}

/// Pending proposals, newest first.
pub async fn proposals(db: &PgPool) -> Result<Value> {
    let rows = sqlx::query("SELECT id, point, kind, summary, change, created_at FROM s1_proposals WHERE status = 'pending' ORDER BY id DESC").fetch_all(db).await?;
    Ok(Value::Array(rows.iter().map(|r| json!({
        "id": r.get::<i64, _>("id"), "decision": r.get::<String, _>("point"), "kind": r.get::<String, _>("kind"),
        "summary": r.get::<String, _>("summary"), "change": r.get::<Value, _>("change"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
    })).collect()))
}

/// The owner's answer to a proposal. Approving writes the change into the owner's decision file
/// (copied from the built-in default first when they have none), then checks it; a file that
/// fails is put back as it was and the proposal stays pending.
pub async fn decide_proposal(db: &PgPool, id: i64, approve: bool) -> Result<Value> {
    let row = sqlx::query("SELECT point, change, status FROM s1_proposals WHERE id = $1").bind(id).fetch_optional(db).await?.context("no such proposal")?;
    let status: String = row.get("status");
    if status != "pending" {
        bail!("proposal {id} is already {status}");
    }
    let point: String = row.get("point");
    if approve {
        let change: Value = row.get("change");
        apply_change(&point, &change)?;
    }
    sqlx::query("UPDATE s1_proposals SET status = $2, decided_at = now() WHERE id = $1 AND status = 'pending'")
        .bind(id)
        .bind(if approve { "approved" } else { "rejected" })
        .execute(db)
        .await?;
    Ok(json!({ "id": id, "decision": point, "status": if approve { "approved" } else { "rejected" } }))
}

/// Set top-level keys in the owner's file (`key = value` lines, comments kept).
fn apply_change(point: &str, change: &Value) -> Result<()> {
    let path = super::dir().join(format!("{point}.toml"));
    let before = std::fs::read_to_string(&path).ok();
    let mut text = match &before {
        Some(t) => t.clone(),
        None => super::DEFAULTS.iter().find(|(n, _)| *n == point).map(|(_, s)| s.to_string()).context("no such decision")?,
    };
    for (key, value) in change["set"].as_object().into_iter().flatten() {
        let rendered = match value {
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => format!("{s:?}"),
            other => bail!("can't write {other} into a decision file"),
        };
        text = set_top_level(&text, key, &rendered);
    }
    let models = super::registry().models.clone();
    let (spec, diags) = super::check::check(point, &text, &models);
    if spec.is_none() {
        bail!("the change doesn't pass the checker: {}", super::check::render(&path.display().to_string(), &diags));
    }
    std::fs::create_dir_all(super::dir())?;
    std::fs::write(&path, text)?;
    super::reload();
    Ok(())
}

/// Replace `key = …` before the first table, or add it there.
pub fn set_top_level(src: &str, key: &str, value: &str) -> String {
    let mut out = Vec::new();
    let mut done = false;
    let mut in_top = true;
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with('[') && in_top {
            if !done {
                out.push(format!("{key} = {value}"));
                done = true;
            }
            in_top = false;
        }
        if in_top && !done && t.split('=').next().map(str::trim) == Some(key) && t.contains('=') {
            out.push(format!("{key} = {value}"));
            done = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !done {
        out.push(format!("{key} = {value}"));
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isotonic_is_monotone_and_fits_rates() {
        let mut data = Vec::new();
        for i in 0..100 {
            let p = i as f64 / 100.0;
            // An overconfident model: right only half as often as it says above 0.5.
            data.push((p, (i % 2 == 0) && p > 0.5));
        }
        let c = isotonic(data);
        assert!(c.windows(2).all(|w| w[0].1 <= w[1].1), "{c:?}");
        assert!(calibrated_on(&c, 0.95) < 0.7, "0.95 calibrates down: {}", calibrated_on(&c, 0.95));
        assert!(calibrated_on(&c, 0.2) < 0.05);
    }

    fn calibrated_on(c: &[(f64, f64)], p: f64) -> f64 {
        interpolate(c, p)
    }

    /// Isolated DB only: SOURCE_TEST_DB_URL=… cargo test s1_learn_database -- --ignored.
    #[tokio::test]
    #[ignore]
    async fn s1_learn_database() {
        let db = PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").expect("a throwaway database")).await.unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        let model = format!("test/{}", uuid::Uuid::new_v4().simple());
        // An overconfident model: it says 0.95 that the owner asked for a job, and is right half the time.
        for i in 0..40 {
            let (p, chosen, correct) = if i < 20 { (0.95, "live", i % 2 == 0) } else { (0.3, "paused", i % 10 != 0) };
            let id: i64 = sqlx::query_scalar("INSERT INTO decisions (point, model, answer, chosen, source) VALUES ('job_approval', $1, $2, $3, 'model') RETURNING id")
                .bind(&model).bind(json!({ "wanted": { "type": "bool", "probability": p } })).bind(chosen).fetch_one(&db).await.unwrap();
            label(&db, id, correct, None, "test").await;
        }
        let summary = nightly(&db).await.unwrap();
        assert!(summary.contains("curves fitted"), "{summary}");
        let labelled: i32 = sqlx::query_scalar("SELECT labelled FROM s1_calibration WHERE point = 'job_approval' AND model = $1").bind(&model).fetch_one(&db).await.unwrap();
        assert_eq!(labelled, 40);
        assert!(calibrated("job_approval", &model, "wanted", 0.95) < 0.9, "the curve brings 0.95 below the bar");
        let proposals = proposals(&db).await.unwrap();
        assert!(proposals.as_array().unwrap().iter().any(|p| p["decision"] == "job_approval" && p["kind"] == "calibrate"), "{proposals}");
        // The owner approving an action a task check held says the hold was wrong.
        let session = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, title, model) VALUES ($1, 's1 learn', 'faux/smoke')").bind(session).execute(&db).await.unwrap();
        let held: i64 = sqlx::query_scalar("INSERT INTO decisions (session_id, point, chosen, source) VALUES ($1, 'task_check', 'hold', 'model') RETURNING id").bind(session).fetch_one(&db).await.unwrap();
        let gate: i64 = sqlx::query_scalar("INSERT INTO action_gates (session_id, action, reason, s1_decision) VALUES ($1, '{}', 'task check: unrelated', $2) RETURNING id")
            .bind(session).bind(held).fetch_one(&db).await.unwrap();
        label_gate(&db, gate, true).await;
        let (correct, truth): (bool, Option<String>) = sqlx::query_as("SELECT correct, truth FROM s1_outcomes WHERE decision_id = $1").bind(held).fetch_one(&db).await.unwrap();
        assert_eq!((correct, truth.as_deref()), (false, Some("run")));
        db.close().await;
    }

    #[test]
    fn owner_files_change_one_line() {
        let src = "name = \"x\"\nversion = 1\n# a comment\n[policy]\nkind = \"costs\"\n";
        let out = set_top_level(src, "calibrate", "true");
        assert_eq!(out, "name = \"x\"\nversion = 1\n# a comment\ncalibrate = true\n[policy]\nkind = \"costs\"\n");
        assert_eq!(set_top_level(&out, "calibrate", "false").matches("calibrate").count(), 1);
    }
}
