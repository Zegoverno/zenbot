//! A small multi-line input editor with prompt history.

use unicode_width::UnicodeWidthChar;

use crate::md::{Line, Sty};

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
        let history = history_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.lines().filter(|l| !l.is_empty()).map(|l| l.replace("\\n", "\n")).collect())
            .unwrap_or_default();
        Editor { buf: String::new(), cursor: 0, history, hist_idx: None, draft: String::new(), history_file }
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn set(&mut self, s: &str) {
        self.buf = s.to_string();
        self.cursor = self.buf.len();
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

    pub fn delete_word(&mut self) {
        let before = &self.buf[..self.cursor];
        let trimmed = before.trim_end_matches(' ');
        let start = trimmed.rfind([' ', '\n']).map(|i| i + 1).unwrap_or(0);
        self.buf.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Move up a line; at the first line, recall older history. Returns true if handled.
    pub fn up(&mut self) {
        let start = self.line_start();
        if start > 0 {
            let col = self.cursor - start;
            let prev_start = self.buf[..start - 1].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let prev_len = start - 1 - prev_start;
            self.cursor = prev_start + col.min(prev_len);
            while !self.buf.is_char_boundary(self.cursor) {
                self.cursor -= 1;
            }
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
            let col = self.cursor - self.line_start();
            let next_start = end + 1;
            let next_end = self.buf[next_start..].find('\n').map(|i| next_start + i).unwrap_or(self.buf.len());
            self.cursor = next_start + col.min(next_end - next_start);
            while !self.buf.is_char_boundary(self.cursor) {
                self.cursor -= 1;
            }
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
            if let Some(p) = &self.history_file {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                    let _ = writeln!(f, "{}", text.replace('\n', "\\n"));
                }
            }
        }
        text
    }

    /// Render with a "› " prompt, hard-wrapped to `width`. Returns lines and the caret (row, col).
    pub fn render(&self, width: usize, placeholder: &str) -> (Vec<Line>, usize, usize) {
        let width = width.max(12);
        let content = width - 2;
        if self.buf.is_empty() {
            return (vec![vec![("› ".into(), Sty::Accent), (placeholder.into(), Sty::Dim)]], 0, 2);
        }
        let mut lines: Vec<Line> = Vec::new();
        let (mut caret_row, mut caret_col) = (0, 2);
        let mut offset = 0;
        for (li, logical) in self.buf.split('\n').enumerate() {
            let prefix = if li == 0 { ("› ".to_string(), Sty::Accent) } else { ("  ".to_string(), Sty::Plain) };
            let mut cur = String::new();
            let mut w = 0;
            let mut first_chunk = true;
            let start_row = lines.len();
            let mut row_in_line = 0;
            let mut found_caret = false;
            for (bi, ch) in logical.char_indices() {
                let cw = ch.width().unwrap_or(0);
                if w + cw > content {
                    lines.push(vec![if first_chunk { prefix.clone() } else { ("  ".into(), Sty::Plain) }, (std::mem::take(&mut cur), Sty::Plain)]);
                    first_chunk = false;
                    row_in_line += 1;
                    w = 0;
                }
                if offset + bi == self.cursor {
                    caret_row = start_row + row_in_line;
                    caret_col = 2 + w;
                    found_caret = true;
                }
                cur.push(ch);
                w += cw;
            }
            if !found_caret && offset + logical.len() == self.cursor {
                caret_row = start_row + row_in_line;
                caret_col = 2 + w;
            }
            lines.push(vec![if first_chunk { prefix } else { ("  ".into(), Sty::Plain) }, (cur, Sty::Plain)]);
            offset += logical.len() + 1;
        }
        (lines, caret_row, caret_col.min(width - 1))
    }
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
}
