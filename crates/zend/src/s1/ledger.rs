//! The record of every System One decision (`decisions`, one row per call, success or not): the
//! decision file and its fingerprint, the model and rung, the answers, the outcome, latency, cost.
//! The ledger's `input` holds the evidence itself only for owner and metadata decisions; private
//! and outside evidence appear by size and digest. The masked state of every decision is kept 30
//! days in `s1_states`, so `zen s1 compare` can ask other models the same thing (pruned nightly).

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use uuid::Uuid;

use super::spec::{Exposure, Spec};
use super::Loaded;

pub struct Record {
    pub session: Option<Uuid>,
    pub point: String,
    pub spec_sha: String,
    pub version: i32,
    pub model: Option<String>,
    pub input: Value,
    pub answer: Value,
    pub chosen: Option<String>,
    pub probability: Option<f64>,
    pub error: Option<String>,
    pub rung: Option<i32>,
    pub latency_ms: i64,
    pub cost: Option<f64>,
    pub tokens: Option<i64>,
    /// `model`, `unavailable` or `blocked`.
    pub source: String,
    pub items: Option<i32>,
}

impl Record {
    pub fn new(l: &Loaded, session: Option<Uuid>) -> Record {
        Record {
            session,
            point: l.spec.name.clone(),
            spec_sha: l.sha.clone(),
            version: l.spec.version as i32,
            model: None,
            input: Value::Null,
            answer: Value::Null,
            chosen: None,
            probability: None,
            error: None,
            rung: None,
            latency_ms: 0,
            cost: None,
            tokens: None,
            source: "model".into(),
            items: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill(&mut self, models: &[String], rung: usize, cost: f64, tokens: i64, answers: &Value, chosen: Option<String>, p: Option<f64>) {
        self.model = Some(models.join("+"));
        self.rung = Some(rung as i32);
        self.cost = Some(cost);
        self.tokens = (tokens > 0).then_some(tokens);
        self.answer = answers.clone();
        self.chosen = chosen;
        self.probability = p;
    }
}

/// What the ledger keeps of the evidence: owner and metadata facts as they are, others by size and
/// digest (the full masked state goes to `s1_states` for 30 days).
pub fn input_of(spec: &Spec, state: &Value) -> Value {
    if matches!(spec.exposure, Exposure::Owner | Exposure::Metadata) {
        return state.clone();
    }
    let Some(o) = state.as_object() else { return Value::Null };
    let digest: Map<String, Value> = o
        .iter()
        .map(|(k, v)| {
            let text = v.to_string();
            (k.clone(), json!({ "chars": text.len(), "sha256": format!("{:x}", Sha256::digest(text.as_bytes()))[..16].to_string() }))
        })
        .collect();
    Value::Object(digest)
}

/// Write the record; returns its id. Never fails the decision: a database error is logged.
pub async fn record(db: &SqlitePool, r: &Record, state: Option<&Value>) -> Option<i64> {
    let res: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "INSERT INTO decisions (session_id, point, model, input, answer, chosen, probability, acted, error,
                                spec_sha, spec_version, rung, latency_ms, cost_usd, tokens, source, items)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) RETURNING id",
    )
    .bind(r.session)
    .bind(&r.point)
    .bind(&r.model)
    .bind(&r.input)
    .bind(&r.answer)
    .bind(&r.chosen)
    .bind(r.probability)
    .bind(r.chosen.is_some() && r.source == "model")
    .bind(&r.error)
    .bind(&r.spec_sha)
    .bind(r.version)
    .bind(r.rung)
    .bind(r.latency_ms as i32)
    .bind(r.cost)
    .bind(r.tokens)
    .bind(&r.source)
    .bind(r.items)
    .fetch_one(db)
    .await;
    match res {
        Ok(id) => {
            if let Some(state) = state {
                if let Err(e) = sqlx::query("INSERT INTO s1_states (decision_id, state) VALUES ($1, $2)").bind(id).bind(state).execute(db).await {
                    tracing::warn!("recording System One state: {e}");
                }
            }
            Some(id)
        }
        Err(e) => {
            tracing::warn!("recording System One decision `{}`: {e}", r.point);
            None
        }
    }
}
