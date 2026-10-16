//! The condition language of `[[policy.rule]] when = "…"`: deliberately tiny, so a rule reads like
//! a sentence and can't grow into a program.
//!
//! ```text
//! injection >= 0.5
//! writers in [author, organisation] and reputation = reputable and reputation.p >= 0.8
//! not (known >= 0.8 or sensitive >= 0.8)
//! ```
//!
//! A question's name stands for its answer: a yes/no question's probability of yes, a choice's
//! chosen option, a score's score. `.p` is the probability (yes/no: of yes; choice: of the chosen
//! option, else its confidence), `.confidence` the model's confidence. A condition on a question
//! that wasn't answered is false. Comparisons: `= != < <= > >=`, `in [..]`, `not in [..]`;
//! combined with `and`, `or`, `not` and parentheses. Bare words are text.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Cmp(Operand, Op, Operand),
    In(Operand, Vec<Operand>, bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// A question's answer (`name`) or one of its fields (`name.p`, `name.confidence`).
    Answer(String, Option<String>),
    Num(f64),
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Num(f64),
    Str(String),
    Op(Op),
    LParen,
    RParen,
    LBrack,
    RBrack,
    Comma,
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            ' ' | '\t' | '\n' => i += 1,
            '(' => { out.push(Tok::LParen); i += 1 }
            ')' => { out.push(Tok::RParen); i += 1 }
            '[' => { out.push(Tok::LBrack); i += 1 }
            ']' => { out.push(Tok::RBrack); i += 1 }
            ',' => { out.push(Tok::Comma); i += 1 }
            '=' => { out.push(Tok::Op(Op::Eq)); i += if cs.get(i + 1) == Some(&'=') { 2 } else { 1 } }
            '!' if cs.get(i + 1) == Some(&'=') => { out.push(Tok::Op(Op::Ne)); i += 2 }
            '<' | '>' => {
                let eq = cs.get(i + 1) == Some(&'=');
                out.push(Tok::Op(match (c, eq) { ('<', true) => Op::Le, ('<', false) => Op::Lt, (_, true) => Op::Ge, _ => Op::Gt }));
                i += if eq { 2 } else { 1 };
            }
            '\'' | '"' => {
                let end = cs[i + 1..].iter().position(|&d| d == c).ok_or("unclosed quote")?;
                out.push(Tok::Str(cs[i + 1..i + 1 + end].iter().collect()));
                i += end + 2;
            }
            c if c.is_ascii_digit() || (c == '-' && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit())) => {
                let start = i;
                i += 1;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') { i += 1 }
                let t: String = cs[start..i].iter().collect();
                out.push(Tok::Num(t.parse().map_err(|_| format!("bad number `{t}`"))?));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let start = i;
                while i < cs.len() && (cs[i].is_alphanumeric() || matches!(cs[i], '_' | '.' | '-' | '/')) { i += 1 }
                out.push(Tok::Word(cs[start..i].iter().collect()));
            }
            c => return Err(format!("unexpected `{c}`")),
        }
    }
    Ok(out)
}

struct Parser<'a> {
    toks: Vec<Tok>,
    at: usize,
    questions: &'a dyn Fn(&str) -> bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }
    fn word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(x)) if x == w)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.at).cloned();
        self.at += 1;
        t
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut e = self.and()?;
        while self.word("or") {
            self.at += 1;
            e = Expr::Or(Box::new(e), Box::new(self.and()?));
        }
        Ok(e)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut e = self.not()?;
        while self.word("and") {
            self.at += 1;
            e = Expr::And(Box::new(e), Box::new(self.not()?));
        }
        Ok(e)
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.word("not") {
            self.at += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        if self.peek() == Some(&Tok::LParen) {
            self.at += 1;
            let e = self.or()?;
            if self.next() != Some(Tok::RParen) {
                return Err("missing `)`".into());
            }
            return Ok(e);
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let left = self.operand()?;
        if !matches!(left, Operand::Answer(..)) {
            return Err("a condition starts with a question's name".into());
        }
        if self.word("in") || (self.word("not") && matches!(self.toks.get(self.at + 1), Some(Tok::Word(w)) if w == "in")) {
            let negated = self.word("not");
            self.at += if negated { 2 } else { 1 };
            if self.next() != Some(Tok::LBrack) {
                return Err("`in` takes a list: [a, b]".into());
            }
            let mut items = Vec::new();
            loop {
                items.push(self.literal()?);
                match self.next() {
                    Some(Tok::Comma) => continue,
                    Some(Tok::RBrack) => break,
                    _ => return Err("missing `]`".into()),
                }
            }
            return Ok(Expr::In(left, items, negated));
        }
        let Some(Tok::Op(op)) = self.next() else { return Err("expected a comparison (= != < <= > >= in)".into()) };
        Ok(Expr::Cmp(left, op, self.literal()?))
    }
    fn operand(&mut self) -> Result<Operand, String> {
        match self.next() {
            Some(Tok::Word(w)) => {
                let (name, field) = match w.split_once('.') {
                    Some((n, f)) => (n.to_string(), Some(f.to_string())),
                    None => (w.clone(), None),
                };
                if (self.questions)(&name) {
                    if let Some(f) = &field {
                        if f != "p" && f != "confidence" {
                            return Err(format!("`{name}.{f}`: a question has `.p` and `.confidence`"));
                        }
                    }
                    Ok(Operand::Answer(name, field))
                } else {
                    Err(format!("`{name}` isn't a question of this decision"))
                }
            }
            _ => Err("expected a question's name".into()),
        }
    }
    fn literal(&mut self) -> Result<Operand, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Operand::Num(n)),
            Some(Tok::Str(s)) | Some(Tok::Word(s)) => Ok(Operand::Text(s)),
            _ => Err("expected a value".into()),
        }
    }
}

