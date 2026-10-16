//! The decision file: what one System One decision asks, on what evidence, and how its answers
//! become an outcome. Plain TOML the owner can read and edit (`crates/zend/s1/*.toml` ship as
//! defaults; `~/.zenbot/s1/<name>.toml` replaces one). Every table refuses unknown fields, so a
//! typo is an error with its line, never a silently ignored setting (check.rs reports them).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub name: String,
    pub version: u32,
    #[serde(default)]
    pub description: String,
    /// What the evidence may contain, which decides where the outcome may lead (check.rs).
    pub exposure: Exposure,
    /// The facts the caller supplies, by name; they become the state's keys.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// Facts the rules may test directly (`context = low`), besides the questions' answers; a rule
    /// on inputs alone is decided without a model call.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Per-item decisions: the prefix of each item's state key (`result` gives `result_0`, …).
    /// One question, asked once per item in a single call.
    #[serde(default)]
    pub item: Option<String>,
    #[serde(default)]
    pub question: BTreeMap<String, Question>,
    #[serde(default)]
    pub policy: Option<Policy>,
    /// The outcome when no model could answer (off, failed, timed out, blocked by privacy).
    #[serde(default)]
    pub unavailable: Option<String>,
    /// Approved model names in order (models.toml); `default` is the list's default; `owner` as the
    /// last rung hands an unsure decision to the owner (outcome `ask_owner`).
    #[serde(default = "default_ladder")]
    pub ladder: Vec<String>,
    /// Ask this many models of the ladder at once and average them; disagreement counts as unsure.
    #[serde(default = "one")]
    pub ensemble: usize,
    /// Apply the learner's calibration curve to this decision's probabilities (opt-in).
    #[serde(default)]
    pub calibrate: bool,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    /// Largest state sent, in characters; above it the decision is unavailable.
    #[serde(default = "default_max_state")]
    pub max_state_chars: usize,
    #[serde(default, rename = "test")]
    pub tests: Vec<TestCase>,
}

