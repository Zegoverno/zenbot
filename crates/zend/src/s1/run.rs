//! Running a decision: evidence to state, the model ladder, the policy, the record.
//!
//! - The state holds only the file's `evidence` facts, every string masked for secrets, at most
//!   `max_state_chars`; a choice's `options_from` list comes from the facts.
//! - The whole decision has the file's `timeout`; each rung gets what's left of it.
//! - Rungs: the ladder's models in order. A model that fails, times out or answers malformed
//!   passes to the next; with `ensemble = n` the first n are asked together and averaged. When the
//!   policy is unsure (`escalate`) or an ensemble disagrees, the next rung is asked; `owner` as the
//!   last rung makes the outcome `ask_owner`. Items and batches use the ladder only on failure.
//! - A circuit breaker: after 5 transport failures in a row every call fails at once for 60 s, so a
//!   down provider costs callers nothing but their `unavailable` outcome. At most
//!   `ZEN_S1_CONCURRENCY` calls are in flight.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::catalog;
use super::ledger::{self, Record};
use super::policy::{self, Judged};
use super::spec::{Exposure, QuestionKind, Spec};
use super::{learn, registry, wire, Decision, Facts, Loaded};


static PERMITS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(crate::config::get().s1_concurrency));

struct Breaker {
    failures: u32,
    open_until: Option<Instant>,
}
static BREAKER: Mutex<Breaker> = Mutex::new(Breaker { failures: 0, open_until: None });
const BREAKER_AFTER: u32 = 5;
const BREAKER_OPEN: Duration = Duration::from_secs(60);

fn breaker_open() -> bool {
    let b = BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    b.open_until.is_some_and(|t| Instant::now() < t)
}

fn breaker_result(ok: bool) {
    let mut b = BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    if ok {
        b.failures = 0;
        b.open_until = None;
    } else {
        b.failures += 1;
        if b.failures >= BREAKER_AFTER {
            if b.open_until.is_none_or(|t| Instant::now() >= t) {
                tracing::warn!("System One failed {} times in a row: every decision takes its fallback for {}s", b.failures, BREAKER_OPEN.as_secs());
            }
            b.open_until = Some(Instant::now() + BREAKER_OPEN);
        }
    }
}

#[cfg(test)]
pub fn reset_breaker() {
    breaker_result(true);
}

/// One model call within the decision's deadline, through the breaker and the cap.
async fn call(model: &str, state: &Value, questions: &Value, deadline: Instant) -> Result<wire::Reply, String> {
    if breaker_open() {
        return Err("System One is failing; skipped for a minute (circuit breaker)".into());
    }
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("timed out".into());
    }
    let _permit = match tokio::time::timeout(left, PERMITS.acquire()).await {
        Ok(Ok(p)) => p,
        _ => return Err("timed out waiting for a free System One slot".into()),
    };
    let left = deadline.saturating_duration_since(Instant::now());
    match tokio::time::timeout(left, wire::call(model, state, questions)).await {
        Ok(Ok(r)) => {
            breaker_result(true);
            Ok(r)
        }
        Ok(Err(e)) => {
            // A malformed answer is the model's fault, not the provider's: it doesn't trip the breaker.
            let transport = format!("{e:#}");
            if !transport.contains("did not answer") && !transport.contains("invalid") {
                breaker_result(false);
            }
            Err(transport)
        }
        Err(_) => {
            breaker_result(false);
            Err("timed out".into())
        }
    }
}

/// Every string in a value, masked for secrets.
fn mask(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(crate::secrets::mask(s)),
        Value::Array(a) => Value::Array(a.iter().map(mask).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), mask(v))).collect()),
        other => other.clone(),
    }
}

/// The state: the file's evidence facts only.
fn state_of(spec: &Spec, facts: &Facts) -> Map<String, Value> {
    spec.evidence.iter().filter_map(|k| facts.map.get(k).map(|v| (k.clone(), mask(v)))).collect()
}

