//! A small multi-line input editor with prompt history.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::md::{Line, Sty};

/// Most prompts kept in the history file.
const HISTORY_MAX: usize = 1000;

/// Read the prompt history: one JSON string per line. A file in the old format (one prompt per
/// line, newlines written as a literal `\n`, so a typed `\n` came back as a newline) is read
/// once and rewritten in the new one. Only the last `HISTORY_MAX` prompts are kept, and the
/// file is made private to the owner.
fn load_history(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let parsed: Option<Vec<String>> = lines.iter().map(|l| serde_json::from_str::<String>(l).ok()).collect();
    let (mut history, old) = match parsed {
        Some(h) => (h, false),
        None => (lines.iter().map(|l| l.replace("\\n", "\n")).collect::<Vec<_>>(), true),
    };
    let cut = history.len().saturating_sub(HISTORY_MAX);
    history.drain(..cut);
    if old || cut > 0 {
        rewrite_history(path, &history);
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    history
}

fn rewrite_history(path: &Path, history: &[String]) {
    let body: String = history.iter().map(|h| serde_json::to_string(h).unwrap_or_default() + "\n").collect();
    let tmp = path.with_extension("tmp");
    if private_file(&tmp, false).and_then(|mut f| f.write_all(body.as_bytes())).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Open a file only the owner can read (mode 0600 when it is created): truncated, or appended to.
fn private_file(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).mode(0o600).open(path)
}

pub struct Editor {
    pub buf: String,
    cursor: usize,
    history: Vec<String>,
    hist_idx: Option<usize>,
    draft: String,
    history_file: Option<std::path::PathBuf>,
}

impl Editor {
    pub fn new(history_file: Option<std::path::PathBuf>) -> Self {
        let history = history_file.as_deref().map(load_history).unwrap_or_default();
        Editor { buf: String::new(), cursor: 0, history, hist_idx: None, draft: String::new(), history_file }
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn set(&mut self, s: &str) {
        self.buf = s.to_string();
        self.cursor = self.buf.len();
    }

    #[cfg(test)]
    pub fn set_history(&mut self, entries: &[&str]) {
        self.history = entries.iter().map(|e| e.to_string()).collect();
    }

    /// True while the buffer shows an unedited prompt recalled from history.
    pub fn browsing_history(&self) -> bool {
        self.hist_idx.is_some_and(|i| self.history.get(i) == Some(&self.buf))
    }

    pub fn clear(&mut self) {
        self.set("");
        self.hist_idx = None;
    }

    pub fn insert(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        self.buf.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    fn prev_boundary(&self) -> usize {
        self.buf[..self.cursor].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_boundary(&self) -> usize {
        self.buf[self.cursor..].chars().next().map(|c| self.cursor + c.len_utf8()).unwrap_or(self.cursor)
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let p = self.prev_boundary();
            self.buf.replace_range(p..self.cursor, "");
            self.cursor = p;
        }
    }

    pub fn delete(&mut self) {
        let n = self.next_boundary();
        self.buf.replace_range(self.cursor..n, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.prev_boundary();
    }

    pub fn right(&mut self) {
        self.cursor = self.next_boundary();
    }

    fn line_start(&self) -> usize {
        self.buf[..self.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self) -> usize {
        self.buf[self.cursor..].find('\n').map(|i| self.cursor + i).unwrap_or(self.buf.len())
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    pub fn kill_to_start(&mut self) {
        let s = self.line_start();
        self.buf.replace_range(s..self.cursor, "");
        self.cursor = s;
    }

    pub fn kill_to_end(&mut self) {
        let e = self.line_end();
        self.buf.replace_range(self.cursor..e, "");
    }

    /// Move to the start of the previous word. Words are separated by ASCII whitespace
    /// (single bytes, so the index arithmetic stays on char boundaries).
    pub fn word_left(&mut self) {
        let before = self.buf[..self.cursor].trim_end_matches(|c: char| c.is_ascii_whitespace());
        self.cursor = before.rfind(|c: char| c.is_ascii_whitespace()).map(|i| i + 1).unwrap_or(0);
    }

    /// Move past the end of the next word.
    pub fn word_right(&mut self) {
        let rest = &self.buf[self.cursor..];
        let skip = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_whitespace()).len();
        let word = rest[skip..].find(|c: char| c.is_ascii_whitespace()).unwrap_or(rest.len() - skip);
        self.cursor += skip + word;
    }

    /// Delete back to the start of the previous word (the same words `word_left` moves by).
    pub fn delete_word(&mut self) {
        let end = self.cursor;
        self.word_left();
        self.buf.replace_range(self.cursor..end, "");
    }

    /// Display column of the caret in its line.
    fn column(&self) -> usize {
        UnicodeWidthStr::width(&self.buf[self.line_start()..self.cursor])
    }

    /// The byte offset in the line `start..end` closest to display column `col`, never past it.
    fn at_column(&self, start: usize, end: usize, col: usize) -> usize {
        let mut w = 0;
        for (i, ch) in self.buf[start..end].char_indices() {
            w += ch.width().unwrap_or(0);
            if w > col {
                return start + i;
            }
        }
        end
    }

    /// Move up a line, keeping the display column; at the first line, recall older history.
    pub fn up(&mut self) {
        let start = self.line_start();
        if start > 0 {
            let prev_start = self.buf[..start - 1].rfind('\n').map(|i| i + 1).unwrap_or(0);
            self.cursor = self.at_column(prev_start, start - 1, self.column());
            return;
        }
        if self.history.is_empty() {
            return;
        }
        let idx = match self.hist_idx {
            None => {
                self.draft = self.buf.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.hist_idx = Some(idx);
        let h = self.history[idx].clone();
        self.set(&h);
    }

    pub fn down(&mut self) {
        let end = self.line_end();
        if end < self.buf.len() {
            let next_start = end + 1;
            let next_end = self.buf[next_start..].find('\n').map(|i| next_start + i).unwrap_or(self.buf.len());
            self.cursor = self.at_column(next_start, next_end, self.column());
            return;
        }
        match self.hist_idx {
            None => {}
            Some(i) if i + 1 < self.history.len() => {
                self.hist_idx = Some(i + 1);
                let h = self.history[i + 1].clone();
                self.set(&h);
            }
            Some(_) => {
                self.hist_idx = None;
                let d = std::mem::take(&mut self.draft);
                self.set(&d);
            }
        }
    }

    /// Take the buffer as a submitted prompt and record it in history.
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.buf);
        self.cursor = 0;
        self.hist_idx = None;
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
            let excess = self.history.len().saturating_sub(HISTORY_MAX);
            if excess > 0 {
                self.history.drain(..excess);
            }
            if let Some(p) = &self.history_file {
                if excess > 0 {
                    rewrite_history(p, &self.history);
                } else if let (Ok(mut f), Ok(json)) = (private_file(p, true), serde_json::to_string(&text)) {
                    let _ = writeln!(f, "{json}");
                }
            }
        }
        text
    }

    /// Where the caret sits in a logical line's wrapped rows: (row, col).
    fn caret_in(ranges: &[(usize, usize)], logical: &str, c: usize) -> (usize, usize) {
        for (ri, &(s, e)) in ranges.iter().enumerate() {
            if c < e || ri + 1 == ranges.len() {
                return (ri, UnicodeWidthStr::width(&logical[s..c.max(s)]));
            }
        }
        (0, 0)
    }

    /// Render as a bordered box `width` columns wide, word-wrapped, showing at most `max_rows`
    /// rows of text (scrolled to keep the caret visible). Returns lines and the caret (row, col).
    pub fn render(&self, width: usize, placeholder: &str, hint: &str, max_rows: usize) -> (Vec<Line>, usize, usize) {
        let width = width.max(16);
        let inner = width - 6; // "│ › " … " │"
        let mut rows: Vec<(String, String, usize)> = Vec::new(); // prefix, text, text width
        let (mut crow, mut ccol) = (0, 0);
        if self.buf.is_empty() {
            let p: String = placeholder.chars().take(inner).collect();
            let w = UnicodeWidthStr::width(p.as_str());
            rows.push(("› ".into(), p, w));
        } else {
            let mut offset = 0;
            for logical in self.buf.split('\n') {
                let ranges = wrap_ranges(logical, inner);
                if self.cursor >= offset && self.cursor <= offset + logical.len() {
                    let (r, c) = Self::caret_in(&ranges, logical, self.cursor - offset);
                    crow = rows.len() + r;
                    ccol = c;
                }
                for (s, e) in ranges {
                    let prefix = if rows.is_empty() { "› " } else { "  " };
                    let mut text = logical[s..e].to_string();
                    if UnicodeWidthStr::width(text.as_str()) > inner {
                        text.pop(); // the hanging space
                    }
                    let w = UnicodeWidthStr::width(text.as_str());
                    rows.push((prefix.into(), text, w));
                }
                offset += logical.len() + 1;
            }
        }

        let max_rows = max_rows.max(1);
        let start = (crow + 1).saturating_sub(max_rows).min(rows.len().saturating_sub(max_rows));
        let below = rows.len().saturating_sub(start + max_rows);
        let border = |left: &str, right: &str, label: String| -> Line {
            let label: String = label.chars().take(width.saturating_sub(6)).collect();
            let fill = width - 2 - UnicodeWidthStr::width(label.as_str());
            if label.is_empty() {
                vec![(format!("{left}{}{right}", "─".repeat(width - 2)), Sty::Dim)]
            } else {
                vec![(format!("{left}{}", "─".repeat(fill - 1)), Sty::Dim), (label, Sty::Dim), (format!("─{right}"), Sty::Dim)]
            }
        };
        let mut lines = vec![border("╭", "╮", if start > 0 { format!(" ↑ {start} more ") } else { String::new() })];
        for (prefix, text, w) in rows.iter().skip(start).take(max_rows) {
            let sty = if self.buf.is_empty() { Sty::Dim } else { Sty::Plain };
            let psty = if prefix.starts_with('›') { Sty::Accent } else { Sty::Plain };
            lines.push(vec![
                ("│ ".into(), Sty::Dim),
                (prefix.clone(), psty),
                (text.clone(), sty),
                (" ".repeat(inner.saturating_sub(*w)), Sty::Plain),
                (" │".into(), Sty::Dim),
            ]);
        }
        let bottom = if below > 0 { format!(" ↓ {below} more ") } else if hint.is_empty() { String::new() } else { format!(" {hint} ") };
        lines.push(border("╰", "╯", bottom));
        (lines, 1 + crow - start, (4 + ccol).min(width - 2))
    }
}

/// Word-wrap one line into byte ranges of at most `width` columns, breaking after a space where possible.
fn wrap_ranges(s: &str, width: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut start, mut w, mut after_space) = (0, 0, None::<usize>);
    for (i, ch) in s.char_indices() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > width && i > start && ch == ' ' {
            // A space that overflows hangs at the end of the row (not drawn) instead of wrapping.
            out.push((start, i + 1));
            (start, w, after_space) = (i + 1, 0, None);
            continue;
        }
        if w + cw > width && i > start {
            let brk = after_space.filter(|&b| b > start && b <= i).unwrap_or(i);
            out.push((start, brk));
            start = brk;
            w = UnicodeWidthStr::width(&s[start..i]);
            after_space = None;
        }
        w += cw;
        if ch == ' ' {
            after_space = Some(i + 1);
        }
    }
    out.push((start, s.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsing_history_until_edited() {
        let mut e = Editor::new(None);
        e.history = vec!["hello".into(), "/status".into(), "last".into()];
        assert!(!e.browsing_history());
        e.up();
        e.up();
        assert_eq!(e.buf, "/status");
        assert!(e.browsing_history());
        e.up();
        assert_eq!(e.buf, "hello");
        e.down();
        e.set("/stat");
        assert!(!e.browsing_history());
    }

    fn text(l: &Line) -> String {
        l.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn wraps_at_words() {
        assert_eq!(wrap_ranges("hello brave new world", 11), vec![(0, 12), (12, 21)]);
        assert_eq!(wrap_ranges("hello brave new world", 10), vec![(0, 6), (6, 16), (16, 21)]);
        assert_eq!(wrap_ranges("abcdefghij", 4), vec![(0, 4), (4, 8), (8, 10)]);
        assert_eq!(wrap_ranges("", 4), vec![(0, 0)]);
    }

    #[test]
    fn renders_a_box_with_paragraphs_and_caret() {
        let mut e = Editor::new(None);
        e.insert("first paragraph\n\nsecond");
        let (lines, row, col) = e.render(30, "", "", 10);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t.len(), 5);
        assert!(t[0].starts_with('╭') && t[4].starts_with('╰'));
        assert!(t[1].starts_with("│ › first paragraph") && t[1].ends_with(" │"));
        assert_eq!(t[2].trim_end_matches(" │").trim(), "│");
        assert!(t.iter().all(|l| UnicodeWidthStr::width(l.as_str()) == 30));
        assert_eq!((row, col), (3, 4 + 6));
    }

    #[test]
    fn long_input_scrolls_to_the_caret() {
        let mut e = Editor::new(None);
        e.insert("1\n2\n3\n4\n5\n6");
        let (lines, row, _) = e.render(30, "", "", 3);
        assert_eq!(lines.len(), 5);
        assert!(text(&lines[0]).contains("↑ 3 more"));
        assert!(text(&lines[3]).contains('6'));
        assert_eq!(row, 3);
        e.up();
        e.up();
        e.up();
        e.up();
        e.up();
        let (lines, row, _) = e.render(30, "", "", 3);
        assert!(text(&lines[1]).contains('1'));
        assert!(text(&lines[4]).contains("↓ 3 more"));
        assert_eq!(row, 1);
    }

    #[test]
    fn up_and_down_keep_the_display_column() {
        let mut e = Editor::new(None);
        e.insert("日本語テキスト\nabcdefgh\nxy");
        e.up(); // from the end of "xy" (column 2) to column 2 of "abcdefgh"
        assert_eq!(&e.buf[e.line_start()..e.cursor], "ab");
        e.right();
        e.right(); // column 4
        e.up();
        assert_eq!(&e.buf[e.line_start()..e.cursor], "日本", "two wide characters are four columns");
        e.right(); // column 6
        e.down();
        assert_eq!(&e.buf[e.line_start()..e.cursor], "abcdef");
        e.down();
        assert_eq!(&e.buf[e.line_start()..e.cursor], "xy", "clamped to a shorter line");
    }

    #[test]
    fn delete_word_uses_the_same_words_as_word_left() {
        let mut e = Editor::new(None);
        e.insert("one\ttwo  ");
        e.delete_word();
        assert_eq!(e.buf, "one    ", "a tab is inserted as spaces, and the word before the spaces goes");
        e.set("first line\nsecond\n");
        e.delete_word();
        assert_eq!(e.buf, "first line\n", "the newline before the caret goes with the word");
    }

    #[test]
    fn history_keeps_prompts_exactly_and_reads_the_old_format_once() {
        let dir = crate::tui::test_util::TempDir::new("history");
        let path = dir.file("history", "first\\nsecond line\nplain\n");
        let mut e = Editor::new(Some(path.clone()));
        assert_eq!(e.history, ["first\nsecond line", "plain"], "old format: \\n was a newline");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\"first\\nsecond line\"\n\"plain\"\n", "rewritten as JSON lines");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        e.set("println!(\"a\\nb\");\nnext line");
        e.take();
        let e = Editor::new(Some(path.clone()));
        assert_eq!(e.history.last().unwrap(), "println!(\"a\\nb\");\nnext line", "a typed \\n stays a backslash and an n");
        // Only the last 1000 are kept.
        let many: String = (0..1200).map(|i| format!("\"p{i}\"\n")).collect();
        std::fs::write(&path, many).unwrap();
        let e = Editor::new(Some(path.clone()));
        assert_eq!((e.history.len(), e.history[0].as_str()), (1000, "p200"));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1000);
        // A new file is created private.
        let fresh = dir.join("fresh");
        let mut e = Editor::new(Some(fresh.clone()));
        e.set("hi");
        e.take();
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o600);
        for i in 0..HISTORY_MAX {
            e.set(&format!("next-{i}"));
            e.take();
        }
        assert_eq!(e.history.len(), HISTORY_MAX);
        assert_eq!(std::fs::read_to_string(&fresh).unwrap().lines().count(), HISTORY_MAX);
        assert_eq!(e.history.first().unwrap(), "next-0");
    }

    #[test]
    fn word_motion() {
        let mut e = Editor::new(None);
        e.insert("one two  three");
        e.word_left();
        assert_eq!(e.cursor, 9);
        e.word_left();
        assert_eq!(e.cursor, 4);
        e.word_right();
        assert_eq!(e.cursor, 7);
        e.kill_to_end();
        assert_eq!(e.buf, "one two");
        e.set("a\tb");
        e.word_left();
        assert_eq!(e.cursor, 2);
    }
}
