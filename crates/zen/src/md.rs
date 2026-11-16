//! Styled terminal lines, word wrapping, and a small line-oriented markdown renderer.
//! Line-oriented so streamed text can be committed to scrollback as each line completes.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sty {
    Plain,
    Dim,
    Bold,
    /// Emphasis (`*x*`, `_x_`), and math.
    Italic,
    /// A heading in the reader, by level (1-6): each level its own colour and weight.
    Heading(u8),
    /// A link's text in the reader: the link's index in `Md::links`.
    Link(u16),
    Code,
    Accent,
    Err,
    Warn,
    /// A grey of the reader's five (0-4: 100, 300, 500, 700, 900 as in font weights, faint to
    /// strong): an explicit 256-colour grey, so it shows in terminals that ignore faint (SGR 2).
    Shade(u8),
    /// A grey in bold: bold text (bionic word heads, headings) dimmed keeps its weight.
    ShadeBold(u8),
    /// A URL that is its own text, written as a terminal hyperlink (OSC 8) so a click opens it
    /// where the terminal runs (a browser terminal included).
    Url,
}

impl Sty {
    fn ansi(self) -> &'static str {
        match self {
            Sty::Plain => "",
            Sty::Dim => "\x1b[2m",
            Sty::Bold => "\x1b[1m",
            Sty::Italic => "\x1b[3m",
            Sty::Link(_) | Sty::Url => "\x1b[4;32m",
            Sty::Heading(1) => "\x1b[1;38;5;117m",
            Sty::Heading(2) => "\x1b[1;38;5;110m",
            Sty::Heading(3) => "\x1b[1;38;5;151m",
            Sty::Heading(_) => "\x1b[1;3;38;5;250m",
            Sty::Code => "\x1b[36m",
            Sty::Accent => "\x1b[32m",
            Sty::Err => "\x1b[31m",
            Sty::Warn => "\x1b[33m",
            Sty::Shade(0) => "\x1b[38;5;238m",
            Sty::Shade(1) => "\x1b[38;5;241m",
            Sty::Shade(2) => "\x1b[38;5;244m",
            Sty::Shade(3) => "\x1b[38;5;247m",
            Sty::Shade(_) => "\x1b[38;5;250m",
            Sty::ShadeBold(0) => "\x1b[1;38;5;238m",
            Sty::ShadeBold(1) => "\x1b[1;38;5;241m",
            Sty::ShadeBold(2) => "\x1b[1;38;5;244m",
            Sty::ShadeBold(3) => "\x1b[1;38;5;247m",
            Sty::ShadeBold(_) => "\x1b[1;38;5;250m",
        }
    }
}

pub type Seg = (String, Sty);
pub type Line = Vec<Seg>;

pub fn line(text: impl Into<String>, sty: Sty) -> Line {
    vec![(text.into(), sty)]
}

/// Text made safe for the terminal: control characters (C0 except newline, DEL, C1) and invisible
/// format characters (`zen_proto::invisible`: bidi controls, zero-width spaces, tags) are
/// dropped and tabs become four spaces. Model replies, tool output, files and kernel messages can
/// carry escape sequences that would otherwise set the window title, move the cursor, clear the
/// screen or write to the clipboard, and invisible characters that hide or reorder what is shown;
/// both would also throw off width-based layout.
pub fn sanitize(s: &str) -> std::borrow::Cow<'_, str> {
    let unsafe_char = |c: char| (c.is_control() && c != '\n') || zen_proto::invisible(c); // C0, DEL, C1, invisible
    if !s.contains(unsafe_char) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if unsafe_char(c) => {}
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// A styled line as ANSI text, for one terminal row. Every segment is sanitized here, the one
/// place all styled text passes through on its way to the terminal, and a stray newline becomes
/// a space so a line stays one row.
pub fn to_ansi(l: &Line) -> String {
    let mut out = String::new();
    for (t, s) in l {
        let t = sanitize(t);
        let t = if t.contains('\n') { std::borrow::Cow::Owned(t.replace('\n', " ")) } else { t };
        if *s == Sty::Plain {
            out.push_str(&t);
        } else if *s == Sty::Url {
            // Sanitized above, so the target can't end the sequence early.
            out.push_str(&format!("\x1b]8;;{t}\x1b\\{}{t}\x1b[0m\x1b]8;;\x1b\\", s.ansi()));
        } else {
            out.push_str(s.ansi());
            out.push_str(&t);
            out.push_str("\x1b[0m");
        }
    }
    out
}

/// `text` in `sty`, with each http(s) URL in it as a hyperlink (`Sty::Url`).
pub fn linked(text: &str, sty: Sty) -> Line {
    let mut out: Line = Vec::new();
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            push(&mut out, " ", sty);
        }
        if word.starts_with("https://") || word.starts_with("http://") {
            out.push((word.to_string(), Sty::Url));
        } else {
            push(&mut out, word, sty);
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
    wrap_to(segs, width.max(12), prefix, cont)
}

