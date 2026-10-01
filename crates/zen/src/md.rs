//! Styled terminal lines, word wrapping, and a small line-oriented markdown renderer.
//! Line-oriented so streamed text can be committed to scrollback as each line completes.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sty {
    Plain,
    Dim,
    Bold,
    Code,
    Accent,
    Err,
    Warn,
}

impl Sty {
    fn ansi(self) -> &'static str {
        match self {
            Sty::Plain => "",
            Sty::Dim => "\x1b[2m",
            Sty::Bold => "\x1b[1m",
            Sty::Code => "\x1b[36m",
            Sty::Accent => "\x1b[32m",
            Sty::Err => "\x1b[31m",
            Sty::Warn => "\x1b[33m",
        }
    }
}

pub type Seg = (String, Sty);
pub type Line = Vec<Seg>;

pub fn line(text: impl Into<String>, sty: Sty) -> Line {
    vec![(text.into(), sty)]
}


pub fn to_ansi(l: &Line) -> String {
    let mut out = String::new();
    for (t, s) in l {
        if *s == Sty::Plain {
            out.push_str(t);
        } else {
            out.push_str(s.ansi());
            out.push_str(t);
            out.push_str("\x1b[0m");
        }
    }
    out
}

fn push(line: &mut Line, text: &str, sty: Sty) {
    if let Some(last) = line.last_mut() {
        if last.1 == sty {
            last.0.push_str(text);
            return;
        }
    }
    line.push((text.to_string(), sty));
}

fn trim_end(line: &mut Line) {
    loop {
        let len = line.len();
        let Some(last) = line.last_mut() else { break };
        let trimmed = last.0.trim_end_matches(' ').len();
        if trimmed == 0 && len > 1 {
            line.pop();
        } else {
            last.0.truncate(trimmed);
            break;
        }
    }
}

/// Split into alternating runs of spaces and non-spaces.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_space = None;
    for (i, c) in s.char_indices() {
        let sp = c == ' ';
        match in_space {
            Some(prev) if prev != sp => {
                out.push(&s[start..i]);
                start = i;
            }
            _ => {}
        }
        in_space = Some(sp);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Word-wrap styled segments to `width` columns with a first-line prefix and a continuation prefix.
pub fn wrap(segs: Vec<Seg>, width: usize, prefix: Seg, cont: Seg) -> Vec<Line> {
    let width = width.max(12);
    let cont_w = UnicodeWidthStr::width(cont.0.as_str());
    let mut lines = Vec::new();
    let mut cur: Line = vec![prefix.clone()];
    let mut start_w = UnicodeWidthStr::width(prefix.0.as_str());
    let mut w = start_w;
    for (text, sty) in segs {
        for tok in tokens(&text) {
            let tw = UnicodeWidthStr::width(tok);
            if tok.starts_with(' ') {
                if w + tw <= width && w > start_w {
                    push(&mut cur, tok, sty);
                    w += tw;
                } else if w == start_w && lines.is_empty() && cur.len() == 1 {
                    // keep leading indentation on the first line (e.g. code)
                    push(&mut cur, tok, sty);
                    w += tw;
                }
                continue;
            }
            if w + tw > width && w > start_w {
                trim_end(&mut cur);
                lines.push(std::mem::replace(&mut cur, vec![cont.clone()]));
                start_w = cont_w;
                w = cont_w;
            }
            if w + tw > width {
                for ch in tok.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if w + cw > width && w > start_w {
                        lines.push(std::mem::replace(&mut cur, vec![cont.clone()]));
                        start_w = cont_w;
                        w = cont_w;
                    }
                    let mut b = [0u8; 4];
                    push(&mut cur, ch.encode_utf8(&mut b), sty);
                    w += cw;
                }
            } else {
                push(&mut cur, tok, sty);
                w += tw;
            }
        }
    }
    trim_end(&mut cur);
    lines.push(cur);
    lines
}

/// Inline markdown: `code` and **bold**.
fn inline(text: &str, base: Sty) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let flush = |cur: &mut String, segs: &mut Vec<Seg>| {
        if !cur.is_empty() {
            segs.push((std::mem::take(cur), base));
        }
    };
    while i < chars.len() {
        if chars[i] == '`' {
            if let Some(j) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                flush(&mut cur, &mut segs);
                segs.push((chars[i + 1..j].iter().collect(), Sty::Code));
                i = j + 1;
                continue;
            }
        }
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(j) = (i + 2..chars.len().saturating_sub(1)).find(|&j| chars[j] == '*' && chars[j + 1] == '*') {
                flush(&mut cur, &mut segs);
                segs.push((chars[i + 2..j].iter().collect(), if base == Sty::Dim { Sty::Dim } else { Sty::Bold }));
                i = j + 2;
                continue;
            }
        }
        cur.push(chars[i]);
        i += 1;
    }
    flush(&mut cur, &mut segs);
    segs
}

/// Stateful markdown renderer (tracks code fences across lines).
#[derive(Default)]
pub struct Md {
    in_code: bool,
}

impl Md {
    pub fn render(&mut self, text: &str, width: usize) -> Vec<Line> {
        text.split('\n').flat_map(|l| self.render_line(l, width)).collect()
    }

    pub fn render_line(&mut self, src: &str, width: usize) -> Vec<Line> {
        let t = src.trim_end();
        let none = || (String::new(), Sty::Plain);
        if t.trim_start().starts_with("```") {
            self.in_code = !self.in_code;
            return Vec::new();
        }
        if self.in_code {
            return wrap(vec![(t.to_string(), Sty::Code)], width, ("  ".into(), Sty::Plain), ("  ".into(), Sty::Plain));
        }
        if t.is_empty() {
            return vec![Vec::new()];
        }
        let body = t.trim_start();
        let lead = t.len() - body.len();
        if let Some(h) = body.strip_prefix('#') {
            let h = h.trim_start_matches('#');
            if h.starts_with(' ') {
                let segs = inline(h.trim(), Sty::Bold);
                return wrap(segs, width, none(), none());
            }
        }
        let marker = if body.starts_with("- ") || body.starts_with("* ") {
            Some(("•".to_string(), &body[2..]))
        } else {
            let digits = body.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && body[digits..].starts_with(". ") {
                Some((body[..digits + 1].to_string(), &body[digits + 2..]))
            } else {
                None
            }
        };
        if let Some((m, rest)) = marker {
            let prefix = format!("{}{} ", " ".repeat(lead), m);
            let cont = " ".repeat(UnicodeWidthStr::width(prefix.as_str()));
            return wrap(inline(rest, Sty::Plain), width, (prefix, Sty::Dim), (cont, Sty::Plain));
        }
        if let Some(q) = body.strip_prefix('>') {
            return wrap(inline(q.trim_start(), Sty::Dim), width, ("│ ".into(), Sty::Dim), ("│ ".into(), Sty::Dim));
        }
        wrap(inline(t, Sty::Plain), width, none(), none())
    }
}
