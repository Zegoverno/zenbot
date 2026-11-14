//! Full-screen frames without flicker. Every frame is built whole (one string per row), then only
//! the rows that differ from the last frame are written, each overwritten in place. Nothing is
//! cleared first, so the screen never shows a blank moment, even on terminals that ignore
//! synchronized output (DEC mode 2026, which we still send).
//!
//! Bytes are latency over a remote terminal (the browser terminal reaches the VM across a
//! continent), so a changed row is written from its first changed character, not from column 1,
//! and long runs of unstyled spaces become "erase n, move n" instead of n spaces. A typed letter
//! costs tens of bytes, not a whole row.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::md::{to_ansi, Line, Sty};

#[derive(Default)]
pub struct Screen {
    /// The rows as last written; empty means the screen must be repainted from scratch.
    prev: Vec<String>,
}

impl Screen {
    /// Forget what's on screen (after a resize): the next frame repaints every row.
    pub fn invalidate(&mut self) {
        self.prev.clear();
    }

    /// Escape codes that turn the screen into `rows` (already ANSI-rendered, one per screen row)
    /// and leave the caret at `caret` (row, column), or hide it.
    pub fn frame(&mut self, rows: Vec<String>, caret: Option<(usize, usize)>) -> String {
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        if self.prev.len() != rows.len() {
            out.push_str("\x1b[H\x1b[2J");
            self.prev.clear();
        }
        for (i, row) in rows.iter().enumerate() {
            match self.prev.get(i) {
                Some(old) if old == row => {}
                old => paint_row(&mut out, i, old.map(String::as_str).unwrap_or(""), row),
            }
        }
        if let Some((r, c)) = caret {
            out.push_str(&format!("\x1b[{};{}H\x1b[?25h", r + 1, c + 1));
        }
        out.push_str("\x1b[?2026l");
        self.prev = rows;
        out
    }

    /// The rows as last written (for tests).
    #[cfg(test)]
    pub fn rows(&self) -> &[String] {
        &self.prev
    }
}

/// A piece of an ANSI row: an escape sequence, or one grapheme cluster.
fn tokens(s: &str) -> impl Iterator<Item = &str> + '_ {
    let mut graphemes = s.grapheme_indices(true).peekable();
    std::iter::from_fn(move || {
        let (start, g) = graphemes.next()?;
        if g != "\x1b" {
            return Some(g);
        }
        // An OSC sequence (a hyperlink): ESC ] up to BEL or ESC \.
        if graphemes.peek().is_some_and(|(_, c)| *c == "]") {
            let mut end = s.len();
            while let Some((i, c)) = graphemes.next() {
                if c == "\x07" || (c == "\x1b" && graphemes.peek().is_some_and(|(_, c)| *c == "\\")) {
                    end = if c == "\x07" { i + 1 } else { graphemes.next().map(|(j, _)| j + 1).unwrap_or(s.len()) };
                    break;
                }
            }
            return Some(&s[start..end]);
        }
        // A CSI sequence: ESC [ parameters, up to a final byte in @..~ (rows hold only SGR).
        let mut end = start + 1;
        while let Some(&(i, c)) = graphemes.peek() {
            graphemes.next();
            end = i + c.len();
            if i > start + 1 && c.len() == 1 && (b'@'..=b'~').contains(&c.as_bytes()[0]) {
                break;
            }
        }
        Some(&s[start..end])
    })
}

/// Columns a grapheme takes, when every terminal agrees on it: emoji and joined sequences are
/// drawn one or two columns wide depending on the terminal, so after one the column is unknown.
fn sure_width(g: &str) -> Option<usize> {
    let mut chars = g.chars();
    let c = chars.next()?;
    let emoji = matches!(c as u32, 0x2600..=0x27BF | 0x1F000..);
    (chars.next().is_none() && !emoji).then(|| UnicodeWidthStr::width(g))
}

/// Write row `i` (0-based), which showed `old`, as `new`: from the first token that differs, with
/// the style in force there, then clear to the end of the row.
fn paint_row(out: &mut String, i: usize, old: &str, new: &str) {
    let (mut col, mut at, mut style) = (0, 0, "");
    let mut olds = tokens(old);
    for t in tokens(new) {
        if olds.next() != Some(t) {
            break;
        }
        if t.starts_with("\x1b]") {
            // A hyperlink can't be resumed mid-way: write the whole row.
            (col, at, style) = (0, 0, "");
            break;
        } else if t.starts_with('\x1b') {
            style = if t == "\x1b[0m" { "" } else { t };
        } else if let Some(w) = sure_width(t) {
            col += w;
        } else {
            (col, at, style) = (0, 0, "");
            break;
        }
        at += t.len();
    }
    out.push_str(&format!("\x1b[{};{}H{style}", i + 1, col + 1));
    push_squeezed(out, &new[at..], style.is_empty());
    out.push_str("\x1b[0m\x1b[K");
}

