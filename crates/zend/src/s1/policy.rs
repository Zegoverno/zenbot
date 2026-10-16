//! How a decision's answers become its outcome. The model only answers; this decides.
//!
//! - **costs** (yes/no): with p the probability of yes, saying yes costs `(1-p)·false_yes` on
//!   average and saying no costs `p·false_no`; the cheaper wins, so the threshold is
//!   `false_yes / (false_yes + false_no)` (9 : 1 → yes from p ≥ 0.9).
//! - **levels** (choice over ordered options): each option's expected cost over the distribution,
//!   `under` per step for rating below the truth and `over` per step above it; the cheapest wins.
//!   Under-rating 10× worse than over-rating picks about the 91st percentile.
//! - **rules**: first matching condition (rules.rs), else `else`.
//!
//! `escalate` (costs, levels): when even the best outcome's expected cost is above it, the decision
//! is unsure and moves to the next rung of its ladder. Probabilities pass through the decision's
//! calibration first when it's on (calibrate.rs).

use std::collections::BTreeMap;

use serde_json::Value;

use super::rules::{self, Expr};
use super::spec::{Policy, Spec};

/// A policy with its conditions parsed, ready to apply.
#[derive(Debug, Clone)]
pub enum Compiled {
    Costs { question: String, false_yes: f64, false_no: f64, yes: String, no: String, escalate: Option<f64> },
    Levels { question: String, order: Vec<String>, under: f64, over: f64, outcomes: BTreeMap<String, String>, escalate: Option<f64> },
    Rules { rules: Vec<(Expr, String)>, otherwise: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    pub outcome: String,
    /// The best outcome's expected cost was above `escalate`.
    pub unsure: bool,
    /// The probability the outcome rests on (yes/no: of yes; levels: of the chosen option).
    pub p: Option<f64>,
    pub expected_cost: Option<f64>,
}

pub fn compile(spec: &Spec) -> Result<Option<Compiled>, String> {
    let is_question = |n: &str| spec.question.contains_key(n);
    Ok(match &spec.policy {
        None => None,
        Some(Policy::Costs { question, false_yes, false_no, yes, no, escalate }) => Some(Compiled::Costs {
            question: question.clone(), false_yes: *false_yes, false_no: *false_no, yes: yes.clone(), no: no.clone(), escalate: *escalate,
        }),
        Some(Policy::Levels { question, order, under, over, outcomes, escalate }) => Some(Compiled::Levels {
            question: question.clone(), order: order.clone(), under: *under, over: *over, outcomes: outcomes.clone(), escalate: *escalate,
        }),
        Some(Policy::Rules { rules: rs, otherwise }) => {
            let mut rules = Vec::new();
            for (i, r) in rs.iter().enumerate() {
                let e = rules::parse(&r.when, &is_question).map_err(|e| format!("rule {}: {e}", i + 1))?;
                rules.push((e, r.decide.clone()));
            }
            Some(Compiled::Rules { rules, otherwise: otherwise.clone() })
        }
    })
}

/// Apply a compiled policy to parsed answers. None when the answers it needs are missing.
pub fn apply(policy: &Compiled, answers: &Value, calibrate: &dyn Fn(&str, f64) -> f64) -> Option<Judged> {
    match policy {
        Compiled::Costs { question, false_yes, false_no, yes, no, escalate } => {
            let p = calibrate(question, answers[question]["probability"].as_f64()?).clamp(0.0, 1.0);
            let (cost_yes, cost_no) = ((1.0 - p) * false_yes, p * false_no);
            let (outcome, cost) = if cost_yes <= cost_no { (yes, cost_yes) } else { (no, cost_no) };
            Some(Judged { outcome: outcome.clone(), unsure: escalate.is_some_and(|e| cost > e), p: Some(p), expected_cost: Some(cost) })
        }
        Compiled::Levels { question, order, under, over, outcomes, escalate } => {
            let a = &answers[question];
            let chosen = a["choice"].as_str()?;
            let mut dist: Vec<f64> = order.iter().map(|o| a["probabilities"][o].as_f64().map(|p| calibrate(question, p)).unwrap_or(0.0)).collect();
            if dist.iter().sum::<f64>() <= 0.0 {
                // Only the choice came back: all the weight on it.
                let i = order.iter().position(|o| o == chosen)?;
                dist[i] = 1.0;
            }
            let total: f64 = dist.iter().sum();
            dist.iter_mut().for_each(|p| *p /= total);
            let cost = |i: usize| dist.iter().enumerate().map(|(j, p)| p * if j > i { under * (j - i) as f64 } else { over * (i - j) as f64 }).sum::<f64>();
            // Ties go to the higher (more cautious) level.
            let best = (0..order.len()).rev().min_by(|&x, &y| cost(x).partial_cmp(&cost(y)).unwrap_or(std::cmp::Ordering::Equal))?;
            let option = &order[best];
            let c = cost(best);
            Some(Judged {
                outcome: outcomes.get(option).cloned().unwrap_or_else(|| option.clone()),
                unsure: escalate.is_some_and(|e| c > e),
                p: Some(dist[best]),
                expected_cost: Some(c),
            })
        }
        Compiled::Rules { rules: rs, otherwise } => {
            let outcome = rs.iter().find(|(e, _)| rules::eval(e, answers)).map(|(_, d)| d.clone()).unwrap_or_else(|| otherwise.clone());
            Some(Judged { outcome, unsure: false, p: None, expected_cost: None })
        }
    }
}

/// Identity: no calibration.
pub fn raw(_: &str, p: f64) -> f64 {
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn costs(fy: f64, fno: f64, escalate: Option<f64>) -> Compiled {
        Compiled::Costs { question: "q".into(), false_yes: fy, false_no: fno, yes: "yes".into(), no: "no".into(), escalate }
    }
    fn yn(p: f64) -> Value {
        json!({ "q": { "type": "bool", "probability": p } })
    }

    #[test]
    fn costs_set_the_threshold() {
        let p = costs(9.0, 1.0, None);
        assert_eq!(apply(&p, &yn(0.9), &raw).unwrap().outcome, "yes", "9:1 is a 0.9 threshold, inclusive like `p >= bar`");
        assert_eq!(apply(&p, &yn(0.89), &raw).unwrap().outcome, "no");
        let p = costs(3.0, 1.0, None);
        assert_eq!(apply(&p, &yn(0.76), &raw).unwrap().outcome, "yes");
        assert_eq!(apply(&p, &yn(0.74), &raw).unwrap().outcome, "no");
        assert!(apply(&p, &json!({}), &raw).is_none(), "no answer, no outcome");
    }

    #[test]
    fn unsure_when_even_the_best_choice_is_too_costly() {
        let p = costs(1.0, 1.0, Some(0.2));
        assert!(!apply(&p, &yn(0.95), &raw).unwrap().unsure);
        assert!(apply(&p, &yn(0.6), &raw).unwrap().unsure);
    }

    #[test]
    fn calibration_is_applied_before_costs() {
        let p = costs(9.0, 1.0, None);
        let shrink = |_: &str, p: f64| 0.5 + (p - 0.5) * 0.5;
        assert_eq!(apply(&p, &yn(0.95), &shrink).unwrap().outcome, "no", "0.95 shrunk to 0.725 is under 0.9");
    }

    #[test]
    fn levels_are_cautious_by_the_stated_amount() {
        let p = Compiled::Levels {
            question: "risk".into(), order: vec!["2".into(), "3".into(), "4".into()], under: 10.0, over: 1.0,
            outcomes: BTreeMap::new(), escalate: None,
        };
        let a = |p2: f64, p3: f64, p4: f64| json!({ "risk": { "type": "choice", "choice": "2", "probabilities": { "2": p2, "3": p3, "4": p4 } } });
        assert_eq!(apply(&p, &a(0.95, 0.04, 0.01), &raw).unwrap().outcome, "2");
        assert_eq!(apply(&p, &a(0.80, 0.15, 0.05), &raw).unwrap().outcome, "3", "under-rating 10× worse: the 91st percentile, not the most likely");
        let symmetric = Compiled::Levels { question: "risk".into(), order: vec!["2".into(), "3".into(), "4".into()], under: 1.0, over: 1.0, outcomes: BTreeMap::new(), escalate: None };
        assert_eq!(apply(&symmetric, &a(0.80, 0.15, 0.05), &raw).unwrap().outcome, "2", "symmetric costs pick the median");
        let only_choice = json!({ "risk": { "type": "choice", "choice": "3" } });
        assert_eq!(apply(&p, &only_choice, &raw).unwrap().outcome, "3");
    }

    #[test]
    fn rules_first_match_wins() {
        let spec = Spec::parse(r#"
name = "t"
version = 1
exposure = "outside"
[question.injection]
type = "yes/no"
text = "?"
[policy]
kind = "rules"
else = "keep"
[[policy.rule]]
when = "injection >= 0.9"
decide = "stop"
[[policy.rule]]
when = "injection >= 0.5"
decide = "raise"
"#).unwrap();
        let p = compile(&spec).unwrap().unwrap();
        let out = |x: f64| apply(&p, &json!({ "injection": { "type": "bool", "probability": x } }), &raw).unwrap().outcome;
        assert_eq!(out(0.95), "stop");
        assert_eq!(out(0.6), "raise");
        assert_eq!(out(0.1), "keep");
        assert_eq!(apply(&p, &json!({}), &raw).unwrap().outcome, "keep", "rules on missing answers fall to else");
    }
}