/// `wrap` to exactly `width` columns, however narrow (a table's cells).
fn wrap_to(segs: Vec<Seg>, width: usize, prefix: Seg, cont: Seg) -> Vec<Line> {
    let width = width.max(1);
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
                // A word longer than the line is cut between user-perceived characters (grapheme
                // clusters), measured the way the whole word was: a ZWJ emoji or ❤️ (with its
                // variation selector) stays whole and as wide as the terminal draws it.
                for g in tok.graphemes(true) {
                    let gw = UnicodeWidthStr::width(g);
                    if w + gw > width && w > start_w {
                        lines.push(std::mem::replace(&mut cur, vec![cont.clone()]));
                        start_w = cont_w;
                        w = cont_w;
                    }
                    push(&mut cur, g, sty);
                    w += gw;
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

/// Inline markdown: `code` and **bold**. Linear in the line's length: where the next backtick
/// and the next `**` start is worked out once, from the end, so an unmatched delimiter in a long
/// line (a streamed reply's unfinished last line is rendered again as it grows) doesn't cost a
/// search to the end of the line at every position.
fn inline(text: &str, base: Sty) -> Vec<Seg> {
    inline_in(text, base, None)
}

/// `inline`; with `links` (the reader), also links shown by their text (`[[page|alias]]`,
/// `[text](url)`, `![[embed]]`, each a `Sty::Link` into `links`), `$math$` in Unicode, `\` escapes,
/// and markdown inside bold.
fn inline_in(text: &str, base: Sty, mut links: Option<&mut Vec<(String, bool)>>) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    // next_tick[i]: the first backtick at or after i; next_stars[i]: the first `**` at or after i;
    // next_em[i]: the first `*` or `_` at or after i that can close emphasis (`*`/`_` alone, after
    // a non-space; a `_` not followed by a letter or digit, so snake_case stays as typed).
    let mut next_tick = vec![None; chars.len() + 1];
    let mut next_stars = vec![None; chars.len() + 1];
    let mut next_em: Vec<[Option<usize>; 2]> = vec![[None; 2]; chars.len() + 1];
    let alnum = |k: Option<&char>| k.is_some_and(|c| c.is_alphanumeric());
    for i in (0..chars.len()).rev() {
        next_tick[i] = if chars[i] == '`' { Some(i) } else { next_tick[i + 1] };
        next_stars[i] = if chars[i] == '*' && chars.get(i + 1) == Some(&'*') { Some(i) } else { next_stars[i + 1] };
        next_em[i] = next_em[i + 1];
        let single = chars.get(i + 1) != Some(&chars[i]) && (i == 0 || chars[i - 1] != chars[i]);
        let after_text = i > 0 && !chars[i - 1].is_whitespace();
        if single && after_text && chars[i] == '*' {
            next_em[i][0] = Some(i);
        }
        if single && after_text && chars[i] == '_' && !alnum(chars.get(i + 1)) {
            next_em[i][1] = Some(i);
        }
    }
    let mut i = 0;
    let flush = |cur: &mut String, segs: &mut Vec<Seg>| {
        if !cur.is_empty() {
            segs.push((std::mem::take(cur), base));
        }
    };
    while i < chars.len() {
        if let Some(links) = links.as_deref_mut() {
            if let Some((shown, sty, len)) = reader_inline(&chars, i, base, links) {
                flush(&mut cur, &mut segs);
                segs.extend(shown.into_iter().map(|t| (t, sty)));
                i += len;
                continue;
            }
        }
        if chars[i] == '`' {
            if let Some(j) = next_tick[i + 1] {
                flush(&mut cur, &mut segs);
                segs.push((chars[i + 1..j].iter().collect(), Sty::Code));
                i = j + 1;
                continue;
            }
        }
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(j) = next_stars.get(i + 2).copied().flatten() {
                flush(&mut cur, &mut segs);
                let sty = if base == Sty::Dim { Sty::Dim } else { Sty::Bold };
                let inner: String = chars[i + 2..j].iter().collect();
                match links.as_deref_mut() {
                    Some(l) => segs.extend(inline_in(&inner, sty, Some(l)).into_iter().map(|(t, s)| (t, if s == sty || s == Sty::Italic { sty } else { s }))),
                    None => segs.push((inner, sty)),
                }
                i = j + 2;
                continue;
            }
        }
        // Emphasis: a lone `*` or `_` before a non-space (a `_` not inside a word), up to its match.
        if let Some(d) = ['*', '_'].iter().position(|c| *c == chars[i]) {
            let single = chars.get(i + 1) != Some(&chars[i]) && (i == 0 || chars[i - 1] != chars[i]);
            let opens = single && chars.get(i + 1).is_some_and(|c| !c.is_whitespace()) && (d == 0 || !alnum(i.checked_sub(1).map(|k| &chars[k])));
            if let Some(j) = next_em.get(i + 1).and_then(|e| e[d]).filter(|_| opens) {
                flush(&mut cur, &mut segs);
                segs.push((chars[i + 1..j].iter().collect(), if base == Sty::Plain { Sty::Italic } else { base }));
                i = j + 1;
                continue;
            }
        }
        cur.push(chars[i]);
        i += 1;
    }
    flush(&mut cur, &mut segs);
    segs
}