/// Parse a condition; `questions` says which names are questions of the decision.
pub fn parse(s: &str, questions: &dyn Fn(&str) -> bool) -> Result<Expr, String> {
    let mut p = Parser { toks: lex(s)?, at: 0, questions };
    let e = p.or()?;
    if p.at < p.toks.len() {
        return Err(format!("unexpected `{}`", s.split_whitespace().nth(p.at).unwrap_or("…")));
    }
    Ok(e)
}

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Num(f64),
    Text(String),
}

/// The value of an operand against parsed answers (`{q: {type, probability|choice|score, …}}`).
fn value(o: &Operand, answers: &Value) -> Option<Val> {
    match o {
        Operand::Num(n) => Some(Val::Num(*n)),
        Operand::Text(t) => Some(Val::Text(t.clone())),
        Operand::Answer(q, field) => {
            let a = &answers[q];
            match (a["type"].as_str()?, field.as_deref()) {
                ("bool", None | Some("p")) => a["probability"].as_f64().map(Val::Num),
                ("bool", Some(_)) => None,
                ("choice", None) => a["choice"].as_str().map(|c| Val::Text(c.into())),
                ("choice", Some("p")) => choice_p(a).map(Val::Num),
                ("score", None) => a["score"].as_f64().map(Val::Num),
                (_, Some("confidence")) => a["confidence"].as_f64().map(Val::Num),
                ("score", Some(_)) => a["confidence"].as_f64().map(Val::Num),
                _ => None,
            }
        }
    }
}

/// The probability of a choice's chosen option, else its confidence.
pub fn choice_p(a: &Value) -> Option<f64> {
    let c = a["choice"].as_str()?;
    a["probabilities"][c].as_f64().or_else(|| a["confidence"].as_f64())
}

fn compare(l: &Val, op: Op, r: &Val) -> bool {
    match (l, r) {
        (Val::Num(a), Val::Num(b)) => match op {
            Op::Eq => (a - b).abs() < 1e-12,
            Op::Ne => (a - b).abs() >= 1e-12,
            Op::Lt => a < b,
            Op::Le => a <= b,
            Op::Gt => a > b,
            Op::Ge => a >= b,
        },
        (Val::Text(a), Val::Text(b)) => match op {
            Op::Eq => a == b,
            Op::Ne => a != b,
            _ => false,
        },
        _ => false,
    }
}

pub fn eval(e: &Expr, answers: &Value) -> bool {
    match e {
        Expr::And(a, b) => eval(a, answers) && eval(b, answers),
        Expr::Or(a, b) => eval(a, answers) || eval(b, answers),
        Expr::Not(a) => !eval(a, answers),
        Expr::Cmp(l, op, r) => match (value(l, answers), value(r, answers)) {
            (Some(l), Some(r)) => compare(&l, *op, &r),
            _ => false,
        },
        Expr::In(l, items, negated) => match value(l, answers) {
            Some(l) => items.iter().filter_map(|i| value(i, answers)).any(|i| compare(&l, Op::Eq, &i)) != *negated,
            None => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn qs(n: &str) -> bool {
        ["injection", "writers", "reputation", "known", "sensitive", "outcome"].contains(&n)
    }

    #[test]
    fn conditions_read_like_sentences() {
        let a = json!({
            "injection": { "type": "bool", "probability": 0.7 },
            "writers": { "type": "choice", "choice": "organisation", "probabilities": { "organisation": 0.85 } },
            "reputation": { "type": "choice", "choice": "reputable", "confidence": 0.9 },
            "outcome": { "type": "score", "score": 2.0, "confidence": 0.6 }
        });
        let t = |s: &str| eval(&parse(s, &qs).unwrap(), &a);
        assert!(t("injection >= 0.5"));
        assert!(!t("injection < 0.5"));
        assert!(t("writers in [author, organisation] and reputation = reputable"));
        assert!(t("writers.p >= 0.8 and reputation.p >= 0.9"));
        assert!(t("writers not in [anyone, unknown]"));
        assert!(t("not (known >= 0.8 or sensitive >= 0.8)"), "unanswered questions make a condition false");
        assert!(t("outcome >= 2 and outcome.confidence > 0.5"));
        assert!(t("reputation = 'reputable'"));
        assert!(!t("writers = author or injection > 0.9"));
    }

    #[test]
    fn mistakes_say_what_is_wrong() {
        assert!(parse("injecton >= 0.5", &qs).unwrap_err().contains("isn't a question"));
        assert!(parse("injection >= ", &qs).unwrap_err().contains("expected a value"));
        assert!(parse("injection.prob >= 0.5", &qs).unwrap_err().contains(".p"));
        assert!(parse("(injection >= 0.5", &qs).unwrap_err().contains(")"));
        assert!(parse("writers in author", &qs).unwrap_err().contains("list"));
        assert!(parse("0.5 <= injection", &qs).is_err());
    }
}
