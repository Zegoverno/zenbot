//! TeX math to plain Unicode, for reading formulas in a terminal: `\frac{a}{b}` as `a/b`,
//! `x^{2}` as `x²`, Greek letters and operators as their symbols, `\text{…}` as its text. Not a
//! typesetter: what has no Unicode form keeps a readable spelling (`x^(n+1)`), and an unknown
//! command keeps its name.

/// `src` (the TeX between `$` signs) as Unicode text.
pub fn to_unicode(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let out = expr(&chars, &mut i, false);
    // Spaces TeX ignores: one at most, none at the ends.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Read until the end, or a `}` closing the group when `group`.
fn expr(c: &[char], i: &mut usize, group: bool) -> String {
    let mut out = String::new();
    while *i < c.len() {
        let ch = c[*i];
        *i += 1;
        match ch {
            '}' if group => return out,
            '{' => out.push_str(&expr(c, i, true)),
            '\\' => out.push_str(&command(c, i)),
            '^' | '_' => {
                let arg = arg(c, i);
                out.push_str(&script(&arg, ch == '^'));
            }
            '&' => out.push(' '),
            '~' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// One argument: a `{group}`, a `\command`, or one character.
fn arg(c: &[char], i: &mut usize) -> String {
    while *i < c.len() && c[*i] == ' ' {
        *i += 1;
    }
    match c.get(*i) {
        Some('{') => {
            *i += 1;
            expr(c, i, true)
        }
        Some('\\') => {
            *i += 1;
            command(c, i)
        }
        Some(&ch) => {
            *i += 1;
            ch.to_string()
        }
        None => String::new(),
    }
}

/// An optional `[…]` argument (as `\sqrt[3]{x}` has).
fn optional(c: &[char], i: &mut usize) -> Option<String> {
    if c.get(*i) != Some(&'[') {
        return None;
    }
    let end = c[*i..].iter().position(|ch| *ch == ']')? + *i;
    let s: String = c[*i + 1..end].iter().collect();
    *i = end + 1;
    Some(s)
}

/// Wrapped in parentheses when it's more than one term, so `a+b/c` doesn't misread.
fn term(s: &str) -> String {
    let s = s.trim();
    // An operator outside any parentheses makes it more than one term.
    let mut depth = 0i32;
    let top_op = s.chars().any(|ch| {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        depth == 0 && " +-−=/·×,<>≤≥".contains(ch)
    });
    if s.chars().count() > 1 && top_op {
        format!("({s})")
    } else {
        s.to_string()
    }
}

/// A superscript (`up`) or subscript in Unicode, else `^x`, `^(…)`, `_x`, `_(…)`.
fn script(s: &str, up: bool) -> String {
    let s = s.trim();
    let map = |ch: char| -> Option<char> {
        let (from, to) = if up {
            ("0123456789+-=()niatkmxyTe*′", "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁿⁱᵃᵗᵏᵐˣʸᵀᵉ*′")
        } else {
            ("0123456789+-=()aeoxhklmnpstij", "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₒₓₕₖₗₘₙₚₛₜᵢⱼ")
        };
        if ch == '−' {
            return Some(if up { '⁻' } else { '₋' });
        }
        from.chars().position(|f| f == ch).and_then(|n| to.chars().nth(n))
    };
    if !s.is_empty() {
        if let Some(mapped) = s.chars().map(map).collect::<Option<String>>() {
            return mapped;
        }
    }
    let mark = if up { '^' } else { '_' };
    if s.chars().count() == 1 { format!("{mark}{s}") } else { format!("{mark}({s})") }
}

/// A combining mark over each letter (`\bar`, `\hat`, …).
fn accent(s: &str, mark: char) -> String {
    s.chars().flat_map(|ch| [ch, mark]).collect()
}

/// A blackboard-bold letter (`\mathbb`).
fn blackboard(s: &str) -> String {
    s.chars()
        .map(|ch| match ch {
            'R' => 'ℝ',
            'N' => 'ℕ',
            'Z' => 'ℤ',
            'Q' => 'ℚ',
            'C' => 'ℂ',
            'E' => '𝔼',
            'P' => 'ℙ',
            _ => ch,
        })
        .collect()
}

fn command(c: &[char], i: &mut usize) -> String {
    let start = *i;
    while *i < c.len() && c[*i].is_ascii_alphabetic() {
        *i += 1;
    }
    if *i == start {
        // A one-character command: `\,` `\;` `\!` `\{` `\%` `\\` …
        let Some(&ch) = c.get(*i) else { return String::new() };
        *i += 1;
        return match ch {
            ',' | ';' | ':' | ' ' => " ".into(),
            '!' => String::new(),
            '\\' => "; ".into(),
            _ => ch.to_string(),
        };
    }
    let name: String = c[start..*i].iter().collect();
    let sym = |s: &str| s.to_string();
    match name.as_str() {
        "frac" | "dfrac" | "tfrac" | "cfrac" => {
            let (a, b) = (arg(c, i), arg(c, i));
            format!("{}/{}", term(&a), term(&b))
        }
        "sqrt" => {
            let n = optional(c, i);
            let x = arg(c, i);
            let root = match n.as_deref() {
                Some("3") => "∛",
                Some("4") => "∜",
                _ => "√",
            };
            format!("{root}{}", term(&x))
        }
        "text" | "textrm" | "textit" | "textbf" | "mathrm" | "mathit" | "mathbf" | "mathsf" | "mathcal" | "operatorname" | "boldsymbol" | "mbox" => arg(c, i),
        "mathbb" => blackboard(&arg(c, i)),
        "bar" | "overline" => accent(&arg(c, i), '\u{0304}'),
        "hat" | "widehat" => accent(&arg(c, i), '\u{0302}'),
        "tilde" | "widetilde" => accent(&arg(c, i), '\u{0303}'),
        "vec" => accent(&arg(c, i), '\u{20D7}'),
        "dot" => accent(&arg(c, i), '\u{0307}'),
        "underbrace" | "overbrace" | "underline" => arg(c, i),
        "begin" | "end" => {
            arg(c, i);
            String::new()
        }
        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr" | "displaystyle" | "limits" | "nolimits" => String::new(),
        "quad" => "  ".into(),
        "qquad" => "    ".into(),
        _ => match symbol(&name) {
            Some(s) => sym(s),
            None => name,
        },
    }
}

fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" | "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" | "vartheta" => "θ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "rho" | "varrho" => "ρ",
        "sigma" => "σ",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" | "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        "times" => "×",
        "cdot" => "·",
        "div" => "÷",
        "pm" => "±",
        "mp" => "∓",
        "le" | "leq" => "≤",
        "ge" | "geq" => "≥",
        "ne" | "neq" => "≠",
        "approx" => "≈",
        "sim" => "∼",
        "simeq" => "≃",
        "equiv" => "≡",
        "propto" => "∝",
        "infty" => "∞",
        "sum" => "∑",
        "prod" => "∏",
        "int" => "∫",
        "oint" => "∮",
        "partial" => "∂",
        "nabla" => "∇",
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "Rightarrow" | "implies" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" | "iff" => "⇔",
        "leftrightarrow" => "↔",
        "mapsto" => "↦",
        "uparrow" => "↑",
        "downarrow" => "↓",
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "supset" => "⊃",
        "cup" => "∪",
        "cap" => "∩",
        "setminus" => "∖",
        "emptyset" | "varnothing" => "∅",
        "forall" => "∀",
        "exists" => "∃",
        "neg" | "lnot" => "¬",
        "land" | "wedge" => "∧",
        "lor" | "vee" => "∨",
        "ldots" | "dots" | "cdots" | "dotsc" | "dotsb" => "…",
        "vdots" => "⋮",
        "mid" => "|",
        "ll" => "≪",
        "gg" => "≫",
        "prime" => "′",
        "circ" => "∘",
        "degree" => "°",
        "star" => "⋆",
        "ast" => "∗",
        "perp" => "⊥",
        "parallel" => "∥",
        "angle" => "∠",
        "triangle" => "△",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "lceil" => "⌈",
        "rceil" => "⌉",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lbrace" => "{",
        "rbrace" => "}",
        "hbar" => "ℏ",
        "ell" => "ℓ",
        "log" | "ln" | "exp" | "max" | "min" | "lim" | "sup" | "inf" | "sin" | "cos" | "tan" | "det" | "Pr" | "arg" | "argmax" | "argmin" | "Var" | "Cov" | "E" | "deg" | "dim" | "ker" | "gcd" | "sgn" | "tanh" | "cosh" | "sinh" => return word(name),
        _ => return None,
    })
}

/// Operator names (`\log`, `\max`): the word, set off by a space after it.
fn word(name: &str) -> Option<&'static str> {
    Some(match name {
        "log" => "log ",
        "ln" => "ln ",
        "exp" => "exp ",
        "max" => "max ",
        "min" => "min ",
        "lim" => "lim ",
        "sup" => "sup ",
        "inf" => "inf ",
        "sin" => "sin ",
        "cos" => "cos ",
        "tan" => "tan ",
        "det" => "det ",
        "Pr" => "Pr ",
        "arg" => "arg ",
        "argmax" => "argmax ",
        "argmin" => "argmin ",
        "Var" => "Var",
        "Cov" => "Cov",
        "E" => "E",
        "deg" => "deg ",
        "dim" => "dim ",
        "ker" => "ker ",
        "gcd" => "gcd ",
        "sgn" => "sgn ",
        "tanh" => "tanh ",
        "cosh" => "cosh ",
        "sinh" => "sinh ",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::to_unicode as u;

    #[test]
    fn formulas_from_the_vault_read_as_unicode() {
        assert_eq!(u(r"=\dfrac{n(n-1)}{2} \propto n^2"), "=n(n-1)/2 ∝ n²");
        assert_eq!(u(r"NPV(\tau)=\sum_t\frac{CF_t(\tau)}{(1+k)^t}."), "NPV(τ)=∑ₜCFₜ(τ)/(1+k)ᵗ.");
        assert_eq!(u(r"1.12/1.08-1\approx3.70\%"), "1.12/1.08-1≈3.70%");
        assert_eq!(u(r"PU=\frac{100000}{1+c\,DC/360}"), "PU=100000/(1+c DC/360)");
        assert_eq!(u(r"\alpha_t = \alpha_0 e^{-\lambda t}"), "αₜ = α₀ e^(-λ t)");
        assert_eq!(u(r"IC = \frac{Current Assets}{Current Liabilities}"), "IC = (Current Assets)/(Current Liabilities)");
        assert_eq!(u(r"\text{Assets} \le \sqrt{x+1} \mathbb{E}[R] \bar{x}"), "Assets ≤ √(x+1) 𝔼[R] x̄");
        assert_eq!(u(r"\left( a \right) \unknown x_{ij} y^{n+1}"), "( a ) unknown xᵢⱼ yⁿ⁺¹");
        assert_eq!(u(r"\log x + \frac{a}"), "log x + a/");
    }
}