/// At `chars[i]`, a reader-only inline: the text to show, its style and the characters it
/// takes. Links are recorded in `links`.
fn reader_inline(chars: &[char], i: usize, base: Sty, links: &mut Vec<(String, bool)>) -> Option<(Vec<String>, Sty, usize)> {
    let rest = &chars[i..];
    let starts = |p: &str| p.chars().enumerate().all(|(k, c)| rest.get(k) == Some(&c));
    let find = |from: usize, p: &str| -> Option<usize> {
        let p: Vec<char> = p.chars().collect();
        (from..rest.len().saturating_sub(p.len() - 1)).find(|k| rest[*k..].starts_with(&p))
    };
    let mut link = |target: String, wiki: bool, shown: String, len: usize| {
        let n = links.len().min(u16::MAX as usize) as u16;
        links.push((target, wiki));
        Some((vec![shown], Sty::Link(n), len))
    };
    // `\*`: the character as typed.
    if rest[0] == '\\' && rest.get(1).is_some_and(|c| c.is_ascii_punctuation()) {
        return Some((vec![rest[1].to_string()], base, 2));
    }
    // [[page]], [[page|alias]], [[page#heading]], ![[embed]].
    let embed = starts("![[");
    if embed || starts("[[") {
        let open = if embed { 3 } else { 2 };
        let e = find(open, "]]")?;
        let inner: String = rest[open..e].iter().collect();
        if inner.contains('[') || inner.trim().is_empty() {
            return None;
        }
        let (target, alias) = match inner.split_once('|') {
            Some((t, a)) => (t.trim().to_string(), Some(a.trim().to_string())),
            None => (inner.trim().to_string(), None),
        };
        let name = || {
            let t = target.trim_start_matches('#');
            let page = t.split('#').next().unwrap_or(t);
            let page = page.rsplit('/').next().unwrap_or(page).trim_end_matches(".md");
            match t.split_once('#') {
                Some((p, h)) if !p.is_empty() => format!("{page} › {}", h.trim_start_matches('^')),
                _ if target.starts_with('#') => t.to_string(),
                _ => page.to_string(),
            }
        };
        let shown = alias.unwrap_or_else(name);
        return link(target.clone(), true, if embed { format!("⧉ {shown}") } else { shown }, e + 2);
    }
    // [text](url) and ![alt](url).
    let image = starts("![");
    if image || rest[0] == '[' {
        let open = if image { 2 } else { 1 };
        let m = find(open, "](")?;
        let text: String = rest[open..m].iter().collect();
        if text.contains(['[', ']']) {
            return None;
        }
        let e = (m + 2..rest.len()).find(|k| rest[*k] == ')')?;
        let url: String = rest[m + 2..e].iter().collect();
        if url.contains(' ') && !url.contains("%20") && url.trim().contains(' ') {
            return None;
        }
        let shown = if text.trim().is_empty() { url.clone() } else { text };
        return link(url, false, if image { format!("🖼 {shown}") } else { shown }, e + 1);
    }
    // $math$: not after a letter or digit (R$5), not before a space; the closing `$` after a
    // non-space and not before a digit.
    if rest[0] == '$' && rest.get(1) != Some(&'$') {
        let before = i.checked_sub(1).map(|k| chars[k]);
        if before.is_some_and(|c| c.is_alphanumeric() || c == '$') || rest.get(1).is_none_or(|c| c.is_whitespace()) {
            return None;
        }
        let e = (2..rest.len()).find(|k| rest[*k] == '$' && rest[*k - 1] != '\\')?;
        if rest[e - 1].is_whitespace() || rest.get(e + 1).is_some_and(|c| c.is_ascii_digit() || *c == '$') {
            return None;
        }
        let tex: String = rest[1..e].iter().collect();
        return Some((vec![crate::tex::to_unicode(&tex)], if base == Sty::Plain { Sty::Italic } else { base }, e + 1));
    }
    None
}

/// Stateful markdown renderer (tracks code fences across lines). `Md::reader()` renders a file
/// for reading: links by their text, math, callouts, checkboxes, rules and query blocks.
#[derive(Default, Clone)]
pub struct Md {
    in_code: bool,
    /// Reading a file, not a chat.
    reader: bool,
    /// Inside an Obsidian query block (`tasks`, `dataview`): shown dimmed, not as code.
    query: bool,
    /// Inside a callout (`> [!note]`): its bar is in colour.
    callout: bool,
    /// The links shown so far, in order: (target, a `[[wiki link]]`). `Sty::Link(i)` is `links[i]`.
    pub links: Vec<(String, bool)>,
}

impl Md {
    pub fn reader() -> Md {
        Md { reader: true, ..Md::default() }
    }

    /// Inline markdown, with the reader's extras when reading.
    fn inl(&mut self, text: &str, base: Sty) -> Vec<Seg> {
        if self.reader {
            inline_in(text, base, Some(&mut self.links))
        } else {
            inline(text, base)
        }
    }

    /// Inside a code fence.
    pub fn in_code(&self) -> bool {
        self.in_code
    }

    pub fn render(&mut self, text: &str, width: usize) -> Vec<Line> {
        text.split('\n').flat_map(|l| self.render_line(l, width)).collect()
    }

