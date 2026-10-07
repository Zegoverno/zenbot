//! Full-screen frames without flicker. Every frame is built whole (one string per row), then only
//! the rows that differ from the last frame are written, each overwritten in place. Nothing is
//! cleared first, so the screen never shows a blank moment, even on terminals that ignore
//! synchronized output (DEC mode 2026, which we still send).

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
            if self.prev.get(i) != Some(row) {
                out.push_str(&format!("\x1b[{};1H{row}\x1b[0m\x1b[K", i + 1));
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
        let mut cut = String::new();
        for ch in text.chars() {
            let cw = ch.width().unwrap_or(0);
            if w + cw > width {
                break;
            }
            cut.push(ch);
            w += cw;
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
}
