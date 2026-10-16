//! The checker: a decision file is used only if it passes. Every problem is reported with its line
//! and column and, where it can, how to fix it. Runs when the kernel loads or reloads a file,
//! in the tests for every default file, and on demand (`zen s1 check`).

use serde::Serialize;

use super::catalog::{self, Effect, Mode};
use super::models::Models;
use super::policy;
use super::spec::{self, Policy, QuestionKind, Spec};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Diag {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

/// `line:col` (1-based) of a byte offset.
fn line_col(src: &str, at: usize) -> (usize, usize) {
    let before = &src[..at.min(src.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
    (line, col)
}

/// A parse error as `line:col: message`, with a suggestion for an unknown field.
pub fn position(src: &str, e: &toml::de::Error) -> String {
    let d = parse_diag(src, e);
    format!("{}:{}: {}", d.line, d.col, d.message)
}

fn parse_diag(src: &str, e: &toml::de::Error) -> Diag {
    let mut at = e.span().map(|s| s.start).unwrap_or(0);
    let msg = e.message().trim().to_string();
    let unknown = unknown_field(&msg);
    if let Some((field, _)) = &unknown {
        // The error spans the table; point at the field itself.
        if let Some(i) = key_offset(src, at, field) {
            at = i;
        }
    }
    let (line, col) = line_col(src, at);
    let message = match unknown {
        Some((field, expected)) => match closest(&field, &expected) {
            Some(near) => format!("unknown field `{field}`; did you mean `{near}`?"),
            None => format!("unknown field `{field}`; expected one of {}", expected.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")),
        },
        None => msg,
    };
    Diag { line, col, message }
}

/// The offset of `field = …` at or after `from` (or `field` in an inline table).
fn key_offset(src: &str, from: usize, field: &str) -> Option<usize> {
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        if offset + line.len() > from {
            let t = line.trim_start();
            let indent = line.len() - t.len();
            if t.split('=').next().map(str::trim) == Some(field) && t.contains('=') {
                return Some(offset + indent);
            }
            for sep in ["{ ", ", ", "{"] {
                if let Some(i) = line.find(&format!("{sep}{field} =")).or_else(|| line.find(&format!("{sep}{field}="))) {
                    return Some(offset + i + sep.len());
                }
            }
        }
        offset += line.len();
    }
    None
}

/// serde's "unknown field `x`, expected one of `a`, `b`" (or "expected `a`").
fn unknown_field(msg: &str) -> Option<(String, Vec<String>)> {
    let rest = msg.strip_prefix("unknown field `")?;
    let (field, rest) = rest.split_once('`')?;
    let expected = rest.split('`').skip(1).step_by(2).map(String::from).collect();
    Some((field.to_string(), expected))
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The nearest candidate within a small edit distance.
pub fn closest(word: &str, candidates: &[impl AsRef<str>]) -> Option<String> {
    candidates
        .iter()
        .map(|c| (distance(word, c.as_ref()), c.as_ref()))
        .filter(|(d, c)| *d <= 2.max(c.len() / 4))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.to_string())
}

/// Where `key` is set inside `[table]` (or at the top when `table` is empty), for semantic errors.
fn locate(src: &str, table: &str, key: &str) -> (usize, usize) {
    let mut in_table = table.is_empty();
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with('[') {
            let name = t.trim_matches(|c| c == '[' || c == ']').trim();
            in_table = name == table;
            if in_table && key.is_empty() {
                return line_col(src, offset + line.find('[').unwrap_or(0));
            }
        } else if in_table && !key.is_empty() {
            let k = t.split('=').next().unwrap_or("").trim().trim_matches('"');
            if t.contains('=') && k == key {
                return line_col(src, offset + line.find(|c: char| !c.is_whitespace()).unwrap_or(0));
            }
        }
        offset += line.len();
    }
    // Inline tables (`question.x = { … }`) and the like: the first mention.
    src.find(table).filter(|_| !table.is_empty()).map(|i| line_col(src, i)).unwrap_or((1, 1))
}

/// Check one decision file. `name`: its file stem. Returns the spec when there are no errors.
pub fn check(name: &str, src: &str, models: &Models) -> (Option<Spec>, Vec<Diag>) {
    let spec = match Spec::parse(src) {
        Ok(s) => s,
        Err(e) => return (None, vec![parse_diag(src, &e)]),
    };
    let diags = std::cell::RefCell::new(Vec::new());
    let err = |table: &str, key: &str, message: String| {
        let (line, col) = locate(src, table, key);
        diags.borrow_mut().push(Diag { line, col, message });
    };

    if spec.name != name {
        err("", "name", format!("name `{}` doesn't match the file name `{name}.toml`", spec.name));
    }
    let Some(kind) = catalog::get(&spec.name) else {
        let names: Vec<&str> = catalog::KINDS.iter().map(|k| k.name).collect();
        let hint = closest(&spec.name, &names).map(|n| format!("; did you mean `{n}`?")).unwrap_or_default();
        err("", "name", format!("no caller asks for a decision named `{}`{hint}", spec.name));
        return (None, diags.into_inner());
    };
    if spec.version == 0 {
        err("", "version", "version starts at 1".into());
    }
    if spec.exposure != kind.exposure {
        err("", "exposure", format!("exposure is fixed by what the caller sends: `{}` must be {:?}", spec.name, kind.exposure).to_lowercase());
    }
    if spec.timeout().is_none() {
        err("", "timeout", format!("timeout `{}`: write it like `10s`, `500ms` or `2m` (at most 10m)", spec.timeout));
    }

    // Questions.
    if spec.question.is_empty() {
        err("", "", "a decision asks at least one question ([question.<name>])".into());
    }
    let facts = kind.facts;
    for (q, question) in &spec.question {
        let table = format!("question.{q}");
        match question.kind {
            QuestionKind::YesNo if question.options.is_some() || question.options_from.is_some() || question.levels.is_some() => {
                err(&table, "type", format!("yes/no question `{q}` takes `yes` and `no`, not options or levels"))
            }
            QuestionKind::Choice => match (&question.options, &question.options_from) {
                (Some(_), Some(_)) => err(&table, "options_from", format!("choice `{q}`: give `options` or `options_from`, not both")),
                (None, None) => err(&table, "type", format!("choice `{q}` needs `options` (or `options_from = \"<fact>\"`)")),
                (Some(o), None) if o.len() < 2 => err(&table, "options", format!("choice `{q}` needs at least two options")),
                (None, Some(f)) if !facts.contains(&f.as_str()) => {
                    err(&table, "options_from", format!("`{f}` isn't a fact the caller supplies ({})", facts.join(", ")))
                }
                _ => {}
            },
            QuestionKind::Score if question.levels.as_ref().is_none_or(|l| l.len() < 2) => err(&table, "levels", format!("score `{q}` needs at least two `levels`, lowest first")),
            _ => {}
        }
    }
    for e in &spec.evidence {
        if !facts.contains(&e.as_str()) {
            let hint = closest(e, facts).map(|n| format!("; did you mean `{n}`?")).unwrap_or_default();
            err("", "evidence", format!("`{e}` isn't a fact the caller supplies{hint}"));
        }
    }
    match (kind.mode, &spec.item) {
        (Mode::Items, None) => err("", "", format!("`{}` judges items: set `item = \"<state key prefix>\"`", spec.name)),
        (Mode::Items, Some(_)) if spec.question.len() != 1 => err("", "item", "an item decision asks exactly one question".into()),
        (Mode::Single | Mode::Batch, Some(_)) => err("", "item", format!("`{}` doesn't judge items: remove `item`", spec.name)),
        _ => {}
    }

    // Policy and outcomes.
    match (&spec.policy, kind.outcomes) {
        (Some(_), None) => err("policy", "", format!("`{}` is information only: its caller reads the answers, so remove [policy]", spec.name)),
        (None, Some(_)) => err("", "", format!("`{}` needs a [policy]: its caller acts on an outcome", spec.name)),
        _ => {}
    }
    if kind.outcomes.is_none() && spec.unavailable.is_some() {
        err("", "unavailable", format!("`{}` is information only: remove `unavailable`", spec.name));
    }
    let yes_no = |q: &str| spec.question.get(q).map(|x| x.kind == QuestionKind::YesNo);
    match &spec.policy {
        Some(Policy::Costs { question, false_yes, false_no, escalate, .. }) => {
            match yes_no(question) {
                None => err("policy", "question", format!("`{question}` isn't one of the questions")),
                Some(false) => err("policy", "question", format!("costs need a yes/no question; `{question}` isn't")),
                _ => {}
            }
            for (k, v) in [("false_yes", *false_yes), ("false_no", *false_no)] {
                if !(v.is_finite() && v > 0.0) {
                    err("policy", k, format!("`{k}` is what that mistake costs: a positive number"));
                }
            }
            if escalate.is_some_and(|e| !(e.is_finite() && e > 0.0)) {
                err("policy", "escalate", "`escalate` is a positive cost".into());
            }
        }
        Some(Policy::Levels { question, order, under, over, outcomes, escalate }) => {
            match spec.question.get(question) {
                None => err("policy", "question", format!("`{question}` isn't one of the questions")),
                Some(q) if q.kind != QuestionKind::Choice => err("policy", "question", format!("levels need a choice question; `{question}` isn't")),
                Some(q) => {
                    if let Some(opts) = &q.options {
                        for o in order.iter().filter(|o| !opts.contains_key(*o)) {
                            err("policy", "order", format!("`{o}` isn't an option of `{question}`"));
                        }
                    }
                }
            }
            if order.len() < 2 {
                err("policy", "order", "levels need at least two options in `order`, lowest first".into());
            }
            for k in outcomes.keys().filter(|k| !order.contains(k)) {
                err("policy", "outcomes", format!("outcome given for `{k}`, which isn't in `order`"));
            }
            for (k, v) in [("under", *under), ("over", *over)] {
                if !(v.is_finite() && v > 0.0) {
                    err("policy", k, format!("`{k}` is a cost per level: a positive number"));
                }
            }
            if escalate.is_some_and(|e| !(e.is_finite() && e > 0.0)) {
                err("policy", "escalate", "`escalate` is a positive cost".into());
            }
        }
        Some(Policy::Rules { rules, .. }) => {
            if let Err(e) = policy::compile(&spec) {
                err("policy", "", e);
            }
            let mut seen = std::collections::HashSet::new();
            for (i, r) in rules.iter().enumerate() {
                if !seen.insert(r.when.trim()) {
                    err("policy", "", format!("rule {} repeats an earlier condition and can never fire", i + 1));
                }
            }
        }
        None => {}
    }
    if let Some(allowed) = kind.outcomes {
        for o in spec.outcomes() {
            match kind.effect(&o) {
                None => {
                    let names: Vec<&str> = allowed.iter().map(|(n, _)| *n).collect();
                    err("policy", "", format!("outcome `{o}` isn't one `{}` understands ({})", spec.name, names.join(", ")))
                }
                Some(Effect::Grant) if matches!(spec.exposure, spec::Exposure::Outside | spec::Exposure::Information) => {
                    err("policy", "", format!("outcome `{o}` allows or lowers something, but the evidence is outside content: it may only lead toward caution"))
                }
                _ => {}
            }
        }
        if let (Some(must), u) = (kind.unavailable, spec.unavailable.as_deref()) {
            if u != Some(must) {
                err("", "unavailable", format!("`unavailable` must be `{must}`: without an answer `{}` fails toward caution", spec.name));
            }
        }
    }

    // Ladder.
    let rungs: Vec<&str> = spec.models().collect();
    if rungs.is_empty() {
        err("", "ladder", "the ladder needs at least one approved model".into());
    }
    // With System One off there's nothing to approve against: the ladder is checked once models exist.
    for r in rungs.iter().filter(|_| models.on()) {
        if !models.knows(r) {
            let names: Vec<&String> = models.list.keys().collect();
            let hint = closest(r, &names).map(|n| format!("; did you mean `{n}`?")).unwrap_or_default();
            err("", "ladder", format!("model `{r}` isn't on the approved list ({}){hint}", if names.is_empty() { "none approved".to_string() } else { names.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ") }));
        }
    }
    if let Some(i) = spec.ladder.iter().position(|r| r == "owner") {
        if i + 1 != spec.ladder.len() {
            err("", "ladder", "`owner` can only be the last rung".into());
        } else if !kind.owner_rung {
            err("", "ladder", format!("`{}`'s caller can't hand a decision to the owner: remove `owner`", spec.name));
        }
    }
    if spec.ensemble == 0 || spec.ensemble > rungs.len().max(1) {
        err("", "ensemble", format!("`ensemble = {}` needs that many models in the ladder ({} listed)", spec.ensemble, rungs.len()));
    }

    // The file's own test cases, through the policy (no model call).
    if diags.borrow().is_empty() {
        if let Ok(Some(p)) = policy::compile(&spec) {
            for (i, case) in spec.tests.iter().enumerate() {
                let label = case.name.clone().unwrap_or_else(|| format!("test {}", i + 1));
                match spec::test_answers(&spec, case) {
                    Err(e) => err("test", "", format!("{label}: {e}")),
                    Ok(answers) => {
                        // No answer the policy can use: the `unavailable` path, written either way.
                        let (got, ok) = match policy::apply(&p, &answers, &policy::raw) {
                            Some(j) => (j.outcome.clone(), j.outcome == case.expect),
                            None => ("unavailable".to_string(), case.expect == "unavailable" || spec.unavailable.as_deref() == Some(case.expect.as_str())),
                        };
                        if !ok {
                            err("test", "", format!("{label}: expected `{}`, the policy gives `{got}`", case.expect));
                        }
                    }
                }
            }
        } else if !spec.tests.is_empty() && spec.policy.is_none() {
            err("test", "", "test cases check outcomes; an information-only decision has none".into());
        }
    }
    let mut diags = diags.into_inner();
    diags.sort_by_key(|d| (d.line, d.col));
    if diags.is_empty() {
        (Some(spec), diags)
    } else {
        (None, diags)
    }
}

/// `path:line:col  error  message`, one per line.
pub fn render(path: &str, diags: &[Diag]) -> String {
    diags.iter().map(|d| format!("{path}:{}:{}  error  {}", d.line, d.col, d.message)).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn models() -> Models {
        Models { default: Some("jev".into()), list: BTreeMap::from([("jev".into(), super::super::models::Entry { id: "typesafe/jev-1.13".into(), note: String::new() })]), source: "test".into() }
    }

    const GOOD: &str = r#"name = "job_approval"
version = 1
exposure = "owner"
evidence = ["owner_messages", "job"]
unavailable = "paused"

[question.wanted]
type = "yes/no"
text = "Did the owner ask for this job?"

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

[[test]]
answers = { wanted = 0.82 }
expect = "paused"

[[test]]
expect = "unavailable"
"#;

    fn errors(src: &str) -> Vec<String> {
        check("job_approval", src, &models()).1.into_iter().map(|d| format!("{}:{} {}", d.line, d.col, d.message)).collect()
    }

    #[test]
    fn a_good_file_passes() {
        let (spec, diags) = check("job_approval", GOOD, &models());
        assert!(diags.is_empty(), "{diags:?}");
        assert!(spec.is_some());
    }

    #[test]
    fn typos_point_at_the_line_with_a_suggestion() {
        let e = errors(&GOOD.replace("false_no = 1", "false_noo = 1"));
        assert_eq!(e.len(), 1);
        assert!(e[0].starts_with("15:1 "), "{e:?}");
        assert!(e[0].contains("did you mean `false_no`?"), "{e:?}");
        let e = errors(&GOOD.replace("evidence = [\"owner_messages\", \"job\"]", "evidence = [\"owner_message\", \"job\"]"));
        assert!(e[0].contains("did you mean `owner_messages`?"), "{e:?}");
    }

    #[test]
    fn references_and_safety_are_checked() {
        assert!(errors(&GOOD.replace("ladder", "x").replace("version = 1", "version = 1\nladder = [\"big\"]"))[0].contains("isn't on the approved list"));
        assert!(errors(&GOOD.replace("exposure = \"owner\"", "exposure = \"metadata\""))[0].contains("exposure is fixed"));
        assert!(errors(&GOOD.replace("yes = \"live\"", "yes = \"approve\"")).iter().any(|e| e.contains("isn't one `job_approval` understands")));
        assert!(errors(&GOOD.replace("unavailable = \"paused\"", "unavailable = \"live\"")).iter().any(|e| e.contains("must be `paused`")));
        assert!(errors(&GOOD.replace("question = \"wanted\"", "question = \"want\"")).iter().any(|e| e.contains("isn't one of the questions")));
        assert!(errors(&GOOD.replace("false_yes = 9", "false_yes = -1")).iter().any(|e| e.contains("positive")));
        assert!(errors(&GOOD.replace("expect = \"live\"", "expect = \"paused\"")).iter().any(|e| e.contains("test 1: expected `paused`, the policy gives `live`")));
    }

    #[test]
    fn outside_evidence_cannot_grant() {
        let src = r#"name = "content_risk"
version = 1
exposure = "outside"
evidence = ["untrusted_text"]
unavailable = "raise"
[question.injection]
type = "yes/no"
text = "?"
[policy]
kind = "rules"
else = "keep"
[[policy.rule]]
when = "injection >= 0.5"
decide = "raise"
"#;
        assert!(check("content_risk", src, &models()).1.is_empty());
        let bad = check("content_risk", &src.replace("decide = \"raise\"", "decide = \"lower\""), &models()).1;
        assert!(bad.iter().any(|d| d.message.contains("isn't one `content_risk` understands")), "{bad:?}");
    }

    #[test]
    fn with_system_one_off_the_built_in_files_still_pass() {
        let (spec, diags) = check("job_approval", GOOD, &Models::default());
        assert!(diags.is_empty() && spec.is_some(), "{diags:?}");
    }

    #[test]
    fn suggestions() {
        assert_eq!(closest("false_noo", &["false_yes", "false_no"]), Some("false_no".into()));
        assert_eq!(closest("zzz", &["false_yes"]), None);
        assert_eq!(unknown_field("unknown field `x`, expected one of `a`, `b`"), Some(("x".into(), vec!["a".into(), "b".into()])));
    }
}
