//! S1, zenbot's System One (D-066): every fast decision goes through here. Like a person's System
//! One it's quick, cheap and always on; the brain (the session's reasoning model) deliberates and
//! uses S1's probabilities as evidence; reflexes (fixed kernel rules) sit below both.
//!
//! - **A decision is a file** (`crates/zend/s1/<name>.toml`, overridden by the owner's
//!   `~/.zenbot/s1/<name>.toml`): its questions, the evidence it's judged on, its policy (what each
//!   mistake costs, or explicit rules), its ladder of approved models and its test cases.
//!   `spec.rs` has the format, `check.rs` the checker; a file that fails the checker never takes
//!   effect (the last good version, or the default, stays).
//! - **The catalog** (`catalog.rs`) fixes in code what each caller sends and which outcomes it
//!   understands, so a file can tune a decision but never make one that reads outside content
//!   allow anything.
//! - **Callers** never call a model: `decide`, `decide_items`, `decide_batch` take facts and return
//!   the outcome with the answers; `judge` serves the brain's `judge` tool. The model only answers;
//!   `policy.rs` decides.
//! - **Every decision is recorded** (`ledger.rs`): model, answers, outcome, rung, latency, cost;
//!   outcomes learned later (`learn.rs`) calibrate probabilities and back the owner's proposals.
//! - **Resilience** (`run.rs`): per-decision timeout, a cap on calls in flight, a circuit breaker,
//!   secret masking of every state, the privacy setting (`ZEN_S1_PRIVATE=0` blocks private and
//!   outside evidence), approved models only (`models.rs`).

pub mod api;
pub mod catalog;
pub mod check;
pub mod learn;
pub mod ledger;
pub mod models;
pub mod policy;
pub mod rules;
pub mod run;
pub mod spec;
pub mod wire;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};
use std::time::{Instant, SystemTime};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;


use check::Diag;
use models::Models;
use policy::Compiled;
use spec::Spec;

/// The default decision files, built into the binary.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("job_approval", include_str!("../../s1/job_approval.toml")),
    ("owner_url", include_str!("../../s1/owner_url.toml")),
    ("source_risk", include_str!("../../s1/source_risk.toml")),
    ("content_risk", include_str!("../../s1/content_risk.toml")),
    ("action_gate", include_str!("../../s1/action_gate.toml")),
    ("web_focus", include_str!("../../s1/web_focus.toml")),
    ("web_rerank", include_str!("../../s1/web_rerank.toml")),
    ("search_rerank", include_str!("../../s1/search_rerank.toml")),
    ("wiki_capture", include_str!("../../s1/wiki_capture.toml")),
    ("skill_duplicate", include_str!("../../s1/skill_duplicate.toml")),
    ("sleep_judge", include_str!("../../s1/sleep_judge.toml")),
    ("sleep_recheck", include_str!("../../s1/sleep_recheck.toml")),
    ("judge", include_str!("../../s1/judge.toml")),
];

/// The owner's decision files and approved models: `~/.zenbot/s1/`.
pub fn dir() -> PathBuf {
    crate::zen_home().join("s1")
}

/// Whether the kernel holds an OpenRouter key. The kernel makes every OpenRouter call itself
/// (System One, embeddings) and workers never get the key.
pub fn openrouter_key() -> bool {
    std::env::var("OPENROUTER_API_KEY").is_ok_and(|k| !k.trim().is_empty())
}

/// Whether System One may see private content (file contents, tool output, memories, search
/// results), not only the owner's messages and metadata: on by default (D-032). ZEN_S1_PRIVATE=0
/// blocks `private`, `outside` and `information` decisions; they take their `unavailable` outcome.
pub fn private_ok() -> bool {
    crate::config::get().s1_private
}

/// Whether System One is configured at all (an approved default model).
pub fn on() -> bool {
    registry().models.on()
}

/// The default model's OpenRouter id, for the record (`sleep_runs.scorer`, status).
pub fn default_model() -> Option<String> {
    let r = registry();
    r.models.id("default").map(String::from)
}

/// A decision file in force.
#[derive(Clone)]
pub struct Loaded {
    pub spec: Spec,
    pub policy: Option<Compiled>,
    pub sha: String,
    /// `built-in` or the owner's file path.
    pub source: String,
}

#[derive(Clone, serde::Serialize)]
pub struct FileDiag {
    pub path: String,
    pub diags: Vec<Diag>,
}

pub struct Registry {
    pub specs: HashMap<String, Loaded>,
    pub models: Models,
    pub problems: Vec<FileDiag>,
    scanned: Instant,
    stamps: Vec<(PathBuf, Option<SystemTime>)>,
}

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(|| RwLock::new(load(None)));