    pub fn render_line(&mut self, src: &str, width: usize) -> Vec<Line> {
        let t = src.trim_end();
        let none = || (String::new(), Sty::Plain);
        if let Some(lang) = t.trim_start().strip_prefix("```") {
            self.in_code = !self.in_code;
            self.query = false;
            // An Obsidian query is a live list in Obsidian; here, what it asks, dimmed.
            let lang = lang.trim();
            if self.reader && self.in_code && ["tasks", "dataview", "dataviewjs", "query"].contains(&lang) {
                self.query = true;
                return vec![vec![("⧉ ".into(), Sty::Dim), (format!("{lang} query (a live list in Obsidian)"), Sty::Dim)]];
            }
            return Vec::new();
        }
        if self.in_code {
            let sty = if self.query { Sty::Dim } else { Sty::Code };
            return wrap(vec![(t.to_string(), sty)], width, ("  ".into(), Sty::Plain), ("  ".into(), Sty::Plain));
        }
        let body = t.trim_start();
        if !body.starts_with('>') {
            self.callout = false;
        }
        if t.is_empty() {
            return vec![Vec::new()];
        }
        let lead = t.len() - body.len();
        if let Some(h) = body.strip_prefix('#') {
            let level = 1 + h.chars().take_while(|c| *c == '#').count();
            let h = h.trim_start_matches('#');
            if h.starts_with(' ') && level <= 6 {
                if !self.reader {
                    return wrap(inline(h.trim(), Sty::Bold), width, none(), none());
                }
                // Each level its own colour; the top two over a rule, as a page title and a
                // section read in Obsidian.
                let sty = Sty::Heading(level as u8);
                let segs = self.inl(h.trim().trim_end_matches('#').trim_end(), sty);
                let mut rows = wrap(segs, width, none(), none());
                let w = rows.iter().map(|r| r.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum::<usize>()).max().unwrap_or(0);
                match level {
                    1 => rows.push(vec![("━".repeat(width.clamp(3, 72)), sty)]),
                    2 => rows.push(vec![("─".repeat(w.max(3)), Sty::Dim)]),
                    _ => {}
                }
                return rows;
            }
        }
        // A thematic break: ---, ***, ___.
        let squeezed: String = body.chars().filter(|c| *c != ' ').collect();
        if squeezed.len() >= 3 && ["-", "*", "_"].iter().any(|m| squeezed.chars().all(|c| c.to_string() == *m)) {
            return vec![vec![("─".repeat(width.clamp(3, 72)), Sty::Dim)]];
        }
        let marker = if body.starts_with("- ") || body.starts_with("* ") || body.starts_with("+ ") {
            Some(("•".to_string(), &body[2..]))
        } else {
            let digits = body.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && (body[digits..].starts_with(". ") || body[digits..].starts_with(") ")) {
                Some((body[..digits + 1].to_string(), &body[digits + 2..]))
            } else {
                None
            }
        };
        if let Some((m, rest)) = marker {
            // A task: its box instead of the bullet; done or dropped ones dimmed.
            let (m, rest, base) = match rest.get(..4) {
                Some("[ ] ") => ("☐".to_string(), &rest[4..], Sty::Plain),
                Some("[x] " | "[X] ") => ("☑".to_string(), &rest[4..], Sty::Dim),
                Some("[-] ") => ("☒".to_string(), &rest[4..], Sty::Dim),
                Some(b) if b.starts_with('[') && b.ends_with("] ") && b.chars().count() == 4 => (b[..3].to_string(), &rest[4..], Sty::Plain),
                _ => (m, rest, Sty::Plain),
            };
            let prefix = format!("{}{} ", " ".repeat(lead), m);
            let cont = " ".repeat(UnicodeWidthStr::width(prefix.as_str()));
            let segs = self.inl(rest, base);
            let segs = if base == Sty::Dim { segs.into_iter().map(|(t, s)| (t, if matches!(s, Sty::Link(_)) { s } else { Sty::Dim })).collect() } else { segs };
            return wrap(segs, width, (prefix, Sty::Dim), (cont, Sty::Plain));
        }
        if let Some(q) = body.strip_prefix('>') {
            if !self.reader {
                return wrap(inline(q.trim_start(), Sty::Dim), width, ("│ ".into(), Sty::Dim), ("│ ".into(), Sty::Dim));
            }
            let inner = q.strip_prefix(' ').unwrap_or(q);
            // A callout's first line: its title (else its kind) in bold, the bar in colour.
            if let Some(head) = inner.strip_prefix("[!") {
                if let Some((kind, title)) = head.split_once(']') {
                    self.callout = true;
                    let title = title.trim_start_matches(['+', '-']).trim();
                    let mut name = kind.to_lowercase();
                    if let Some(f) = name.get(..1) {
                        name = f.to_uppercase() + &name[1..];
                    }
                    let segs = if title.is_empty() { vec![(name, Sty::Bold)] } else { self.inl(title, Sty::Bold) };
                    return wrap(segs, width, ("┃ ".into(), Sty::Accent), ("┃ ".into(), Sty::Accent));
                }
            }
            // The quote's own markdown (lists, bold, links), behind the bar.
            let bar = if self.callout { ("┃ ".to_string(), Sty::Accent) } else { ("│ ".to_string(), Sty::Dim) };
            let (code, query, callout) = (self.in_code, self.query, self.callout);
            self.in_code = false;
            let mut rows = self.render_line(inner, width.saturating_sub(2).max(10));
            (self.in_code, self.query, self.callout) = (code, query, callout);
            if rows.is_empty() {
                rows.push(Vec::new());
            }
            for r in &mut rows {
                r.insert(0, bar.clone());
            }
            return rows;
        }
        // Display math on its line: $$…$$.
        if self.reader {
            if let Some(m) = body.strip_prefix("$$").and_then(|m| m.trim_end().strip_suffix("$$")) {
                let shown = crate::tex::to_unicode(m);
                return wrap(vec![(shown, Sty::Italic)], width, ("    ".into(), Sty::Plain), ("    ".into(), Sty::Plain));
            }
        }
        let segs = self.inl(t, Sty::Plain);
        wrap(segs, width, none(), none())
    }