/// Spaces in a run this long or longer are written as erase-and-move (ECH, CUF), which is shorter.
const SQUEEZE: usize = 12;

/// Append `s`, turning long runs of unstyled spaces into erase-and-move; unstyled spaces at the very
/// end are dropped, since the row is cleared to its end anyway.
fn push_squeezed(out: &mut String, s: &str, mut plain: bool) {
    let mut spaces = 0;
    let flush = |out: &mut String, n: usize, at_end: bool| {
        if at_end {
            return;
        }
        if n >= SQUEEZE {
            out.push_str(&format!("\x1b[{n}X\x1b[{n}C"));
        } else {
            out.extend(std::iter::repeat_n(' ', n));
        }
    };
    for t in tokens(s) {
        if t == " " && plain {
            spaces += 1;
            continue;
        }
        flush(out, std::mem::take(&mut spaces), false);
        if t.starts_with("\x1b[") {
            plain = t == "\x1b[0m";
        }
        out.push_str(t);
    }
    flush(out, spaces, true);
}

/// Cut a styled line to at most `width` display columns; with `pad`, fill it out to exactly
/// `width` with spaces so whatever follows lines up.
pub fn fit(l: &Line, width: usize, pad: bool) -> Line {
    let mut out: Line = Vec::new();
    let mut w = 0;
    for (text, sty) in l {
        if w >= width {
            break;
        }
        if w + UnicodeWidthStr::width(text.as_str()) <= width {
            w += UnicodeWidthStr::width(text.as_str());
            out.push((text.clone(), *sty));
            continue;
        }
        // Cut between grapheme clusters, each measured as the whole segment was, so an emoji
        // sequence (❤️, a ZWJ family) is kept or dropped whole and columns stay where they are.
        let mut cut = String::new();
        for g in text.graphemes(true) {
            let gw = UnicodeWidthStr::width(g);
            if w + gw > width {
                break;
            }
            cut.push_str(g);
            w += gw;
        }
        out.push((cut, *sty));
        break;
    }
    if pad && w < width {
        out.push((" ".repeat(width - w), Sty::Plain));
    }
    out
}