/// A question as the wire wants it.
fn wire_question(q: &super::spec::Question, text: &str, facts: &Facts) -> Value {
    match q.kind {
        QuestionKind::YesNo => json!({ "type": "bool", "instructions": text,
            "criteria": { "true": q.yes.clone().unwrap_or_else(|| "Yes".into()), "false": q.no.clone().unwrap_or_else(|| "No".into()) } }),
        QuestionKind::Choice => {
            let options = match (&q.options, &q.options_from) {
                (Some(o), _) => json!(o),
                (None, Some(f)) => facts.map.get(f).cloned().unwrap_or_else(|| json!({})),
                _ => json!({}),
            };
            json!({ "type": "choice", "instructions": text, "criteria": options })
        }
        QuestionKind::Score => json!({ "type": "score", "instructions": text, "criteria": q.levels.clone().unwrap_or_default() }),
    }
}

/// Average several models' answers to the same questions.
fn combine(all: &[Value]) -> Value {
    if all.len() == 1 {
        return all[0].clone();
    }
    let mut out = Map::new();
    let Some(first) = all[0].as_object() else { return all[0].clone() };
    for (q, a) in first {
        let same: Vec<&Value> = all.iter().map(|x| &x[q]).filter(|x| x["type"] == a["type"]).collect();
        let mean = |f: &dyn Fn(&Value) -> Option<f64>| {
            let v: Vec<f64> = same.iter().filter_map(|x| f(x)).collect();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        };
        let merged = match a["type"].as_str() {
            Some("bool") => json!({ "type": "bool", "probability": mean(&|x| x["probability"].as_f64()) }),
            Some("score") => json!({ "type": "score", "score": mean(&|x| x["score"].as_f64()), "confidence": mean(&|x| x["confidence"].as_f64()) }),
            Some("choice") => {
                let mut probs: Map<String, Value> = Map::new();
                let options: std::collections::BTreeSet<String> = same.iter().flat_map(|x| x["probabilities"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()).collect();
                for o in &options {
                    probs.insert(o.clone(), json!(mean(&|x| Some(x["probabilities"][o].as_f64().unwrap_or(0.0))).unwrap_or(0.0)));
                }
                let choice = probs.iter().max_by(|a, b| a.1.as_f64().partial_cmp(&b.1.as_f64()).unwrap_or(std::cmp::Ordering::Equal)).map(|(k, _)| k.clone())
                    .or_else(|| a["choice"].as_str().map(String::from));
                json!({ "type": "choice", "choice": choice, "probabilities": probs, "confidence": mean(&|x| x["confidence"].as_f64()) })
            }
            _ => a.clone(),
        };
        out.insert(q.clone(), merged);
    }
    Value::Object(out)
}

/// Why a decision can't run at all: System One off, or privacy blocks its evidence.
fn blocked(spec: &Spec) -> Option<&'static str> {
    if !registry().models.on() {
        return Some("off");
    }
    let private = matches!(spec.exposure, Exposure::Private | Exposure::Outside | Exposure::Information);
    (private && !super::private_ok()).then_some("blocked")
}

fn unavailable(l: &Loaded, error: String) -> Decision {
    Decision { outcome: l.spec.unavailable.clone(), error: Some(error), ..Default::default() }
}

fn spec_of(name: &str) -> Option<Loaded> {
    let r = registry();
    r.specs.get(name).cloned()
}

fn model_ids(l: &Loaded) -> Vec<String> {
    let r = registry();
    l.spec.models().filter_map(|m| r.models.id(m).map(String::from)).collect()
}

struct Asked {
    answers: Value,
    models: Vec<String>,
    rung: usize,
    cost: f64,
    tokens: i64,
    judged: Option<Judged>,
    ask_owner: bool,
}