fn default_ladder() -> Vec<String> {
    vec!["default".into()]
}
fn one() -> usize {
    1
}
fn default_timeout() -> String {
    "30s".into()
}
fn default_max_state() -> usize {
    120_000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    /// Only the owner's own words.
    Owner,
    /// Names, URLs, paths: no content.
    Metadata,
    /// File contents, memories, the agent's own text (needs ZEN_S1_PRIVATE).
    Private,
    /// Outside content (web pages, MCP output): outcomes may only lead toward caution.
    Outside,
    /// No outcome at all: answers are information for the caller (or the brain).
    Information,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    #[serde(rename = "type")]
    pub kind: QuestionKind,
    pub text: String,
    /// yes/no: what each answer means.
    #[serde(default)]
    pub yes: Option<String>,
    #[serde(default)]
    pub no: Option<String>,
    /// choice: option → what it means.
    #[serde(default)]
    pub options: Option<BTreeMap<String, String>>,
    /// choice: the options come from this fact (an object option → meaning), at call time.
    #[serde(default)]
    pub options_from: Option<String>,
    /// score: the levels, lowest first.
    #[serde(default)]
    pub levels: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum QuestionKind {
    #[serde(rename = "yes/no")]
    YesNo,
    #[serde(rename = "choice")]
    Choice,
    #[serde(rename = "score")]
    Score,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Policy {
    /// A yes/no question and what each mistake costs: the outcome with the lower expected cost.
    Costs {
        question: String,
        false_yes: f64,
        false_no: f64,
        yes: String,
        no: String,
        #[serde(default)]
        escalate: Option<f64>,
    },
    /// A choice over ordered options and what under- and over-rating cost per step: the option
    /// with the lowest expected cost (a cautious quantile when under-rating costs more).
    Levels {
        question: String,
        order: Vec<String>,
        under: f64,
        over: f64,
        #[serde(default)]
        outcomes: BTreeMap<String, String>,
        #[serde(default)]
        escalate: Option<f64>,
    },
    /// Explicit rules, first match wins (rules.rs has the condition language).
    Rules {
        #[serde(rename = "rule", default)]
        rules: Vec<Rule>,
        #[serde(rename = "else")]
        otherwise: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub when: String,
    pub decide: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    #[serde(default)]
    pub name: Option<String>,
    /// Answers as the model would give them: yes/no a probability, choice an option (or
    /// `{ choice, p }`), score a number (or `{ score, confidence }`). Missing = no answer.
    #[serde(default)]
    pub answers: BTreeMap<String, toml::Value>,
    /// The expected outcome; `"unavailable"` checks the no-answer path.
    pub expect: String,
}

impl Spec {
    pub fn parse(src: &str) -> Result<Spec, toml::de::Error> {
        toml::from_str(src)
    }

    pub fn timeout(&self) -> Option<Duration> {
        parse_duration(&self.timeout)
    }

    /// Every outcome the file can produce, for the checker.
    pub fn outcomes(&self) -> Vec<String> {
        let mut out: Vec<String> = match &self.policy {
            None => Vec::new(),
            Some(Policy::Costs { yes, no, .. }) => vec![yes.clone(), no.clone()],
            Some(Policy::Levels { order, outcomes, .. }) => order.iter().map(|o| outcomes.get(o).cloned().unwrap_or_else(|| o.clone())).collect(),
            Some(Policy::Rules { rules, otherwise }) => rules.iter().map(|r| r.decide.clone()).chain([otherwise.clone()]).collect(),
        };
        out.extend(self.unavailable.clone());
        if self.ladder.last().is_some_and(|l| l == "owner") {
            out.push("ask_owner".into());
        }
        out.sort();
        out.dedup();
        out
    }

    /// The models of the ladder, without the owner rung.
    pub fn models(&self) -> impl Iterator<Item = &str> {
        self.ladder.iter().map(String::as_str).filter(|m| *m != "owner")
    }
}

/// `10s`, `500ms`, `2m`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit() && c != '.')?);
    let n: f64 = num.parse().ok()?;
    let secs = match unit {
        "ms" => n / 1000.0,
        "s" => n,
        "m" => n * 60.0,
        _ => return None,
    };
    (secs > 0.0 && secs <= 600.0).then(|| Duration::from_secs_f64(secs))
}

/// A test case's answers in the wire's parsed form (what `wire::parse_answers` returns).
pub fn test_answers(spec: &Spec, case: &TestCase) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    for (q, v) in &case.answers {
        if spec.inputs.contains(q) {
            let value = match v {
                toml::Value::String(s) => serde_json::json!(s),
                toml::Value::Boolean(b) => serde_json::json!(b),
                other => serde_json::json!(num(other)),
            };
            out.insert(q.clone(), serde_json::json!({ "type": "fact", "value": value }));
            continue;
        }
        let question = spec.question.get(q).ok_or_else(|| format!("test answers unknown question `{q}`"))?;
        let parsed = match (question.kind, v) {
            (QuestionKind::YesNo, v) => {
                let p = num(v).ok_or_else(|| format!("test answer for yes/no question `{q}` must be a probability"))?;
                serde_json::json!({ "type": "bool", "probability": p })
            }
            (QuestionKind::Choice, toml::Value::String(c)) => serde_json::json!({ "type": "choice", "choice": c, "probabilities": { c: 1.0 } }),
            (QuestionKind::Choice, toml::Value::Table(t)) => {
                let c = t.get("choice").and_then(|c| c.as_str()).ok_or_else(|| format!("test answer for `{q}` needs `choice`"))?;
                let p = t.get("p").and_then(num).unwrap_or(1.0);
                serde_json::json!({ "type": "choice", "choice": c, "probabilities": { c: p }, "confidence": t.get("confidence").and_then(num) })
            }
            (QuestionKind::Score, toml::Value::Table(t)) => {
                let s = t.get("score").and_then(num).ok_or_else(|| format!("test answer for `{q}` needs `score`"))?;
                serde_json::json!({ "type": "score", "score": s, "confidence": t.get("confidence").and_then(num) })
            }
            (QuestionKind::Score, v) => {
                let s = num(v).ok_or_else(|| format!("test answer for score question `{q}` must be a number"))?;
                serde_json::json!({ "type": "score", "score": s })
            }
            (QuestionKind::Choice, _) => return Err(format!("test answer for choice question `{q}` must be an option")),
        };
        out.insert(q.clone(), parsed);
    }
    Ok(Value::Object(out))
}

fn num(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: &str = r#"
name = "job_approval"
version = 1
exposure = "owner"
evidence = ["owner_messages", "job"]
unavailable = "paused"

[question.wanted]
type = "yes/no"
text = "Did the owner ask for this job?"
yes = "asked"
no = "not asked"

[policy]
kind = "costs"
question = "wanted"
false_yes = 9
false_no = 1
yes = "live"
no = "paused"

[[test]]
answers = { wanted = 0.95 }
expect = "live"
"#;

    #[test]
    fn a_decision_file_parses_with_defaults() {
        let s = Spec::parse(JOB).unwrap();
        assert_eq!(s.ladder, vec!["default"]);
        assert_eq!(s.timeout(), Some(Duration::from_secs(30)));
        assert_eq!(s.outcomes(), vec!["live", "paused"]);
        assert!(matches!(s.policy, Some(Policy::Costs { false_yes, .. }) if false_yes == 9.0));
        assert_eq!(test_answers(&s, &s.tests[0]).unwrap()["wanted"]["probability"], 0.95);
    }

    #[test]
    fn unknown_fields_are_errors_with_a_position() {
        let e = Spec::parse(&JOB.replace("false_no = 1", "false_noo = 1")).unwrap_err();
        assert!(e.message().contains("false_noo"), "{e}");
        assert!(e.span().is_some());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("10s"), Some(Duration::from_secs(10)));
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("10"), None);
        assert_eq!(parse_duration("1h"), None);
    }
}
