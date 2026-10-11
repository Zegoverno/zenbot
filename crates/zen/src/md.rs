//! Styled terminal lines, word wrapping, and a small line-oriented markdown renderer.
//! Line-oriented so streamed text can be committed to scrollback as each line completes.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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

/// Text made safe for the terminal: control characters (C0 except newline, DEL, C1) are dropped
/// and tabs become four spaces. Model replies, tool output, files and kernel messages can carry
/// escape sequences that would otherwise set the window title, move the cursor, clear the screen
/// or write to the clipboard; they would also throw off width-based layout.
pub fn sanitize(s: &str) -> std::borrow::Cow<'_, str> {
    let unsafe_char = |c: char| c.is_control() && c != '\n'; // C0, DEL and C1
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
        } else {
            out.push_str(s.ansi());
            out.push_str(&t);
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
    let mut segs = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    // next_tick[i]: the first backtick at or after i; next_stars[i]: the first `**` at or after i.
    let mut next_tick = vec![None; chars.len() + 1];
    let mut next_stars = vec![None; chars.len() + 1];
    for i in (0..chars.len()).rev() {
        next_tick[i] = if chars[i] == '`' { Some(i) } else { next_tick[i + 1] };
        next_stars[i] = if chars[i] == '*' && chars.get(i + 1) == Some(&'*') { Some(i) } else { next_stars[i + 1] };
    }
    let mut i = 0;
    let flush = |cur: &mut String, segs: &mut Vec<Seg>| {
        if !cur.is_empty() {
            segs.push((std::mem::take(cur), base));
        }
    };
    while i < chars.len() {
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
#[derive(Default, Clone, Copy)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_never_reach_the_terminal() {
        let evil = "title\x1b]0;pwned\x07 \x1b[2J\x1b[5A\u{9b}31m\x7f\ttab\r";
        assert_eq!(sanitize(evil), "title]0;pwned [2J[5A31m    tab");
        assert_eq!(sanitize("line one\nline two"), "line one\nline two", "newlines stay in text");
        assert!(matches!(sanitize("plain"), std::borrow::Cow::Borrowed(_)));
        let out = to_ansi(&vec![(evil.to_string(), Sty::Err), ("a\nb".to_string(), Sty::Plain)]);
        assert_eq!(out, "\x1b[31mtitle]0;pwned [2J[5A31m    tab\x1b[0ma b", "only our own styling escapes are left");
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
    fn a_long_line_with_unmatched_delimiters_renders_as_typed_and_fast() {
        // A streamed reply's unfinished last line renders again with every frame; an unmatched
        // backtick or `**` there must not cost a search to the end of the line from each position.
        for open in ["`", "**", "` ** `` **"] {
            let line = format!("{open}{}", "word ** ` x ".repeat(20_000));
            let start = std::time::Instant::now();
            let out = Md::default().render_line(&line, 100);
            assert!(start.elapsed() < std::time::Duration::from_secs(2), "{open}: {:?}", start.elapsed());
            assert!(out.len() > 1000);
        }
        let out = Md::default().render_line("a ` b ** c", 40);
        assert_eq!(text(&out[0]), "a ` b ** c");
    }
}