/// The ladder for one state and set of questions. `judge` applies the policy (None: information).
async fn ask(l: &Loaded, state: &Value, questions: &Value, deadline: Instant, escalating: bool,
             judge: &(dyn Fn(&Value, &str) -> Option<Judged> + Sync)) -> Result<Asked, String> {
    let ids = model_ids(l);
    if ids.is_empty() {
        return Err("no approved model in the ladder".into());
    }
    let group = if escalating { l.spec.ensemble.max(1) } else { 1 };
    let owner_last = l.spec.ladder.last().is_some_and(|r| r == "owner");
    let (mut cost, mut tokens, mut last_err) = (0.0, 0i64, String::new());
    let mut i = 0;
    while i < ids.len() {
        let rung: Vec<String> = ids[i..(i + group).min(ids.len())].to_vec();
        let replies = join_all(rung.iter().map(|m| call(m, state, questions, deadline))).await;
        let mut oks = Vec::new();
        let mut answered_by = Vec::new();
        for (m, r) in rung.iter().zip(replies) {
            match r {
                Ok(r) => {
                    cost += r.usage["cost"].as_f64().unwrap_or(0.0);
                    tokens += r.usage["total_tokens"].as_i64().unwrap_or(0);
                    // The configured id, not the provider's resolved name: calibration curves and
                    // `compare` key on what the owner approved.
                    answered_by.push(m.clone());
                    oks.push(r.answers);
                }
                Err(e) => last_err = format!("{m}: {e}"),
            }
        }
        let next = i + rung.len();
        if oks.is_empty() {
            i = next;
            continue;
        }
        let answers = combine(&oks);
        let calibrated_model = rung[0].clone();
        let judged = judge(&answers, &calibrated_model);
        if !escalating {
            return Ok(Asked { answers, models: answered_by, rung: i, cost, tokens, judged, ask_owner: false });
        }
        let disagree = oks.len() > 1 && {
            let outs: Vec<Option<String>> = oks.iter().map(|a| judge(a, &calibrated_model).map(|j| j.outcome)).collect();
            outs.windows(2).any(|w| w[0] != w[1])
        };
        let unsure = judged.as_ref().is_some_and(|j| j.unsure) || disagree;
        if judged.is_some() && unsure && next < ids.len() {
            i = next;
            continue;
        }
        if judged.is_none() && l.policy.is_some() {
            // The answers the policy needs are missing: treat like a failed rung.
            last_err = format!("{}: answers missing what the policy needs", rung.join("+"));
            i = next;
            continue;
        }
        return Ok(Asked { answers, models: answered_by, rung: i, cost, tokens, judged, ask_owner: unsure && owner_last });
    }
    Err(if last_err.is_empty() { "no model answered".into() } else { last_err })
}

fn calibrator(l: &Loaded) -> impl Fn(&str, &str, f64) -> f64 + '_ {
    move |model: &str, question: &str, p: f64| if l.spec.calibrate { learn::calibrated(&l.spec.name, model, question, p) } else { p }
}