/// One screen row: the chat line, and when the side panel is open, a divider and the panel line.
pub fn row(chat: Option<&Line>, panel: Option<(Option<&Line>, usize)>, chat_w: usize) -> String {
    let empty = Vec::new();
    match panel {
        None => to_ansi(&fit(chat.unwrap_or(&empty), chat_w, false)),
        Some((p, pw)) => {
            let mut l = fit(chat.unwrap_or(&empty), chat_w, true);
            l.push((" │ ".into(), Sty::Dim));
            l.extend(fit(p.unwrap_or(&empty), pw, false));
            to_ansi(&l)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md::line;

    #[test]
    fn only_changed_rows_are_written_and_nothing_is_cleared() {
        let mut s = Screen::default();
        let first = s.frame(vec!["a".into(), "b".into(), "c".into()], Some((2, 0)));
        assert!(first.contains("\x1b[2J"), "the first frame paints from scratch");
        let second = s.frame(vec!["a".into(), "B".into(), "c".into()], Some((2, 0)));
        assert!(second.contains("\x1b[2;1HB"), "{second:?}");
        assert!(!second.contains("\x1b[1;1Ha") && !second.contains("\x1b[3;1Hc"), "unchanged rows are left alone: {second:?}");
        assert!(!second.contains("\x1b[2J") && !second.contains("\x1b[J"), "no clearing between frames");
        s.invalidate();
        assert!(s.frame(vec!["a".into(), "B".into(), "c".into()], None).contains("\x1b[1;1Ha"));
    }

    /// A tiny terminal: just what frames use (cursor moves, SGR, erase, text), as cells of
    /// (grapheme, style) so a test sees what the user would.
    struct Term {
        cells: Vec<Vec<(String, String)>>,
        row: usize,
        col: usize,
        sgr: String,
    }

    impl Term {
        fn new(w: usize, h: usize) -> Term {
            Term { cells: vec![vec![(" ".into(), String::new()); w]; h], row: 0, col: 0, sgr: String::new() }
        }

        fn feed(&mut self, s: &str) {
            let w = self.cells[0].len();
            for t in tokens(s) {
                let Some(csi) = t.strip_prefix("\x1b[") else {
                    let gw = UnicodeWidthStr::width(t);
                    if self.col + gw <= w {
                        self.cells[self.row][self.col] = (t.to_string(), self.sgr.clone());
                        for c in 1..gw {
                            self.cells[self.row][self.col + c] = (String::new(), self.sgr.clone());
                        }
                    }
                    self.col += gw;
                    continue;
                };
                let (args, f) = csi.split_at(csi.len() - 1);
                let n = |d: usize| args.split(';').next().and_then(|a| a.parse().ok()).unwrap_or(d);
                match f {
                    "H" => {
                        let mut p = args.split(';').map(|a| a.parse::<usize>().unwrap_or(1));
                        self.row = p.next().unwrap_or(1) - 1;
                        self.col = p.next().unwrap_or(1) - 1;
                    }
                    "m" => self.sgr = if args == "0" { String::new() } else { format!("{}{t}", self.sgr) },
                    "X" => (self.col..(self.col + n(1)).min(w)).for_each(|c| self.cells[self.row][c] = (" ".into(), String::new())),
                    "C" => self.col = (self.col + n(1)).min(w - 1),
                    "K" => (self.col.min(w)..w).for_each(|c| self.cells[self.row][c] = (" ".into(), String::new())),
                    "J" => self.cells.iter_mut().flatten().for_each(|c| *c = (" ".into(), String::new())),
                    _ => {} // ?25l/h, ?2026h/l
                }
            }
        }
    }

    #[test]
    fn frames_written_from_the_first_change_show_what_full_rows_would() {
        let rows = |texts: &[&str]| -> Vec<String> {
            texts.iter().map(|t| {
                let mut l: Line = vec![(t.to_string(), Sty::Plain)];
                l.push(("  ".into(), Sty::Plain));
                l.push(("dim".into(), Sty::Dim));
                to_ansi(&fit(&l, 40, true))
            }).collect()
        };
        let frames: Vec<Vec<String>> = vec![
            rows(&["hello world", "", "› ", "a界b"]),
            rows(&["hello world!", "x", "› a", "a界c"]),
            rows(&["hello", "x                y", "› ab", "a界c"]),
            rows(&["❤\u{fe0f} heart", "x                y", "› ab", "界"]),
            rows(&["❤\u{fe0f} hearts", "", "› abc", "界界界"]),
        ];
        let (mut s, mut term) = (Screen::default(), Term::new(40, 4));
        for f in frames {
            term.feed(&s.frame(f.clone(), None));
            let mut fresh = Term::new(40, 4);
            fresh.feed(&Screen::default().frame(f.clone(), None));
            assert_eq!(term.cells, fresh.cells, "after {f:?}");
        }
    }

    #[test]
    fn a_typed_letter_writes_a_few_bytes() {
        let mut s = Screen::default();
        let row = |t: &str| to_ansi(&fit(&vec![("│ › ".into(), Sty::Plain), (t.into(), Sty::Plain), ("".into(), Sty::Plain)], 200, true)) + "│";
        s.frame(vec![row("hello")], None);
        let out = s.frame(vec![row("hello!")], None);
        assert!(out.len() < 60, "{} bytes: {out:?}", out.len());
        assert!(out.contains("\x1b[1;"), "{out:?}");
    }

    #[test]
    fn fit_cuts_and_pads_by_display_width() {
        let l = vec![("ab".to_string(), Sty::Bold), ("界界".to_string(), Sty::Plain)];
        let cut = fit(&l, 5, true);
        let text: String = cut.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "ab界 ", "a wide character that doesn't fit is dropped, then padded");
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 5);
        let row = row(Some(&line("chat", Sty::Plain)), Some((Some(&line("file", Sty::Plain)), 10)), 6);
        assert_eq!(row, "chat  \x1b[2m │ \x1b[0mfile");
    }

    #[test]
    fn fit_keeps_emoji_sequences_whole() {
        let family = "👨\u{200d}👩\u{200d}👧";
        for (text, width) in [("ab❤\u{fe0f}cd".to_string(), 3), (format!("a{family}b"), 2), (format!("a{family}b"), 3)] {
            let cut = fit(&vec![(text.clone(), Sty::Plain)], width, true);
            let out: String = cut.iter().map(|(t, _)| t.as_str()).collect();
            assert_eq!(UnicodeWidthStr::width(out.as_str()), width, "{text:?} at {width}: {out:?}");
            assert!(!out.contains('\u{fe0f}') || out.contains('❤'), "no stray variation selector: {out:?}");
            assert!(out.matches('\u{200d}').count().is_multiple_of(2), "a ZWJ family is kept or dropped whole: {out:?}");
        }
        let side = row(Some(&line(format!("{family}❤\u{fe0f} chat"), Sty::Plain)), Some((Some(&line("file", Sty::Plain)), 4)), 6);
        assert!(side.starts_with(&format!("{family}❤\u{fe0f} c\x1b[2m │")), "two 2-column emoji: the divider stays in column 7: {side:?}");
    }

    #[test]
    fn a_hyperlink_is_one_token_and_its_row_is_written_whole() {
        let link = "\x1b]8;;https://x.example\x1b\\";
        let row = format!("ab {link}url\x1b]8;;\x1b\\ cd");
        let toks: Vec<&str> = tokens(&row).collect();
        assert_eq!(toks, ["a", "b", " ", link, "u", "r", "l", "\x1b]8;;\x1b\\", " ", "c", "d"]);
        let mut s = Screen::default();
        s.frame(vec![row.clone()], None);
        let out = s.frame(vec![row.replace("cd", "ce")], None);
        assert!(out.contains("\x1b[1;1H") && out.contains(link), "repainted from column 1: {out:?}");
    }
}