    /// A table (its source rows, the second the `|---|` line) laid out in `width` columns: cells
    /// wrapped in their columns, the header in bold over a rule. Columns share what doesn't fit,
    /// the widest giving way first.
    pub fn table(&mut self, rows: &[&str], width: usize) -> Vec<Line> {
        let cells = |r: &str| -> Vec<String> {
            let r = r.trim();
            let r = r.strip_prefix('|').unwrap_or(r);
            let r = r.strip_suffix('|').filter(|x| !x.ends_with('\\')).unwrap_or(r);
            let mut out = vec![String::new()];
            let mut esc = false;
            for c in r.chars() {
                if c == '|' && !esc {
                    out.push(String::new());
                } else {
                    out.last_mut().unwrap().push(c);
                }
                esc = c == '\\';
            }
            out.into_iter().map(|c| c.trim().replace("\\|", "|")).collect()
        };
        let align: Vec<bool> = cells(rows.get(1).copied().unwrap_or("")).iter().map(|c| c.ends_with(':') && !c.starts_with(':')).collect();
        let body: Vec<Vec<Vec<Seg>>> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(i, r)| cells(r).iter().map(|c| self.inl(c, if i == 0 { Sty::Bold } else { Sty::Plain })).collect())
            .collect();
        let n = body.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let seg_w = |c: &Vec<Seg>| c.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum::<usize>();
        let mut w: Vec<usize> = (0..n).map(|k| body.iter().filter_map(|r| r.get(k)).map(seg_w).max().unwrap_or(0).max(1)).collect();
        let room = width.saturating_sub(3 * (n - 1)).max(n * 4);
        while w.iter().sum::<usize>() > room {
            let (k, _) = w.iter().enumerate().max_by_key(|(_, x)| **x).unwrap();
            if w[k] <= 4 {
                break;
            }
            w[k] -= 1;
        }
        let mut out = Vec::new();
        for (ri, r) in body.iter().enumerate() {
            let wrapped: Vec<Vec<Line>> = (0..n).map(|k| wrap_to(r.get(k).cloned().unwrap_or_default(), w[k], (String::new(), Sty::Plain), (String::new(), Sty::Plain))).collect();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
            for h in 0..height {
                let mut l: Line = Vec::new();
                for k in 0..n {
                    if k > 0 {
                        l.push((" │ ".into(), Sty::Dim));
                    }
                    let cell = wrapped[k].get(h).cloned().unwrap_or_default();
                    let cw: usize = seg_w(&cell);
                    let pad = " ".repeat(w[k].saturating_sub(cw));
                    if align.get(k) == Some(&true) {
                        l.push((pad, Sty::Plain));
                        l.extend(cell);
                    } else {
                        l.extend(cell);
                        if k + 1 < n {
                            l.push((pad, Sty::Plain));
                        }
                    }
                }
                l.retain(|(t, _)| !t.is_empty());
                out.push(l);
            }
            if ri == 0 {
                let rule: Vec<String> = w.iter().map(|x| "─".repeat(*x)).collect();
                out.push(vec![(rule.join("─┼─"), Sty::Dim)]);
            }
        }
        out
    }
}

/// A file's markdown lines made ready to read: lines a hard wrap split are joined again (a long
/// line without a closing stop, followed by more of the same paragraph; short lines stay apart, as
/// Obsidian shows them), and a `$$` block over several lines becomes one line. Code is untouched.
/// HTML comments on lines of their own are left out, as Obsidian's reading view hides them: they
/// hold what a tool keeps behind the text (the writer's alternatives and ghosted text).
pub fn reader_source(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut code = false;
    let mut comment = false;
    let mut math: Option<String> = None;
    let mut joinable = false;
    for raw in lines {
        let l = raw.trim_end_matches('\r');
        let body = l.trim_start();
        if body.starts_with("```") {
            code = !code;
            out.push(l.to_string());
            joinable = false;
            continue;
        }
        if code {
            out.push(l.to_string());
            continue;
        }
        if comment {
            comment = !body.contains("-->");
            continue;
        }
        if let Some(rest) = body.strip_prefix("<!--") {
            match rest.find("-->") {
                Some(e) if rest[e + 3..].trim().is_empty() => continue,
                Some(_) => {}
                None => {
                    comment = true;
                    continue;
                }
            }
        }
        if let Some(m) = &mut math {
            // Inside $$ … $$: gather to the closing marks, then any text after them.
            match body.find("$$") {
                Some(e) => {
                    m.push(' ');
                    m.push_str(&body[..e]);
                    out.push(format!("$${}$$", m.trim()));
                    math = None;
                    let after = body[e + 2..].trim();
                    if !after.is_empty() {
                        out.push(after.to_string());
                    }
                }
                None => {
                    m.push(' ');
                    m.push_str(body);
                }
            }
            joinable = false;
            continue;
        }
        if let Some(m) = body.strip_prefix("$$") {
            match m.find("$$") {
                Some(e) => {
                    out.push(format!("$${}$$", &m[..e]));
                    let after = m[e + 2..].trim();
                    if !after.is_empty() {
                        out.push(after.to_string());
                    }
                }
                None => math = Some(m.to_string()),
            }
            joinable = false;
            continue;
        }
        let block = body.is_empty() || body.starts_with(['#', '|', '<']) || body.starts_with("- ") || body.starts_with("* ") || body.starts_with("+ ") || {
            let d = body.chars().take_while(char::is_ascii_digit).count();
            d > 0 && (body[d..].starts_with(". ") || body[d..].starts_with(") "))
        } || body.chars().all(|c| "-*_ ".contains(c));
        let quote = |s: &str| s.trim_start().strip_prefix('>').map(|q| q.strip_prefix(' ').unwrap_or(q).to_string());
        if joinable {
            let prev = out.last().cloned().unwrap_or_default();
            // In a quote, only the quote's own lines join.
            let (pq, cq) = (quote(&prev), quote(l));
            let cur_inner = cq.clone().unwrap_or_else(|| body.to_string());
            let inner_block = cq.as_deref().is_some_and(|c| c.is_empty() || c.starts_with(['#', '|', '[', '>']) || c.starts_with("- ") || c.starts_with("* "));
            if pq.is_some() == cq.is_some() && !((cq.is_none() && block) || inner_block) {
                let last = out.last_mut().unwrap();
                last.push(' ');
                last.push_str(cur_inner.trim());
                joinable = wrapped_line(&cur_inner);
                continue;
            }
        }
        out.push(l.to_string());
        let inner = quote(l).unwrap_or_else(|| body.to_string());
        let heading = body.starts_with('#') || body.starts_with('|') || body.starts_with("[!") || inner.starts_with("[!");
        joinable = !body.is_empty() && !heading && wrapped_line(&inner);
    }
    if let Some(m) = math {
        out.push(format!("$${}", m));
    }
    out
}