fn sha(src: &str) -> String {
    format!("{:x}", Sha256::digest(src.as_bytes()))[..12].to_string()
}

/// What's in the owner's directory and when each file changed, to notice edits.
fn stamps() -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut out: Vec<(PathBuf, Option<SystemTime>)> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| {
            let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
            (p, t)
        })
        .collect();
    out.sort();
    out
}

/// Load every decision: the owner's file when it passes the checker, else the last good version
/// (`previous`), else the built-in default. Problems are kept for `zen s1 check` and status.
fn load(previous: Option<&Registry>) -> Registry {
    let mut problems = Vec::new();
    let models = match std::fs::read_to_string(models::path()) {
        Ok(src) => match Models::parse(&src) {
            Ok(m) => m,
            Err(e) => {
                problems.push(FileDiag { path: models::path().display().to_string(), diags: vec![diag_from(&e)] });
                previous.map(|p| p.models.clone()).unwrap_or_else(Models::from_env)
            }
        },
        Err(_) => Models::from_env(),
    };
    let mut specs = HashMap::new();
    for (name, src) in DEFAULTS {
        let (spec, diags) = check::check(name, src, &models);
        let builtin = match spec {
            Some(s) => Some(loaded(s, src, "built-in")),
            None => {
                // A default that fails the checker is a bug (the tests check every default); with
                // the owner's models it can also mean a ladder name their list doesn't have.
                problems.push(FileDiag { path: format!("built-in {name}.toml"), diags });
                None
            }
        };
        let path = dir().join(format!("{name}.toml"));
        let chosen = match std::fs::read_to_string(&path) {
            Ok(own) => {
                let (spec, diags) = check::check(name, &own, &models);
                match spec {
                    Some(s) => Some(loaded(s, &own, &path.display().to_string())),
                    None => {
                        problems.push(FileDiag { path: path.display().to_string(), diags });
                        previous.and_then(|p| p.specs.get(*name).cloned()).filter(|l| l.source != "built-in").or(builtin)
                    }
                }
            }
            Err(_) => builtin,
        };
        if let Some(l) = chosen {
            specs.insert(name.to_string(), deprecated_overrides(l));
        }
    }
    // Files in the owner's directory that no caller asks for.
    for (p, _) in stamps() {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem != "models" && !DEFAULTS.iter().any(|(n, _)| *n == stem) {
            let names: Vec<&str> = DEFAULTS.iter().map(|(n, _)| *n).collect();
            let hint = check::closest(stem, &names).map(|n| format!("; did you mean `{n}.toml`?")).unwrap_or_default();
            problems.push(FileDiag { path: p.display().to_string(), diags: vec![Diag { line: 1, col: 1, message: format!("no decision is named `{stem}`, so this file is unused{hint}") }] });
        }
    }
    for f in &problems {
        tracing::warn!("{}", check::render(&f.path, &f.diags));
    }
    Registry { specs, models, problems, scanned: Instant::now(), stamps: stamps() }
}

fn diag_from(e: &str) -> Diag {
    // `line:col: message` from Models::parse, or a plain message.
    let mut parts = e.splitn(3, ':');
    match (parts.next().and_then(|l| l.parse().ok()), parts.next().and_then(|c| c.trim().parse().ok()), parts.next()) {
        (Some(line), Some(col), Some(msg)) => Diag { line, col, message: msg.trim().to_string() },
        _ => Diag { line: 1, col: 1, message: e.to_string() },
    }
}

fn loaded(spec: Spec, src: &str, source: &str) -> Loaded {
    let policy = policy::compile(&spec).ok().flatten();
    Loaded { spec, policy, sha: sha(src), source: source.to_string() }
}

/// `ZEN_JOB_BAR`, set by hand, still applies to the built-in `job_approval` for one release: the
/// bar b becomes the costs b/(1-b) : 1. The owner's own file wins over it.
fn deprecated_overrides(mut l: Loaded) -> Loaded {
    if l.spec.name == "job_approval" && l.source == "built-in" && std::env::var("ZEN_JOB_BAR").is_ok() {
        let bar = crate::config::get().job_bar.clamp(0.01, 0.99);
        if let Some(Compiled::Costs { false_yes, false_no, .. }) = &mut l.policy {
            *false_yes = bar / (1.0 - bar);
            *false_no = 1.0;
            tracing::warn!("ZEN_JOB_BAR is deprecated: set the costs in ~/.zenbot/s1/job_approval.toml instead (zen s1 check)");
        }
    }
    l
}