pub async fn single(db: &sqlx::PgPool, name: &str, facts: Facts) -> Decision {
    let Some(l) = spec_of(name) else { return Decision { error: Some(format!("no decision `{name}`")), ..Default::default() } };
    // Rules on the caller's inputs alone decide without a model (and without a record): most
    // low-risk decisions end here, even with System One off.
    let inputs: Map<String, Value> = l.spec.inputs.iter().filter_map(|k| facts.map.get(k).map(|v| (k.clone(), v.clone()))).collect();
    let inputs = super::rules::facts_as_answers(&inputs);
    if let Some(outcome) = l.policy.as_ref().and_then(|p| policy::decide_early(p, &inputs)) {
        return Decision { outcome: Some(outcome), ..Default::default() };
    }
    let started = Instant::now();
    let mut rec = Record::new(&l, facts.session);
    if let Some(why) = blocked(&l.spec) {
        if why != "off" {
            rec.source = why.into();
            rec.error = Some("ZEN_S1_PRIVATE=0 keeps this evidence from System One".into());
            rec.chosen = l.spec.unavailable.clone();
            ledger::record(db, &rec, None).await;
        }
        return unavailable(&l, format!("System One {why}"));
    }
    let state = Value::Object(state_of(&l.spec, &facts));
    let questions: Value = Value::Object(l.spec.question.iter().map(|(k, q)| (k.clone(), wire_question(q, &q.text, &facts))).collect());
    rec.input = ledger::input_of(&l.spec, &state);
    let size = state.to_string().len();
    let deadline = started + l.spec.timeout().unwrap_or(Duration::from_secs(30));
    let cal = calibrator(&l);
    let judge = |a: &Value, model: &str| {
        // The rules read the inputs beside the answers.
        let mut with_inputs = a.clone();
        if let (Some(w), Some(i)) = (with_inputs.as_object_mut(), inputs.as_object()) {
            for (k, v) in i {
                w.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        l.policy.as_ref().and_then(|p| policy::apply(p, &with_inputs, &|q, p| cal(model, q, p)))
    };
    let result = if size > l.spec.max_state_chars {
        Err(format!("state too large ({size} > {} characters)", l.spec.max_state_chars))
    } else {
        ask(&l, &state, &questions, deadline, true, &judge).await
    };
    rec.latency_ms = started.elapsed().as_millis() as i64;
    let mut d = match result {
        Ok(a) => {
            let outcome = if a.ask_owner { Some("ask_owner".to_string()) } else { a.judged.as_ref().map(|j| j.outcome.clone()) };
            rec.fill(&a.models, a.rung, a.cost, a.tokens, &a.answers, outcome.clone(), a.judged.as_ref().and_then(|j| j.p));
            Decision { outcome, answers: a.answers, p: a.judged.and_then(|j| j.p), answered: true, model: a.models.first().cloned(), error: None, id: None }
        }
        Err(e) => {
            rec.source = "unavailable".into();
            rec.error = Some(e.clone());
            rec.chosen = l.spec.unavailable.clone();
            unavailable(&l, e)
        }
    };
    d.id = ledger::record(db, &rec, Some(&state)).await;
    d
}

pub async fn items(db: &sqlx::PgPool, name: &str, facts: Facts, items: Vec<Value>) -> Vec<Decision> {
    let n = items.len();
    let Some(l) = spec_of(name) else { return vec![Decision::default(); n] };
    let (qname, q) = match l.spec.question.iter().next() {
        Some((k, q)) => (k.clone(), q.clone()),
        None => return vec![Decision::default(); n],
    };
    let none = |l: &Loaded, e: String| (0..n).map(|_| unavailable(l, e.clone())).collect::<Vec<_>>();
    if n == 0 {
        return Vec::new();
    }
    let started = Instant::now();
    let mut rec = Record::new(&l, facts.session);
    rec.items = Some(n as i32);
    if let Some(why) = blocked(&l.spec) {
        if why != "off" {
            rec.source = why.into();
            rec.error = Some("ZEN_S1_PRIVATE=0 keeps this evidence from System One".into());
            ledger::record(db, &rec, None).await;
        }
        return none(&l, format!("System One {why}"));
    }
    let prefix = l.spec.item.clone().unwrap_or_else(|| "item".into());
    let mut state = state_of(&l.spec, &facts);
    let mut questions = Map::new();
    for (i, item) in items.into_iter().enumerate() {
        let key = format!("{prefix}_{i}");
        questions.insert(format!("q{i}"), wire_question(&q, &q.text.replace("{item}", &key), &facts));
        state.insert(key, mask(&item));
    }
    let state = Value::Object(state);
    let questions = Value::Object(questions);
    rec.input = ledger::input_of(&l.spec, &state);
    let deadline = started + l.spec.timeout().unwrap_or(Duration::from_secs(30));
    let size = state.to_string().len();
    let result = if size > l.spec.max_state_chars {
        Err(format!("state too large ({size} > {} characters)", l.spec.max_state_chars))
    } else {
        ask(&l, &state, &questions, deadline, false, &|_, _| None).await
    };
    rec.latency_ms = started.elapsed().as_millis() as i64;
    let out = match result {
        Ok(a) => {
            let cal = calibrator(&l);
            let model = a.models.first().cloned().unwrap_or_default();
            let per: Vec<Decision> = (0..n)
                .map(|i| {
                    let answers = match a.answers.get(format!("q{i}")) {
                        Some(x) => json!({ qname.clone(): x }),
                        None => json!({}),
                    };
                    let judged = l.policy.as_ref().and_then(|p| policy::apply(p, &answers, &|qq, p| cal(&model, qq, p)));
                    let outcome = judged.as_ref().map(|j| j.outcome.clone()).or_else(|| if l.policy.is_some() { l.spec.unavailable.clone() } else { None });
                    let answered = answers.as_object().is_some_and(|o| !o.is_empty());
                    Decision { outcome, p: judged.and_then(|j| j.p), answers, answered, model: Some(model.clone()), error: None, id: None }
                })
                .collect();
            let summary = summarize(&per);
            rec.fill(&a.models, a.rung, a.cost, a.tokens, &a.answers, summary, None);
            per
        }
        Err(e) => {
            rec.source = "unavailable".into();
            rec.error = Some(e.clone());
            none(&l, e)
        }
    };
    let id = ledger::record(db, &rec, Some(&state)).await;
    out.into_iter().map(|d| Decision { id, ..d }).collect()
}

/// "keep 3, drop 2" (outcomes) or "3 of 5 likely" (yes/no answers at 0.5).
fn summarize(per: &[Decision]) -> Option<String> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for d in per {
        if let Some(o) = &d.outcome {
            *counts.entry(o.clone()).or_default() += 1;
        }
    }
    if !counts.is_empty() {
        return Some(counts.iter().map(|(o, n)| format!("{o} {n}")).collect::<Vec<_>>().join(", "));
    }
    let yes = per.iter().filter(|d| d.answers.as_object().and_then(|o| o.values().next()).and_then(|a| a["probability"].as_f64()).is_some_and(|p| p >= 0.5)).count();
    Some(format!("{yes} of {} likely", per.len()))
}

pub async fn batch(db: &sqlx::PgPool, name: &str, session: Option<Uuid>, subjects: Vec<Facts>) -> Vec<Decision> {
    let n = subjects.len();
    let Some(l) = spec_of(name) else { return vec![Decision::default(); n] };
    if n == 0 {
        return Vec::new();
    }
    let none = |l: &Loaded, e: String| (0..n).map(|_| unavailable(l, e.clone())).collect::<Vec<_>>();
    let started = Instant::now();
    let mut rec = Record::new(&l, session);
    rec.items = Some(n as i32);
    if let Some(why) = blocked(&l.spec) {
        if why != "off" {
            rec.source = why.into();
            ledger::record(db, &rec, None).await;
        }
        return none(&l, format!("System One {why}"));
    }
    let mut state = Map::new();
    let mut questions = Map::new();
    for (i, f) in subjects.iter().enumerate() {
        state.insert(i.to_string(), Value::Object(state_of(&l.spec, f)));
        for (k, q) in &l.spec.question {
            // Each question names its subject, so one call can carry several.
            questions.insert(format!("{i}_{k}"), wire_question(q, &format!("For source {i} in state only. {}", q.text), f));
        }
    }
    let state = Value::Object(state);
    let questions = Value::Object(questions);
    rec.input = ledger::input_of(&l.spec, &state);
    let deadline = started + l.spec.timeout().unwrap_or(Duration::from_secs(30));
    let result = ask(&l, &state, &questions, deadline, false, &|_, _| None).await;
    rec.latency_ms = started.elapsed().as_millis() as i64;
    let out = match result {
        Ok(a) => {
            let cal = calibrator(&l);
            let model = a.models.first().cloned().unwrap_or_default();
            let per: Vec<Decision> = (0..n)
                .map(|i| {
                    let answers: Map<String, Value> = l.spec.question.keys().filter_map(|k| a.answers.get(format!("{i}_{k}")).map(|x| (k.clone(), x.clone()))).collect();
                    let answers = Value::Object(answers);
                    let judged = l.policy.as_ref().and_then(|p| policy::apply(p, &answers, &|qq, p| cal(&model, qq, p)));
                    let outcome = judged.as_ref().map(|j| j.outcome.clone()).or_else(|| l.spec.unavailable.clone());
                    Decision { outcome, p: judged.and_then(|j| j.p), answers, answered: true, model: Some(model.clone()), error: None, id: None }
                })
                .collect();
            let summary = summarize(&per);
            rec.fill(&a.models, a.rung, a.cost, a.tokens, &a.answers, summary, None);
            per
        }
        Err(e) => {
            rec.source = "unavailable".into();
            rec.error = Some(e.clone());
            none(&l, e)
        }
    };
    let id = ledger::record(db, &rec, Some(&state)).await;
    out.into_iter().map(|d| Decision { id, ..d }).collect()
}

/// The brain's `judge` tool: one ad-hoc question over items, through the built-in `judge` file
/// (its ladder, timeout and size limit). Information only: answers and probabilities, no outcome.
pub async fn judge(db: &sqlx::PgPool, session: Uuid, question: &str, kind: &str, options: Option<&Value>, items: Vec<Value>, context: Option<&str>) -> Result<Vec<Decision>, String> {
    let Some(l) = spec_of("judge") else { return Err("the judge decision is missing".into()) };
    if let Some(why) = blocked(&l.spec) {
        return Err(match why {
            "off" => "System One isn't configured (ZEN_S1_MODEL or ~/.zenbot/s1/models.toml)".into(),
            _ => "ZEN_S1_PRIVATE=0 keeps the brain's material from System One".into(),
        });
    }
    let max_items = 200;
    if items.is_empty() || items.len() > max_items {
        return Err(format!("give 1 to {max_items} items"));
    }
    let q = match kind {
        "yes/no" => super::spec::Question { kind: QuestionKind::YesNo, text: question.into(), yes: None, no: None, options: None, options_from: None, levels: None },
        "choice" => {
            let opts: std::collections::BTreeMap<String, String> = match options {
                Some(Value::Array(a)) => a.iter().filter_map(|o| o.as_str()).map(|o| (o.to_string(), o.to_string())).collect(),
                Some(Value::Object(o)) => o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or(k).to_string())).collect(),
                _ => Default::default(),
            };
            if opts.len() < 2 {
                return Err("a choice needs at least two `options`".into());
            }
            super::spec::Question { kind: QuestionKind::Choice, text: question.into(), yes: None, no: None, options: Some(opts), options_from: None, levels: None }
        }
        "score" => {
            let levels: Vec<String> = options.and_then(|o| o.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            if levels.len() < 2 {
                return Err("a score needs `options`: at least two levels, lowest first".into());
            }
            super::spec::Question { kind: QuestionKind::Score, text: question.into(), yes: None, no: None, options: None, options_from: None, levels: Some(levels) }
        }
        other => return Err(format!("type `{other}`: use yes/no, choice or score")),
    };
    let mut adhoc = l.clone();
    adhoc.spec.question = std::collections::BTreeMap::from([("answer".to_string(), super::spec::Question { text: format!("{} (about {{item}})", q.text), ..q })]);
    let facts = Facts::new(Some(session)).with("context", context.unwrap_or(""));
    let n = items.len();
    let started = Instant::now();
    let mut rec = Record::new(&adhoc, Some(session));
    rec.items = Some(n as i32);
    let qd = adhoc.spec.question["answer"].clone();
    let mut state = state_of(&adhoc.spec, &facts);
    let mut questions = Map::new();
    for (i, item) in items.into_iter().enumerate() {
        let key = format!("item_{i}");
        questions.insert(format!("q{i}"), wire_question(&qd, &qd.text.replace("{item}", &key), &facts));
        state.insert(key, mask(&item));
    }
    let state = Value::Object(state);
    let questions = Value::Object(questions);
    rec.input = json!({ "question": question, "type": kind, "items": n });
    let size = state.to_string().len();
    if size > adhoc.spec.max_state_chars {
        return Err(format!("the items are too long together ({size} characters; at most {})", adhoc.spec.max_state_chars));
    }
    let deadline = started + adhoc.spec.timeout().unwrap_or(Duration::from_secs(30));
    let result = ask(&adhoc, &state, &questions, deadline, false, &|_, _| None).await;
    rec.latency_ms = started.elapsed().as_millis() as i64;
    match result {
        Ok(a) => {
            let model = a.models.first().cloned();
            let per: Vec<Decision> = (0..n)
                .map(|i| {
                    let answers = a.answers.get(format!("q{i}")).map(|x| json!({ "answer": x })).unwrap_or_else(|| json!({}));
                    Decision { answered: answers.as_object().is_some_and(|o| !o.is_empty()), answers, model: model.clone(), ..Default::default() }
                })
                .collect();
            rec.fill(&a.models, a.rung, a.cost, a.tokens, &a.answers, summarize(&per), None);
            let id = ledger::record(db, &rec, Some(&state)).await;
            Ok(per.into_iter().map(|d| Decision { id, ..d }).collect())
        }
        Err(e) => {
            rec.source = "unavailable".into();
            rec.error = Some(e.clone());
            ledger::record(db, &rec, Some(&state)).await;
            Err(e)
        }
    }
}