/// A line that looks cut by a hard wrap: long, with no stop or hard break at its end.
fn wrapped_line(l: &str) -> bool {
    let t = l.trim_start();
    let t = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).unwrap_or(t);
    if l.ends_with("  ") || l.ends_with('\\') {
        return false;
    }
    let t = t.trim_end();
    UnicodeWidthStr::width(t) >= 60 && !t.ends_with(['.', ':', '!', '?', '|'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_drops_invisible_reordering_and_tag_characters() {
        // "evil.exe" shown as "exe.live" by a right-to-left override, and an invisible tag message.
        assert_eq!(sanitize("file\u{202E}exe.live"), "fileexe.live");
        assert_eq!(sanitize("ok\u{E0041}\u{E0042}"), "ok");
        assert_eq!(sanitize("a\u{200B}b\u{FEFF}"), "ab");
        assert_eq!(sanitize("👨\u{200D}👩"), "👨\u{200D}👩", "emoji joiners stay");
    }

    #[test]
    fn escape_sequences_never_reach_the_terminal() {
        let evil = "title\x1b]0;pwned\x07 \x1b[2J\x1b[5A\u{9b}31m\x7f\ttab\r";
        assert_eq!(sanitize(evil), "title]0;pwned [2J[5A31m    tab");
        assert_eq!(sanitize("line one\nline two"), "line one\nline two", "newlines stay in text");
        assert!(matches!(sanitize("plain"), std::borrow::Cow::Borrowed(_)));
        let out = to_ansi(&vec![(evil.to_string(), Sty::Err), ("a\nb".to_string(), Sty::Plain)]);
        assert_eq!(out, "\x1b[31mtitle]0;pwned [2J[5A31m    tab\x1b[0ma b", "only our own styling escapes are left");
    }

    #[test]
    fn reader_leaves_out_html_comments() {
        let src = [
            "# Note",
            "",
            "Kept text.",
            "<!-- one line -->",
            "",
            "<!-- zen:alternatives",
            "{\"version\":1,\"ghosts\":[{\"text\":\"Cut this.\"}]}",
            "-->",
            "<!-- inline --> and text stays",
            "```",
            "<!-- in code stays -->",
            "```",
        ];
        let out = reader_source(&src);
        let all = out.join("\n");
        assert!(!all.contains("zen:alternatives") && !all.contains("Cut this") && !all.contains("one line"), "{all}");
        assert!(all.contains("Kept text.") && all.contains("<!-- inline --> and text stays"), "{all}");
        assert!(all.contains("<!-- in code stays -->"), "code is untouched");
    }

    fn text(l: &Line) -> String {
        l.iter().map(|(t, _)| t.as_str()).collect()
    }

    fn plain(text: &str) -> Vec<Seg> {
        vec![(text.to_string(), Sty::Plain)]
    }

    fn none() -> Seg {
        (String::new(), Sty::Plain)
    }

    #[test]
    fn wrap_breaks_at_spaces_with_prefixes_and_cuts_long_words() {
        let lines = wrap(plain("the quick brown fox jumps over the lazy dog"), 16, ("> ".into(), Sty::Dim), ("  ".into(), Sty::Plain));
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t, ["> the quick", "  brown fox", "  jumps over the", "  lazy dog"]);
        assert!(t.iter().all(|l| UnicodeWidthStr::width(l.as_str()) <= 16));
        // A word longer than the line is cut where the line ends.
        let t: Vec<String> = wrap(plain("abcdefghijklmnopqrstuvwxyz"), 12, none(), none()).iter().map(text).collect();
        assert_eq!(t, ["abcdefghijkl", "mnopqrstuvwx", "yz"]);
        // Leading indentation stays on the first line (code); styles stay with their text.
        let lines = wrap(vec![("    x = 1".into(), Sty::Code)], 20, none(), none());
        assert_eq!(text(&lines[0]), "    x = 1");
        assert!(lines[0].iter().all(|(t, s)| t.is_empty() || *s == Sty::Code));
        // Wide characters count two columns.
        let t: Vec<String> = wrap(plain("界界界界界界界界"), 12, none(), none()).iter().map(text).collect();
        assert_eq!(t, ["界界界界界界", "界界"]);
    }

    #[test]
    fn wrap_never_splits_an_emoji_sequence() {
        let family = "👨\u{200d}👩\u{200d}👧";
        let word = format!("{}{}", "❤\u{fe0f}".repeat(5), family.repeat(5));
        for l in wrap(plain(&word), 12, none(), none()) {
            let t = text(&l);
            assert!(UnicodeWidthStr::width(t.as_str()) <= 12, "{t:?}");
            assert!(!t.starts_with('\u{fe0f}') && !t.starts_with('\u{200d}') && !t.ends_with('\u{200d}'), "{t:?}");
        }
    }

    #[test]
    fn markdown_basics() {
        let mut md = Md::default();
        let r = |md: &mut Md, l: &str| md.render_line(l, 40);
        // Headings are bold, without the hashes.
        let h = r(&mut md, "## A **big** title");
        assert_eq!(text(&h[0]), "A big title");
        assert!(h[0].iter().all(|(t, s)| t.is_empty() || *s == Sty::Bold), "{h:?}");
        // Inline code and bold.
        let p = r(&mut md, "run `cargo test` **now**");
        assert_eq!(p[0], [("run ".into(), Sty::Plain), ("cargo test".into(), Sty::Code), (" ".into(), Sty::Plain), ("now".into(), Sty::Bold)]);
        // Emphasis in italics, with either mark; snake_case, a lone star and 2 * 3 stay as typed.
        let e = r(&mut md, "a *power* and _this_ here");
        assert_eq!(e[0], [("a ".into(), Sty::Plain), ("power".into(), Sty::Italic), (" and ".into(), Sty::Plain), ("this".into(), Sty::Italic), (" here".into(), Sty::Plain)]);
        for raw in ["snake_case_name here", "2 * 3 * 4", "a * b", "file_name.md and *open", "*not closed *", "**x"] {
            assert_eq!(text(&r(&mut md, raw)[0]), raw, "{raw}");
            assert!(r(&mut md, raw)[0].iter().all(|(_, s)| *s != Sty::Italic), "{raw}");
        }
        assert_eq!(text(&r(&mut md, "**bold** and *em*")[0]), "bold and em");
        // Lists: bullets and numbers, continuation lines indented under the text.
        assert_eq!(text(&r(&mut md, "- item")[0]), "• item");
        assert_eq!(text(&r(&mut md, "12. twelfth")[0]), "12. twelfth");
        let long = r(&mut md, "- a list item long enough to wrap onto a second line");
        assert!(text(&long[1]).starts_with("  ") && !text(&long[1]).starts_with("   "), "{long:?}");
        // Quotes.
        assert_eq!(text(&r(&mut md, "> quoted")[0]), "│ quoted");
        // Code fences: the fence lines vanish, the code is indented, and markdown inside is literal.
        assert!(r(&mut md, "```rust").is_empty());
        let code = r(&mut md, "let x = **y**; // # not a heading");
        assert_eq!(text(&code[0]), "  let x = **y**; // # not a heading");
        assert!(code[0].iter().any(|(_, s)| *s == Sty::Code));
        assert!(r(&mut md, "```").is_empty());
        assert_eq!(text(&r(&mut md, "**bold** again")[0]), "bold again", "the fence closed");
        // Blank lines stay; a whole text renders line by line.
        assert_eq!(Md::default().render("a\n\nb", 40).len(), 3);
    }

    #[test]
    fn the_reader_shows_links_by_their_text_math_callouts_tasks_and_rules() {
        let mut md = Md::reader();
        let r = |md: &mut Md, l: &str| md.render_line(l, 60).iter().map(text).collect::<Vec<_>>().join("\n");
        assert_eq!(r(&mut md, "see [[Economic moat|moats]], [[Alpha decay]], [[Goals/X#Why]] and [web](https://x.y)"), "see moats, Alpha decay, X › Why and web");
        assert_eq!(md.links, [("Economic moat".to_string(), true), ("Alpha decay".into(), true), ("Goals/X#Why".into(), true), ("https://x.y".into(), false)]);
        let l = md.render_line("a [[b]] c", 60);
        assert!(l[0].iter().any(|(t, s)| t == "b" && *s == Sty::Link(4)), "{l:?}");
        assert_eq!(r(&mut md, "![[diagram.png]] **bold [[x|y]]**"), "⧉ diagram.png bold y");
        // Math, but not money; escapes as typed.
        assert_eq!(r(&mut md, r"growth $\alpha_t = e^{-\lambda t}$ for $5M or $5 and $6 \*not em\*"), "growth αₜ = e^(-λ t) for $5M or $5 and $6 *not em*");
        assert_eq!(r(&mut md, r"$$IP= \frac{a}{b+c}$$"), "    IP= a/(b+c)");
        // Callouts: the title, then the body's own markdown behind a bar.
        assert_eq!(r(&mut md, "> [!note] zenbot, today"), "┃ zenbot, today");
        assert_eq!(r(&mut md, "> - a **point**"), "┃ • a point");
        assert_eq!(r(&mut md, "plain"), "plain");
        assert_eq!(r(&mut md, "> [!WARNING]"), "┃ Warning");
        assert_eq!(r(&mut md, "> quoted"), "┃ quoted");
        md.render_line("", 60);
        assert_eq!(r(&mut md, "> quoted"), "│ quoted");
        // Headings: a colour per level, the top two over a rule; the chat keeps them bold.
        let h1 = md.render_line("# Title [[x|link]]", 20);
        assert_eq!(h1.iter().map(text).collect::<Vec<_>>(), ["Title link", &"━".repeat(20)]);
        assert!(h1[0].iter().any(|(t, s)| t == "Title " && *s == Sty::Heading(1)) && h1[0].iter().any(|(_, s)| matches!(s, Sty::Link(_))));
        assert_eq!(r(&mut md, "## Section ##"), "Section\n───────");
        assert!(md.render_line("### Sub", 60)[0].iter().any(|(t, s)| t == "Sub" && *s == Sty::Heading(3)));
        assert!(md.render_line("#### Deep", 60)[0].iter().any(|(t, s)| t == "Deep" && *s == Sty::Heading(4)));
        assert!(Md::default().render_line("# Chat", 60)[0].iter().any(|(t, s)| t == "Chat" && *s == Sty::Bold));
        // Tasks, rules, query blocks.
        assert_eq!(r(&mut md, "- [ ] open"), "☐ open");
        assert_eq!(r(&mut md, "- [x] done"), "☑ done");
        assert!(md.render_line("- [x] done", 60)[0].iter().skip(1).all(|(_, s)| *s == Sty::Dim));
        assert_eq!(r(&mut md, "---"), "─".repeat(60));
        assert_eq!(r(&mut md, "```tasks"), "⧉ tasks query (a live list in Obsidian)");
        assert_eq!(r(&mut md, "not done"), "  not done");
        assert!(md.render_line("```", 60).is_empty() && r(&mut md, "**b**") == "b");
        // The chat keeps its links as written.
        assert_eq!(text(&Md::default().render_line("[[a|b]] $x$", 60)[0]), "[[a|b]] $x$");
    }

    #[test]
    fn tables_fit_their_width_and_hard_wrapped_lines_join() {
        let mut md = Md::reader();
        let rows = ["| Fruit | Allowed | Note |", "|---|:---|---:|", "| Pear | yes | 30 days |", r"| Stone fruit (plum, peach, cherry) | yes \| maybe | [[x\|link]] |"];
        let t: Vec<String> = md.table(&rows, 30).iter().map(text).collect();
        assert!(t.iter().all(|r| UnicodeWidthStr::width(r.as_str()) <= 30), "{t:#?}");
        assert!(t[0].starts_with("Fruit") && t[1].contains("┼") && t[2].starts_with("Pear"), "{t:#?}");
        assert!(t.iter().any(|r| r.contains("yes |")) && t.iter().any(|r| r.contains("maybe")) && t.iter().any(|r| r.contains("link")), "{t:#?}");
        assert!(t[2].ends_with("30 days"), "the last column aligned right: {t:#?}");
        let src = [
            "A long line that a hard wrap cut in the middle of its sentence, so",
            "it goes on here.",
            "Receitas: maçãs, pêras, ameixas, pêssegos, cerejas, figos, outras.",
            "Notas: short.",
            "- a list item long enough to have been cut by the hard wrap, and",
            "  its continuation",
            "> a quote line long enough to have been cut by the hard wrap, then",
            "> more of it",
            "$$",
            r"x = \frac{1}{2}",
            "$$ and after",
            "```",
            "code line long enough that it would be joined if it were prose text",
            "next",
            "```",
        ];
        assert_eq!(reader_source(&src), [
            "A long line that a hard wrap cut in the middle of its sentence, so it goes on here.",
            "Receitas: maçãs, pêras, ameixas, pêssegos, cerejas, figos, outras.",
            "Notas: short.",
            "- a list item long enough to have been cut by the hard wrap, and its continuation",
            "> a quote line long enough to have been cut by the hard wrap, then more of it",
            r"$$x = \frac{1}{2}$$",
            "and after",
            "```",
            "code line long enough that it would be joined if it were prose text",
            "next",
            "```",
        ]);
    }

    #[test]
    fn a_long_line_with_unmatched_delimiters_renders_as_typed_and_fast() {
        // A streamed reply's unfinished last line renders again with every frame; an unmatched
        // backtick or `**` there must not cost a search to the end of the line from each position.
        for open in ["`", "**", "` ** `` **", "*", "_"] {
            let line = format!("{open}{}", "word ** ` x ".repeat(20_000));
            let start = std::time::Instant::now();
            let out = Md::default().render_line(&line, 100);
            assert!(start.elapsed() < std::time::Duration::from_secs(2), "{open}: {:?}", start.elapsed());
            assert!(out.len() > 1000);
        }
        let out = Md::default().render_line("a ` b ** c", 40);
        assert_eq!(text(&out[0]), "a ` b ** c");
    }

    #[test]
    fn urls_in_a_notice_become_hyperlinks_that_cant_be_escaped() {
        let l = linked("open https://x.example/?a=1 now", Sty::Dim);
        assert_eq!(l, vec![("open ".into(), Sty::Dim), ("https://x.example/?a=1".into(), Sty::Url), (" now".into(), Sty::Dim)]);
        assert_eq!(to_ansi(&l), "\x1b[2mopen \x1b[0m\x1b]8;;https://x.example/?a=1\x1b\\\x1b[4;32mhttps://x.example/?a=1\x1b[0m\x1b]8;;\x1b\\\x1b[2m now\x1b[0m");
        let evil = to_ansi(&vec![("https://x\x1b\\\x1b]52;c;aGk=\x07".into(), Sty::Url)]);
        assert!(!evil.contains("\x1b]52") && evil.matches('\x1b').count() == 6, "{evil:?}");
    }
}
