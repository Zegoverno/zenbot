//! The one call to OpenRouter's System One API (`POST /systemone`): a state and typed questions
//! in, typed answers with probabilities out. `bool` is `noul` on the wire, mapped back here.
//! Questions, state and criteria serialise in key order (serde_json without `preserve_order`);
//! the e2e stub relies on it, so keep it.

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    // The decision's own timeout (run.rs) is shorter; this only bounds a stuck connection.
    reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("System One HTTP client")
});

pub fn wire_questions(questions: &Value) -> Result<Value> {
    let entries = questions.as_object().context("System One questions must be an object")?;
    let mut wire = Map::new();
    for (id, question) in entries {
        let kind = question["type"].as_str().context("each System One question needs a type")?;
        if !matches!(kind, "choice" | "score" | "bool") {
            bail!("unsupported System One question type `{kind}`");
        }
        let mut q = question.as_object().context("each System One question must be an object")?.clone();
        if kind == "bool" {
            q.insert("type".into(), json!("noul"));
        }
        wire.insert(id.clone(), Value::Object(q));
    }
    Ok(Value::Object(wire))
}

pub fn parse_answers(body: &Value, questions: &Value) -> Result<Value> {
    let mut answers = Map::new();
    for (id, question) in questions.as_object().context("System One questions must be an object")? {
        let a = &body["answers"][id];
        let parsed = match question["type"].as_str().unwrap_or("") {
            "bool" => {
                let p = a["noul"].as_f64().with_context(|| format!("System One did not answer `{id}` as a bool"))?;
                if a["type"] != "noul" || !p.is_finite() || !(0.0..=1.0).contains(&p) {
                    bail!("invalid bool answer for `{id}`");
                }
                json!({ "type": "bool", "probability": p })
            }
            "choice" => {
                let choice = a["choice"].as_str().with_context(|| format!("System One did not answer `{id}` as a choice"))?;
                if a["type"] != "choice" || question["criteria"].get(choice).is_none() {
                    bail!("invalid choice answer for `{id}`");
                }
                json!({ "type": "choice", "choice": choice, "probabilities": a["probabilities"], "confidence": a["confidence"] })
            }
            "score" => {
                let score = a["score"].as_f64().with_context(|| format!("System One did not answer `{id}` as a score"))?;
                if a["type"] != "score" || !score.is_finite() {
                    bail!("invalid score answer for `{id}`");
                }
                json!({ "type": "score", "score": score, "confidence": a["confidence"] })
            }
            kind => bail!("unsupported System One question type `{kind}`"),
        };
        answers.insert(id.clone(), parsed);
    }
    Ok(Value::Object(answers))
}

/// What one call returned.
pub struct Reply {
    pub answers: Value,
    pub usage: Value,
}

/// Ask `model` (an OpenRouter id; an `openrouter/` prefix is dropped) the questions about `state`.
pub async fn call(model: &str, state: &Value, questions: &Value) -> Result<Reply> {
    let key = std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is not set")?;
    let model_id = model.strip_prefix("openrouter/").unwrap_or(model);
    let url = &crate::config::get().s1_url;
    let response = CLIENT
        .post(url)
        .bearer_auth(key)
        .json(&json!({ "model": model_id, "state": state, "questions": wire_questions(questions)? }))
        .send()
        .await
        .context("calling OpenRouter System One")?;
    let status = response.status();
    if !status.is_success() {
        bail!("OpenRouter System One returned HTTP {status}");
    }
    let body: Value = response.json().await.context("reading OpenRouter System One response")?;
    if let Some(e) = body["error"].as_str() {
        bail!("OpenRouter System One: {e}");
    }
    Ok(Reply { answers: parse_answers(&body, questions)?, usage: body["usage"].clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_is_noul_on_the_wire_and_back() {
        let q = json!({ "b": { "type": "bool", "instructions": "?", "criteria": { "true": "y", "false": "n" } },
                        "c": { "type": "choice", "instructions": "?", "criteria": { "x": "x", "y": "y" } } });
        let w = wire_questions(&q).unwrap();
        assert_eq!(w["b"]["type"], "noul");
        assert_eq!(w["c"]["type"], "choice");
        let body = json!({ "answers": { "b": { "type": "noul", "noul": 0.82 }, "c": { "type": "choice", "choice": "y", "probabilities": { "y": 0.7 } } } });
        let a = parse_answers(&body, &q).unwrap();
        assert_eq!(a["b"]["probability"], 0.82);
        assert_eq!(a["c"]["choice"], "y");
        assert!(parse_answers(&json!({ "answers": { "b": { "type": "noul", "noul": 1.5 } } }), &q).is_err());
    }

    #[test]
    fn keys_go_out_in_order() {
        let q = json!({ "zeta": { "type": "bool" }, "alpha": { "type": "bool" } });
        let keys: Vec<_> = wire_questions(&q).unwrap().as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["alpha", "zeta"], "the e2e stub picks the first criterion in key order");
    }
}