/// Re-ask a recorded state on another model (for `zen s1 compare`). Not recorded as a decision.
pub async fn reask(name: &str, model_id: &str, state: &Value) -> Result<(Value, Option<String>, Duration, f64), String> {
    let l = spec_of(name).ok_or_else(|| format!("no decision `{name}`"))?;
    if l.spec.item.is_some() || catalog::get(name).is_some_and(|k| k.mode != catalog::Mode::Single) {
        return Err(format!("`{name}` judges items; compare works on single decisions"));
    }
    let facts = Facts { session: None, map: state.as_object().cloned().unwrap_or_default() };
    let questions: Value = Value::Object(l.spec.question.iter().map(|(k, q)| (k.clone(), wire_question(q, &q.text, &facts))).collect());
    let started = Instant::now();
    let deadline = started + l.spec.timeout().unwrap_or(Duration::from_secs(30));
    let r = call(model_id, state, &questions, deadline).await?;
    let outcome = l.policy.as_ref().and_then(|p| policy::apply(p, &r.answers, &policy::raw)).map(|j| j.outcome);
    Ok((r.answers, outcome, started.elapsed(), r.usage["cost"].as_f64().unwrap_or(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensembles_average() {
        let a = json!({ "b": { "type": "bool", "probability": 0.9 }, "c": { "type": "choice", "choice": "x", "probabilities": { "x": 0.6, "y": 0.4 } } });
        let b = json!({ "b": { "type": "bool", "probability": 0.5 }, "c": { "type": "choice", "choice": "y", "probabilities": { "x": 0.2, "y": 0.8 } } });
        let m = combine(&[a, b]);
        assert!((m["b"]["probability"].as_f64().unwrap() - 0.7).abs() < 1e-9);
        assert_eq!(m["c"]["choice"], "y");
    }

    #[test]
    fn states_are_masked() {
        std::env::set_var("S1_MASK_TEST_TOKEN", "sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789");
        let v = mask(&json!({ "a": ["key sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789 here"] }));
        assert!(!v.to_string().contains("abcdefghijklmnopqrstuvwxyz0123456789"), "{v}");
    }

    #[test]
    fn the_breaker_opens_after_failures_and_closes_on_success() {
        reset_breaker();
        for _ in 0..BREAKER_AFTER {
            breaker_result(false);
        }
        assert!(breaker_open());
        breaker_result(true);
        assert!(!breaker_open());
    }
}