/// The decisions in force, reloaded when the owner's files changed (checked at most every 2 s).
pub fn registry() -> std::sync::RwLockReadGuard<'static, Registry> {
    let stale = REGISTRY.read().map(|r| r.scanned.elapsed().as_secs_f64() > 2.0).unwrap_or(false);
    if stale {
        if let Ok(mut r) = REGISTRY.write() {
            if r.scanned.elapsed().as_secs_f64() > 2.0 {
                // models.toml is in the same directory, so its edits show here too.
                if stamps() != r.stamps {
                    let fresh = load(Some(&r));
                    *r = fresh;
                } else {
                    r.scanned = Instant::now();
                }
            }
        }
    }
    REGISTRY.read().unwrap_or_else(|e| e.into_inner())
}

/// Reload now (after the owner accepted a proposal that wrote their file).
pub fn reload() {
    if let Ok(mut r) = REGISTRY.write() {
        let fresh = load(Some(&r));
        *r = fresh;
    }
}

/// Facts for a decision: the state's keys, plus option lists for `options_from`.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub session: Option<Uuid>,
    pub map: Map<String, Value>,
}

impl Facts {
    pub fn new(session: Option<Uuid>) -> Facts {
        Facts { session, map: Map::new() }
    }
    pub fn with(mut self, name: &str, value: impl Into<Value>) -> Facts {
        self.map.insert(name.to_string(), value.into());
        self
    }
}

/// What a decision came to.
#[derive(Clone, Debug, Default)]
pub struct Decision {
    /// The policy's outcome, `ask_owner`, or the file's `unavailable` outcome; None for
    /// information-only decisions (read `answers`) and when there's neither.
    pub outcome: Option<String>,
    /// The parsed answers by question (`{type, probability|choice|score, …}`); empty without one.
    pub answers: Value,
    /// The probability the outcome rests on, when the policy has one.
    pub p: Option<f64>,
    /// Whether a model answered (false: unavailable, blocked or off).
    pub answered: bool,
    /// The model that answered, or why none did.
    pub model: Option<String>,
    pub error: Option<String>,
    /// The ledger row.
    pub id: Option<i64>,
}

impl Decision {
    pub fn is(&self, outcome: &str) -> bool {
        self.outcome.as_deref() == Some(outcome)
    }
    /// A yes/no answer's probability.
    pub fn prob(&self, question: &str) -> Option<f64> {
        self.answers[question]["probability"].as_f64()
    }
    pub fn choice(&self, question: &str) -> Option<&str> {
        self.answers[question]["choice"].as_str()
    }
}

/// One decision.
pub async fn decide(db: &sqlx::PgPool, name: &str, facts: Facts) -> Decision {
    run::single(db, name, facts).await
}

/// One question per item, in one call: each item's decision, in order.
pub async fn decide_items(db: &sqlx::PgPool, name: &str, facts: Facts, items: Vec<Value>) -> Vec<Decision> {
    run::items(db, name, facts, items).await
}

/// The same decision for several subjects, in one call.
pub async fn decide_batch(db: &sqlx::PgPool, name: &str, session: Option<Uuid>, subjects: Vec<Facts>) -> Vec<Decision> {
    run::batch(db, name, session, subjects).await
}

/// The probability a yes/no cost policy says yes from (`false_yes / (false_yes + false_no)`),
/// for messages that name the bar.
pub fn threshold(name: &str) -> Option<f64> {
    match registry().specs.get(name)?.policy.as_ref()? {
        Compiled::Costs { false_yes, false_no, .. } => Some(false_yes / (false_yes + false_no)),
        _ => None,
    }
}

/// Status for `zen status` and `GET /api/s1/check`: models, files in force, problems.
pub fn status() -> Value {
    let r = registry();
    let mut files: Vec<Value> = r.specs.values().map(|l| json!({ "name": l.spec.name, "description": l.spec.description, "version": l.spec.version, "sha": l.sha, "source": l.source })).collect();
    files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({
        "on": r.models.on(),
        "models": { "default": r.models.default, "list": r.models.list, "source": r.models.source },
        "files": files,
        "problems": r.problems,
        "errors": r.problems.iter().map(|p| p.diags.len()).sum::<usize>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_file_passes_the_checker() {
        let models = Models {
            default: Some("jev".into()),
            list: std::collections::BTreeMap::from([("jev".into(), models::Entry { id: "typesafe/jev-1.13".into(), note: String::new() })]),
            source: "test".into(),
        };
        for (name, src) in DEFAULTS {
            let (spec, diags) = check::check(name, src, &models);
            assert!(diags.is_empty(), "{}", check::render(&format!("{name}.toml"), &diags));
            assert!(spec.is_some());
        }
        for k in catalog::KINDS {
            assert!(DEFAULTS.iter().any(|(n, _)| *n == k.name), "{} has no default file", k.name);
        }
    }
}
