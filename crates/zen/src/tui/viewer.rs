//! The file viewer: a file shown in place of a pane's Files tree (enter on a file in Files, a
//! link followed, `/open`), drawn and driven here; `reader.rs` lays the file out. Esc, ← or
//! backspace goes back to the tree at the same row (or to the file a link was followed from).
//! Prose is laid out for reading (`Reading`): `<` `>` narrower or wider lines, `:` line spacing,
//! `b` bionic, `f` focus (off, paragraph, line; ↑↓ move it; bionic stays readable in the dimmed
//! text), `l` `L` choose a link and enter follows it (a click too; ← comes back), `p` one word at
//! a time (`Rsvp`; space pauses, `+` `-` speed, esc stops), `[` `]` the previous or next heading,
//! `i` the properties header, `h` the changelog footer. The header says how far in you are and
//! the minutes left, and a file opens again where it was left. `e` gives a vault note's zenwrite
//! link (`App::open_in_zenwrite`).

use super::*;

impl App {
    /// A key for the file open in `pane`; false when the viewer has no use for it. While it plays
    /// one word at a time the player's keys come first, and while the changelog shows, its own.
    pub(super) fn viewer_key(&mut self, pane: Pane, k: KeyEvent, page: usize) -> bool {
        let playing = self.viewer(pane).as_ref().is_some_and(|p| p.rsvp.is_some());
        if playing && self.player_key(pane, k) {
            return true;
        }
        if k.code == KeyCode::Char('e') {
            self.open_in_zenwrite(pane);
            return true;
        }
        let (focus, wpm, pauses) = (self.reading.focus, self.wpm, self.pauses);
        let Some(p) = self.viewer(pane).as_mut() else { return false };
        if !playing && p.show_log && log_key(p, k.code) {
            return true;
        }
        match k.code {
            KeyCode::Char('i') => p.props_open = !p.props_open,
            KeyCode::Char('h') if !playing => {
                if p.log.is_empty() {
                    self.note("this file has no changelog", Sty::Dim);
                } else {
                    p.show_log = !p.show_log;
                }
            }
            // Focus: ↑↓ go paragraph by paragraph, or line by line.
            KeyCode::Up | KeyCode::Down if focus != Focus::Off && !playing => {
                let by = if k.code == KeyCode::Up { -1 } else { 1 };
                if focus == Focus::Line {
                    p.line_step(by, page);
                } else {
                    p.focus_step(by);
                }
            }
            KeyCode::Up => self.view_scroll(pane, true, 1),
            KeyCode::Down => self.view_scroll(pane, false, 1),
            KeyCode::PageUp => self.view_scroll(pane, true, page),
            KeyCode::PageDown => self.view_scroll(pane, false, page),
            KeyCode::Home => self.view_scroll(pane, true, usize::MAX / 2),
            KeyCode::End => self.view_scroll(pane, false, usize::MAX / 2),
            KeyCode::Char(c @ ('<' | '>')) => {
                self.reading.step(if c == '<' { -1 } else { 1 });
                self.note_reading();
            }
            KeyCode::Char('b') if !playing => {
                self.reading.bionic = !self.reading.bionic;
                self.note_reading();
            }
            KeyCode::Char('f') if !playing => {
                self.reading.focus = self.reading.focus.next();
                self.note_reading();
            }
            KeyCode::Char(':') => {
                self.reading.spaced = !self.reading.spaced;
                self.note_reading();
            }
            KeyCode::Char('p') => match &mut p.rsvp {
                Some(r) if r.playing => r.playing = false,
                Some(r) => r.play(),
                None => {
                    // The whole file, starting at the top of the view, so ↑ can go back past
                    // where it started.
                    let words = p.words_from(0);
                    if let Some(at) = words.iter().position(|(_, r, _)| *r >= p.scroll) {
                        let mut r = Rsvp::new(words, wpm);
                        r.at = at;
                        r.pauses = pauses;
                        r.play();
                        p.rsvp = Some(r);
                    }
                }
            },
            KeyCode::Char(c @ ('[' | ']')) => {
                let at = p.scroll;
                let next = if c == ']' { p.headings.iter().find(|h| **h > at) } else { p.headings.iter().rev().find(|h| **h < at) };
                p.scroll = next.copied().unwrap_or(if c == ']' { at } else { 0 });
            }
            // Links: l and L choose the next or previous, enter follows.
            KeyCode::Char(c @ ('l' | 'L')) => p.choose_link(c == 'l', page),
            KeyCode::Enter => {
                if let Some(i) = p.link_sel {
                    self.follow_link(pane, i);
                }
            }
            // Back to the file a link was followed from, else to the tree.
            KeyCode::Left | KeyCode::Backspace if !p.back.is_empty() => self.go_back(pane),
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace => self.close_viewer(pane),
            _ => return false,
        }
        true
    }

    /// The player's keys, while the file in `pane` is read one word at a time; false for others.
    fn player_key(&mut self, pane: Pane, k: KeyEvent) -> bool {
        if let KeyCode::Char(c @ ('+' | '=' | '-')) = k.code {
            self.wpm = if c == '-' { self.wpm.saturating_sub(50).max(100) } else { (self.wpm + 50).min(1000) };
        }
        let wpm = self.wpm;
        let Some(p) = self.viewer(pane).as_mut() else { return false };
        let Some(r) = p.rsvp.as_mut() else { return false };
        match k.code {
            // ↑↓ pause and step back or on a word.
            KeyCode::Up => {
                r.playing = false;
                r.at = r.at.saturating_sub(1);
            }
            KeyCode::Down => {
                r.playing = false;
                r.at = (r.at + 1).min(r.words.len().saturating_sub(1));
            }
            KeyCode::Char(' ') if r.playing => r.playing = false,
            KeyCode::Char(' ') => r.play(),
            KeyCode::Char('+' | '=' | '-') => r.wpm = wpm,
            // Stop, leaving the view at the word reached.
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace => {
                p.scroll = r.row();
                p.rsvp = None;
            }
            _ => return false,
        }
        true
    }

    /// Say how prose is laid out now, after a key changed it.
    fn note_reading(&mut self) {
        self.note(format!("reading: {}", self.reading.label()), Sty::Dim);
    }

    /// A click on row `row` of the file open in `pane` (0: its name): a link there is followed.
    pub(super) fn viewer_click(&mut self, pane: Pane, x: usize, row: usize) {
            let Some(p) = self.viewer(pane).as_ref() else { return };
            let Some(idx) = row.checked_sub(1).map(|r| p.scroll + r) else { return };
            let Some(l) = p.lines.get(idx) else { return };
            let text: String = l.iter().map(|(t, _)| t.as_str()).collect();
            // The column to a byte of the row's text.
            let mut col = 0;
            let mut at = None;
            for (b, c) in text.char_indices() {
                let cw = UnicodeWidthStr::width(c.encode_utf8(&mut [0; 4]) as &str);
                if x < col + cw.max(1) {
                    at = Some(b);
                    break;
                }
                col += cw;
            }
            let Some(at) = at else { return };
            if let Some(i) = p.links.iter().position(|k| k.spans.iter().any(|(r, a, b)| *r == idx && (*a..*b).contains(&at))) {
                if let Some(p) = self.viewer(pane).as_mut() {
                    p.link_sel = Some(i);
                }
                self.follow_link(pane, i);
            }
    }

    /// Read the reader's settings file when it changed since it was last read: `wpm`,
    /// `pauses` and the layout (`Reading::with`). A bad file is reported and changes nothing.
    pub(super) fn load_reader_settings(&mut self) {
        let Some(path) = self.reader_file.clone() else { return };
        let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else { return };
        if self.reader_read == Some(modified) {
            return;
        }
        self.reader_read = Some(modified);
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string())) {
            Ok(v) => {
                self.reading = self.reading.with(&v);
                self.wpm = v["wpm"].as_u64().map(|w| (w as usize).clamp(100, 1000)).unwrap_or(self.wpm);
                self.pauses = v["pauses"].as_bool().unwrap_or(self.pauses);
            }
            Err(e) => self.note(format!("{}: {e}", path.display()), Sty::Err),
        }
    }

    /// Show a file in `pane`'s Files view, in place of the tree.
    pub(super) fn open_file(&mut self, pane: Pane, path: PathBuf) {
        self.load_reader_settings();
        match FileView::open(path) {
            Ok(mut p) => {
                if pane == Pane::Side {
                    self.warn_if_narrow();
                    self.side = true;
                }
                self.close_viewer(pane);
                p.scroll = self.read_at.get(&p.path).copied().unwrap_or(0);
                *self.viewer(pane) = Some(p);
                self.set_view(pane, View::Files);
            }
            Err(e) => self.note(e, Sty::Err),
        }
    }

    /// A file is being read one word at a time and isn't paused.
    pub(super) fn rsvp_playing(&self) -> bool {
        [&self.side_file, &self.home_file].iter().any(|p| p.as_ref().and_then(|p| p.rsvp.as_ref()).is_some_and(|r| r.playing))
    }

    /// When the next word is due in a playing reader.
    pub(super) fn rsvp_due(&self) -> Option<std::time::Instant> {
        [&self.side_file, &self.home_file].iter().filter_map(|p| p.as_ref()?.rsvp.as_ref().filter(|r| r.playing).map(|r| r.due)).min()
    }

    /// Move each playing reader on; true when a word changed.
    pub(super) fn rsvp_advance(&mut self) -> bool {
        let mut changed = false;
        for p in [&mut self.side_file, &mut self.home_file].into_iter().flatten() {
            if let Some(r) = &mut p.rsvp {
                changed |= r.advance();
            }
        }
        changed
    }

    /// Close the file open in `pane`, remembering where it was left.
    /// Follow link `i` of the file in `pane`: its file opens in its place, and ← comes back.
    pub(super) fn follow_link(&mut self, pane: Pane, i: usize) {
        let Some(p) = self.viewer(pane).as_ref() else { return };
        let Some(link) = p.links.get(i).cloned() else { return };
        let (from, scroll, mut back) = (p.path.clone(), p.scroll, p.back.clone());
        match resolve_link(&link, &from).and_then(FileView::open) {
            Ok(mut next) => {
                self.read_at.insert(from.clone(), scroll);
                back.push((from, scroll));
                next.back = back;
                next.scroll = self.read_at.get(&next.path).copied().unwrap_or(0);
                *self.viewer(pane) = Some(next);
            }
            Err(e) => self.note(e, Sty::Warn),
        }
    }

    /// Back to the file the one in `pane` was reached from by a link, where it was left.
    pub(super) fn go_back(&mut self, pane: Pane) {
        let Some(p) = self.viewer(pane).as_mut() else { return };
        let Some((path, scroll)) = p.back.pop() else { return };
        let back = std::mem::take(&mut p.back);
        let (here, at) = (p.path.clone(), p.scroll);
        match FileView::open(path) {
            Ok(mut prev) => {
                self.read_at.insert(here, at);
                prev.scroll = scroll;
                prev.back = back;
                *self.viewer(pane) = Some(prev);
            }
            Err(e) => {
                if let Some(p) = self.viewer(pane).as_mut() {
                    p.back = back;
                }
                self.note(e, Sty::Err);
            }
        }
    }

    pub(super) fn close_viewer(&mut self, pane: Pane) {
        if let Some(p) = self.viewer(pane).take() {
            self.read_at.insert(p.path, p.scroll);
        }
    }

    /// Reload the open files that changed on disk; true when one did.
    pub(super) fn reload_viewers(&mut self) -> bool {
        let side = self.side_file.as_mut().is_some_and(FileView::reload_if_changed);
        let home = self.home_file.as_mut().is_some_and(FileView::reload_if_changed);
        side || home
    }
}

/// The changelog footer's keys while it shows: ↑↓ and the page keys scroll it, esc hides it.
/// False for other keys.
fn log_key(p: &mut FileView, key: KeyCode) -> bool {
    match key {
        KeyCode::Esc => p.show_log = false,
        KeyCode::Up => p.log_scroll = p.log_scroll.saturating_sub(1),
        KeyCode::Down => p.log_scroll += 1,
        KeyCode::PageUp => p.log_scroll = p.log_scroll.saturating_sub(5),
        KeyCode::PageDown => p.log_scroll += 5,
        KeyCode::Home => p.log_scroll = 0,
        KeyCode::End => p.log_scroll = p.log.len(),
        _ => return false,
    }
    true
}

/// An open file: its name, how far in you are and the minutes left, and its folder; then as much
/// of it as fits, and at its end the reading keys.
pub(super) fn file_lines(p: &mut FileView, w: usize, rows: usize, look: Reading) -> Vec<Line> {
    // The text, a blank row and the keys.
    let n = p.lines(w, look).len() + 2;
    // The header pinned under the name; the changelog, when shown, a footer with its own scroll.
    let header = p.header_lines(w, p.props_open);
    let mut footer: Vec<Line> = Vec::new();
    if p.rsvp.is_none() && p.show_log && !p.log.is_empty() {
        let fh = p.log.len().min((rows / 3).max(4));
        p.log_scroll = p.log_scroll.min(p.log.len() - fh);
        let at = if p.log.len() > fh { format!(" · {}–{} of {}", p.log_scroll + 1, p.log_scroll + fh, p.log.len()) } else { String::new() };
        footer.push(line(format!("──── changelog{at} · ↑↓ scroll · h hide"), Sty::Dim));
        footer.extend(p.log.iter().skip(p.log_scroll).take(fh).cloned());
    }
    let shown = rows.saturating_sub(1 + header.len() + footer.len()).max(1);
    let text_rows = if p.rsvp.is_some() { shown.saturating_sub(5).max(1) } else { shown };
    if let Some(r) = &p.rsvp {
        // Playing: the text follows the word.
        let row = r.row();
        if row < p.scroll || row >= p.scroll + text_rows {
            p.scroll = row.saturating_sub(text_rows / 3);
        }
    }
    p.scroll = p.scroll.min(n.saturating_sub(text_rows));
    let s = p.scroll;
    let end = (s + text_rows).min(n);
    let pos = if n > text_rows {
        let left = p.minutes_left(end);
        format!(" · {}%{}", end * 100 / n, if left > 0 { format!(" · {left} min left") } else { String::new() })
    } else {
        String::new()
    };
    // The name first: a narrow pane cuts the folder, not the name.
    let name = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.path.display().to_string());
    let dir = p.path.parent().map(|d| format!("  {}", d.display())).unwrap_or_default();
    let mut out = vec![vec![(name, Sty::Bold), (pos, Sty::Dim), (dir, Sty::Dim)]];
    out.extend(header);
    let extra = format!("{}{}", if p.props.is_empty() { "" } else { " · i header" }, if p.log.is_empty() { "" } else { " · h changelog" });
    let keys = line(format!("  < > width · : spacing · b bionic · f focus · p play · [ ] headings · l links, enter follow{extra} · ← back · esc files"), Sty::Dim);
    // Only the rows in view are copied, however far into a long file.
    let blank = Vec::new();
    let mut body: Vec<Line> = p.lines.iter().chain([&blank, &keys]).skip(s).take(text_rows).cloned().collect();
    if let Some(r) = &p.rsvp {
        // The whole text dimmed but for the word playing, in bold; the player below.
        let sh = DIM;
        let (row, nth) = r.words.get(r.at).map(|(_, row, i)| (*row, *i)).unwrap_or((usize::MAX, 0));
        for (i, l) in body.iter_mut().enumerate() {
            *l = if s + i == row { mark_word(l, nth, sh) } else { dim(l, sh) };
        }
        body.resize(text_rows, Vec::new());
        out.extend(body);
        out.extend(rsvp_lines(r, w));
        return out;
    }
    let view = s.min(p.lines.len())..(s + text_rows).min(p.lines.len());
    let sh = DIM;
    match look.focus {
        Focus::Off => {}
        Focus::Paragraph => {
            // One paragraph lit, the others dimmed. The lit one stays while it's in view;
            // otherwise the first in view takes over.
            if !p.focus_para.is_some_and(|f| p.para[view.clone()].contains(&f)) {
                p.focus_para = view.clone().find(|r| p.focusable(*r)).map(|r| p.para[r]);
            }
            if let Some(lit) = p.focus_para {
                for (i, l) in body.iter_mut().enumerate() {
                    if p.para.get(s + i) != Some(&lit) {
                        *l = dim(l, sh);
                    }
                }
            }
        }
        Focus::Line => {
            // A reading ruler: the line as it is with a mark in the margin, the rest of its
            // paragraph two levels less dim, the others dimmed.
            let near = (sh + 2).min(4);
            if !p.focus_line.is_some_and(|r| view.contains(&r)) {
                p.focus_line = view.clone().find(|r| p.focusable(*r));
            }
            if let Some(lit) = p.focus_line {
                let para = p.para.get(lit).copied();
                for (i, l) in body.iter_mut().enumerate() {
                    if s + i == lit {
                        *l = ruler(l);
                    } else if p.para.get(s + i).copied() != para {
                        *l = dim(l, sh);
                    } else {
                        *l = dim(l, near);
                    }
                }
            }
        }
    }
    // Links in green, the chosen one in yellow; a dimmed link stays dimmed.
    for (i, k) in p.links.iter().enumerate() {
        for (r, a, b) in &k.spans {
            if let Some(l) = r.checked_sub(s).and_then(|r| body.get_mut(r)) {
                let chosen = p.link_sel == Some(i);
                *l = restyle(l, *a, *b, |st| match st {
                    _ if chosen => Sty::Warn,
                    Sty::Shade(_) | Sty::ShadeBold(_) => st,
                    _ => Sty::Accent,
                });
            }
        }
    }
    if !footer.is_empty() {
        body.resize(text_rows, Vec::new());
    }
    out.extend(body);
    out.extend(footer);
    out
}

/// A row with bytes `a..b` of its text restyled by `f`, the rest as it is.
fn restyle(l: &Line, a: usize, b: usize, f: impl Fn(Sty) -> Sty) -> Line {
    let mut out: Line = Vec::new();
    let mut at = 0;
    for (t, s) in l {
        let (start, end) = (at, at + t.len());
        at = end;
        let (x, y) = (a.clamp(start, end), b.clamp(start, end));
        for (u, v, sty) in [(start, x, *s), (x, y, f(*s)), (y, end, *s)] {
            if u < v && t.is_char_boundary(u - start) && t.is_char_boundary(v - start) {
                out.push((t[u - start..v - start].to_string(), sty));
            }
        }
    }
    out
}

/// The line a reading ruler is on: as it is (bionic stays), with `›` in the margin when there's
/// room.
fn ruler(l: &Line) -> Line {
    let mut out: Line = l.clone();
    if let Some((pad, _)) = out.first_mut() {
        if pad.len() >= 2 && pad.trim().is_empty() {
            pad.truncate(pad.len() - 2);
            out.insert(1, ("› ".into(), Sty::Accent));
        }
    }
    out
}

/// A row in grey level `sh` (0-4), keeping its weight: bold (a bionic word head, a heading)
/// becomes a bold grey a level stronger, so bionic still reads in dimmed text.
pub(super) fn dim(l: &Line, sh: u8) -> Line {
    l.iter().map(|(t, s)| (t.clone(), dim_sty(*s, sh))).collect()
}

fn dim_sty(s: Sty, sh: u8) -> Sty {
    match s {
        Sty::Bold | Sty::ShadeBold(_) | Sty::Heading(_) => Sty::ShadeBold((sh + 1).min(4)),
        _ => Sty::Shade(sh),
    }
}

/// A row dimmed (bionic kept) but for its `nth` word, in bold: where one word at a time is.
pub(super) fn mark_word(l: &Line, nth: usize, sh: u8) -> Line {
    let text: String = l.iter().map(|(t, _)| t.as_str()).collect();
    let mut starts = Vec::new();
    let mut prev_space = true;
    for (i, c) in text.char_indices() {
        if !c.is_whitespace() && prev_space {
            starts.push(i);
        }
        prev_space = c.is_whitespace();
    }
    let Some(&a) = starts.get(nth) else { return dim(l, sh) };
    let b = text[a..].find(char::is_whitespace).map(|e| a + e).unwrap_or(text.len());
    // Walk the segments, cutting them where the word starts and ends.
    let mut out: Line = Vec::new();
    let mut at = 0;
    for (t, s) in l {
        let (start, end) = (at, at + t.len());
        at = end;
        for (x, y) in [(start, a.clamp(start, end)), (a.clamp(start, end), b.clamp(start, end)), (b.clamp(start, end), end)] {
            if x < y {
                let sty = if x >= a && y <= b { Sty::Bold } else { dim_sty(*s, sh) };
                out.push((text[x..y].to_string(), sty));
            }
        }
    }
    out
}

/// The player: the word on a fixed mark, its recognition letter in red, two words either side
/// dimmed; then the speed, where it is and the keys.
fn rsvp_lines(r: &Rsvp, w: usize) -> Vec<Line> {
    let word = r.words.get(r.at).map(|(w, _, _)| w.as_str()).unwrap_or("");
    let k = orp(word);
    let col = (w / 2).saturating_sub(2).max(k);
    let head: String = word.chars().take(k).collect();
    let mid: String = word.chars().skip(k).take(1).collect();
    let tail: String = word.chars().skip(k + 1).collect();
    let before: Vec<&str> = r.words[r.at.saturating_sub(2)..r.at.min(r.words.len())].iter().map(|(w, _, _)| w.as_str()).collect();
    let after: Vec<&str> = r.words.iter().skip(r.at + 1).take(2).map(|(w, _, _)| w.as_str()).collect();
    let mut ctx = before.join(" ");
    if !ctx.is_empty() {
        ctx.push(' ');
    }
    // Left of the mark: the words before, cut from the left when they don't fit.
    let room = col.saturating_sub(UnicodeWidthStr::width(head.as_str()));
    let cw = UnicodeWidthStr::width(ctx.as_str());
    let ctx = if cw > room { ctx.chars().skip(cw - room).collect() } else { format!("{}{ctx}", " ".repeat(room - cw)) };
    let right = if after.is_empty() { String::new() } else { format!(" {}", after.join(" ")) };
    let mark = |m: &str| line(format!("{}{m}", " ".repeat(col)), Sty::Dim);
    let state = if r.playing { "playing" } else if r.at + 1 >= r.words.len() { "the end" } else { "paused" };
    vec![
        mark("▼"),
        vec![(ctx, Sty::Dim), (head, Sty::Bold), (mid, Sty::Err), (tail, Sty::Bold), (right, Sty::Dim)],
        mark("▲"),
        Vec::new(),
        line(format!("  {} wpm · {state} · word {} of {} · space pause · ↑↓ word · + - speed · esc stop", r.wpm, r.at + 1, r.words.len()), Sty::Dim),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    #[test]
    fn prose_is_laid_out_for_reading_and_code_is_not() {
        let dir = TempDir::new("reading");
        let para = "word ".repeat(200);
        let md = dir.file("a.md", &format!("---\ntype: source\n---\n# One\n{para}\n\n\n\n## Two\n```\n{}\n```\n", "x".repeat(150)));
        let mut p = FileView::open(md).unwrap();
        let rows = texts(p.lines(120, Reading::default())).to_vec();
        // Front matter is the header, not the text; prose wraps at 72 columns, centered in 120.
        assert!(rows[0].contains("One") && p.props == [("type".to_string(), "source".to_string())]);
        let prose: Vec<&String> = rows.iter().filter(|r| r.contains("word")).collect();
        assert!(prose.iter().all(|r| r.starts_with(&" ".repeat(24)) && width(r.trim_start()) <= 72), "{prose:?}");
        // One blank row between paragraphs however many the file has, and one before a heading.
        assert!(!rows.windows(2).any(|w| w[0].is_empty() && w[1].is_empty()), "{rows:?}");
        assert_eq!(p.headings.len(), 2);
        assert!(rows[p.headings[0]].contains("One") && p.headings[0] == 0);
        assert!(rows[p.headings[1]].contains("Two"));
        assert!(p.words_from(0)[0].0 == "One", "front matter isn't read word by word");
        // Spaced: a blank row after each prose row, but code stays dense.
        let spaced = texts(p.lines(120, Reading { measure: 1, spaced: true, ..Reading::default() })).to_vec();
        let i = spaced.iter().position(|r| r.contains("word")).unwrap();
        assert!(spaced[i + 1].is_empty() && spaced.len() > rows.len() + prose.len() - 2);
        // Full width uses the whole pane; a code file is never narrowed.
        assert!(texts(p.lines(120, Reading { measure: 3, ..Reading::default() })).iter().any(|r| width(r) > 110));
        let rs = dir.file("a.rs", &"let x = 1; ".repeat(20));
        assert!(width(&texts(FileView::open(rs).unwrap().lines(150, Reading::default()))[0]) > 100);
        // Minutes left at 238 words a minute: about 200 words, all left, is 1 minute; none at the end.
        assert_eq!((p.minutes_left(0), p.minutes_left(rows.len())), (1, 0));
    }

    #[test]
    fn properties_are_a_pinned_header_and_the_changelog_a_footer_with_its_own_scroll() {
        let dir = TempDir::new("sections");
        let log: String = (1..=30).map(|i| format!("- 2026-10-{i:02} — change {i}\n")).collect();
        let text = format!("---\ntype: source\naliases: [moat, edge]\ntags:\n  - a\n  - b\nproject: \"[[Projects/x]]\"\ndue:\n---\n# Title\n{}\n\n## Changelog\n{log}", "Body words here. ".repeat(100));
        let mut p = FileView::open(dir.file("p.md", &text)).unwrap();
        let look = Reading { bionic: true, spaced: true, ..Reading::default() };
        p.lines(80, look);
        // Properties: aligned keys, lists joined, quotes off, empty values left out.
        let open = texts(&p.header_lines(80, true)).iter().map(|r| r.trim().to_string()).collect::<Vec<_>>();
        assert_eq!(open, ["type     source", "aliases  moat · edge", "tags     a · b", "project  [[Projects/x]]"]);
        let folded = texts(&p.header_lines(80, false)).to_vec();
        assert!(folded.len() == 1 && folded[0].contains("type source") && folded[0].contains("aliases moat · edge"), "{folded:?}");
        // The text is the title to the changelog: nothing else is read, lit or bionic.
        assert!(texts(&p.lines).iter().all(|r| !r.contains("change") && !r.contains("source")));
        assert_eq!(p.words_from(0)[0].0, "Title");
        assert!(p.words_from(0).iter().all(|w| !w.0.contains("Changelog")));
        assert_eq!(p.log.len(), 30);
        // Pinned header under the name; the text scrolls without moving it.
        p.scroll = 10;
        let rows = texts(&file_lines(&mut p, 80, 24, look)).to_vec();
        assert!(rows[1].contains("type source") && !rows.iter().any(|r| r.contains("change 1")), "{rows:?}");
        p.props_open = true;
        assert!(texts(&file_lines(&mut p, 80, 24, look))[4].contains("project"));
        // h: the changelog at the bottom, scrolled on its own; the text stays where it was.
        p.show_log = true;
        let rows = texts(&file_lines(&mut p, 80, 24, look)).to_vec();
        assert_eq!(rows.len(), 24);
        let at = rows.iter().position(|r| r.contains("──── changelog")).unwrap();
        assert!(rows[at + 1].ends_with("change 1"), "{rows:?}");
        p.log_scroll = 5;
        let rows2 = texts(&file_lines(&mut p, 80, 24, look)).to_vec();
        assert!(rows2[at + 1].ends_with("change 6") && rows2[..at] == rows[..at] && p.scroll == 10, "{rows2:?}");
        p.log_scroll = 1000;
        let rows3 = texts(&file_lines(&mut p, 80, 24, look)).to_vec();
        assert!(rows3.last().unwrap().contains("change 30"), "scrolls only to its end: {rows3:?}");
    }

    #[tokio::test]
    async fn the_viewer_keys_change_the_layout_jump_headings_and_remember_the_place() {
        let dir = TempDir::new("reading-keys");
        let body: String = (1..=4).map(|i| format!("## Part {i}\n{}\n\n", "lorem ipsum dolor ".repeat(60))).collect();
        dir.file("a.md", &body);
        let mut a = board_app(120, 24);
        a.files = Some(Files::new(dir.path().to_path_buf()));
        key(&mut a, KeyCode::Tab).await;
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("% · ") && text.contains("min left"), "progress in the header: {text}");
        // ] goes to the next heading, [ back.
        key(&mut a, KeyCode::Char(']')).await;
        let p = a.home_file.as_ref().unwrap();
        assert_eq!(p.scroll, p.headings[1]);
        key(&mut a, KeyCode::Char('[')).await;
        assert_eq!(a.home_file.as_ref().unwrap().scroll, 0);
        // < > change the width, : the spacing.
        key(&mut a, KeyCode::Char('<')).await;
        key(&mut a, KeyCode::Char(':')).await;
        assert_eq!(a.reading, Reading { measure: 0, spaced: true, ..Reading::default() });
        key(&mut a, KeyCode::Char('>')).await;
        key(&mut a, KeyCode::Char('>')).await;
        key(&mut a, KeyCode::Char('>')).await;
        key(&mut a, KeyCode::Char('>')).await;
        assert_eq!(a.reading.measure, MEASURES.len() - 1, "stops at full width");
        // Closed and opened again, the file is where it was left; its end shows the keys.
        key(&mut a, KeyCode::End).await;
        a.draw();
        assert!(plain_rows(&a).join("\n").contains("[ ] headings"));
        let at = a.home_file.as_ref().unwrap().scroll;
        assert!(at > 0);
        key(&mut a, KeyCode::Esc).await;
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.home_file.as_ref().unwrap().scroll, at);
    }

    #[test]
    fn bionic_bolds_word_heads_and_one_word_at_a_time_marks_the_recognition_letter() {
        let dir = TempDir::new("bionic");
        let mut p = FileView::open(dir.file("a.md", "Reading this, quickly.\n\n`code` stays\n")).unwrap();
        let look = Reading { measure: 3, bionic: true, ..Reading::default() };
        let l = p.lines(40, look)[0].clone();
        let bold: Vec<&str> = l.iter().filter(|(_, s)| *s == Sty::Bold).map(|(t, _)| t.as_str()).collect();
        assert_eq!(bold, ["Read", "th", "quic"], "{l:?}");
        assert_eq!(texts(&[l])[0], "Reading this, quickly.", "the text is unchanged");
        assert!(p.lines(40, look)[2].iter().any(|(t, s)| t == "code" && *s == Sty::Code), "code isn't bolded");
        assert_eq!([orp("a"), orp("word"), orp("reading"), orp("comprehension"), orp("internationalization")], [0, 1, 2, 3, 4]);
        // The pause is longer at the end of a sentence; it moves on only when the word's time is up.
        let ms = std::time::Duration::from_millis;
        let mut r = Rsvp::new(vec![("one,".into(), 0, 0), ("two.".into(), 1, 0), ("three".into(), 2, 0)], 600);
        assert!(r.dwell() == ms(100) && !r.advance(), "every word the same time by default");
        r.pauses = true;
        assert_eq!(r.dwell(), ms(150));
        // Each deadline follows the last one: woken 30 ms late, the next word is still due on the
        // beat (220 ms after the deadline, not after the wake-up).
        let due = std::time::Instant::now() - ms(30);
        r.due = due;
        assert!(r.advance() && r.at == 1 && r.row() == 1 && r.dwell() == ms(220));
        assert_eq!(r.due, due + ms(220));
        // Far behind, it starts again from now instead of rushing through the words missed.
        r.due = std::time::Instant::now() - ms(5000);
        assert!(r.advance() && r.at == 2 && r.due > std::time::Instant::now());
        assert!(!r.advance(), "one word per deadline");
        r.due -= std::time::Duration::from_secs(1);
        assert!(r.advance() && !r.playing && r.at == 2, "stops at the last word");
    }

    #[tokio::test]
    async fn h_shows_the_changelog_and_the_arrows_scroll_it_not_the_text() {
        let dir = TempDir::new("log-keys");
        let log: String = (1..=40).map(|i| format!("- change {i}\n")).collect();
        dir.file("a.md", &format!("---\ntype: x\n---\n{}\n## Changelog\n{log}", "line of text\n\n".repeat(60)));
        let mut a = board_app(100, 24);
        a.files = Some(Files::new(dir.path().to_path_buf()));
        key(&mut a, KeyCode::Tab).await;
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        key(&mut a, KeyCode::Down).await;
        let p = a.home_file.as_ref().unwrap();
        assert!(p.scroll == 1 && !p.show_log);
        key(&mut a, KeyCode::Char('h')).await;
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Down).await;
        a.draw();
        let p = a.home_file.as_ref().unwrap();
        assert!(p.show_log && p.log_scroll == 2 && p.scroll == 1, "the arrows move the changelog: {} {}", p.log_scroll, p.scroll);
        assert!(plain_rows(&a).join("\n").contains("changelog · 3–"));
        key(&mut a, KeyCode::Esc).await;
        let p = a.home_file.as_ref().unwrap();
        assert!(!p.show_log, "esc hides the changelog, the file stays open");
        key(&mut a, KeyCode::Down).await;
        assert_eq!(a.home_file.as_ref().unwrap().scroll, 2);
    }

    #[tokio::test]
    async fn focus_dims_other_paragraphs_and_one_word_at_a_time_plays_and_stops_where_it_got() {
        let dir = TempDir::new("focus");
        dir.file("a.md", "---\ntype: x\n---\nfirst paragraph here\n\nsecond paragraph there\n\nthird one\n");
        let mut a = board_app(100, 24);
        a.files = Some(Files::new(dir.path().to_path_buf()));
        key(&mut a, KeyCode::Tab).await;
        key(&mut a, KeyCode::Enter).await;
        key(&mut a, KeyCode::Char('f')).await;
        let lit = |a: &mut App, t: &str| {
            let p = a.home_file.as_mut().unwrap();
            let rows = file_lines(p, 100, 20, a.reading);
            let row = rows.iter().find(|l| texts(&[(*l).clone()])[0].contains(t)).unwrap().clone();
            !row.iter().all(|(_, s)| matches!(s, Sty::Shade(_) | Sty::ShadeBold(_)))
        };
        assert!(lit(&mut a, "first") && !lit(&mut a, "second") && !lit(&mut a, "third"));
        // ↓ and ↑ move the light paragraph by paragraph, even when the whole file fits.
        key(&mut a, KeyCode::Down).await;
        assert!(!lit(&mut a, "first") && lit(&mut a, "second"));
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Down).await;
        assert!(lit(&mut a, "third"), "stops at the last");
        key(&mut a, KeyCode::Up).await;
        assert!(lit(&mut a, "second"));
        // f again: line focus, a ruler on one line, moved line by line.
        key(&mut a, KeyCode::Char('f')).await;
        assert_eq!(a.reading.focus, Focus::Line);
        let lit_rows = |a: &mut App| {
            let p = a.home_file.as_mut().unwrap();
            let rows = file_lines(p, 100, 20, a.reading);
            rows.iter().skip(1).filter(|l| l.iter().any(|(t, s)| *s == Sty::Accent && t.starts_with('›'))).map(|l| texts(std::slice::from_ref(l))[0].trim().to_string()).collect::<Vec<_>>()
        };
        key(&mut a, KeyCode::Down).await;
        let before = lit_rows(&mut a);
        key(&mut a, KeyCode::Down).await;
        let after = lit_rows(&mut a);
        assert!(before.len() == 1 && after.len() == 1 && before != after, "{before:?} {after:?}");
        assert!(after[0].starts_with("› "), "a mark in the margin: {after:?}");
        key(&mut a, KeyCode::Char('f')).await;
        assert_eq!(a.reading.focus, Focus::Off);
        key(&mut a, KeyCode::Char('f')).await;
        key(&mut a, KeyCode::Home).await;
        // p plays from the top of the view; esc stops and leaves the view at the word reached.
        key(&mut a, KeyCode::Char('p')).await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("200 wpm · playing · word 1 of 8") && text.contains("▼"), "{text}");
        // The player shows the two words after; the text above marks the word playing in bold.
        assert!(text.contains("first paragraph here"), "{text}");
        let p = a.home_file.as_mut().unwrap();
        p.rsvp.as_mut().unwrap().at = 4;
        let rows = file_lines(p, 100, 20, Reading::default());
        let player = texts(&rows[rows.len() - 4..rows.len() - 3])[0].clone();
        assert!(player.contains("here second paragraph there third"), "{player}");
        assert!(rows.iter().flatten().any(|(t, s)| t == "paragraph" && *s == Sty::Bold));
        assert!(rows.iter().flatten().filter(|(_, s)| *s == Sty::Bold).count() >= 1);
        p.rsvp.as_mut().unwrap().at = 0;
        key(&mut a, KeyCode::Char('+')).await;
        key(&mut a, KeyCode::Char(' ')).await;
        // ↓ ↑ step a word on and back, paused.
        key(&mut a, KeyCode::Char(' ')).await;
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Up).await;
        let r = a.home_file.as_mut().unwrap().rsvp.as_mut().unwrap();
        assert!(r.wpm == 250 && !r.playing && r.at == 1, "{}", r.at);
        r.at = 0;
        key(&mut a, KeyCode::Up).await;
        let r = a.home_file.as_mut().unwrap().rsvp.as_mut().unwrap();
        assert_eq!(r.at, 0, "stops at the first word");
        r.at = 3;
        key(&mut a, KeyCode::Esc).await;
        let p = a.home_file.as_ref().unwrap();
        assert!(p.rsvp.is_none() && p.scroll == 2, "on the row of word 4, still in the file");
        assert_eq!(a.wpm, 250, "the speed is kept");
    }

    #[tokio::test]
    async fn settings_come_from_the_reader_file() {
        let dir = TempDir::new("reader-settings");
        dir.file("a.md", "- first point here\n- second point\n\nnext paragraph\n");
        let mut a = board_app(100, 24);
        a.files = Some(Files::new(dir.path().to_path_buf()));
        assert_eq!(a.wpm, 200, "200 wpm by default");
        // The file is read when a file opens; unknown or missing keys keep what's there.
        let cfg = dir.file("reader.json", r#"{"wpm": 260, "width": 90, "focus": "paragraph", "pauses": true}"#);
        a.reader_file = Some(cfg);
        key(&mut a, KeyCode::Tab).await;
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Enter).await;
        assert!(a.home_file.is_some());
        assert_eq!((a.wpm, a.pauses, a.reading.measure, a.reading.focus), (260, true, 2, Focus::Paragraph));
        // Rows out of focus are grey.
        let p = a.home_file.as_mut().unwrap();
        let rows = file_lines(p, 100, 20, a.reading);
        assert!(rows.iter().flatten().any(|(t, s)| t.contains("next") && *s == Sty::Shade(DIM)), "{rows:?}");
        // List bullets aren't words to play.
        let words: Vec<String> = a.home_file.as_ref().unwrap().words_from(0).into_iter().map(|w| w.0).collect();
        assert_eq!(words[..3], ["first", "point", "here"]);
        assert_eq!(md::to_ansi(&vec![("x".into(), Sty::Shade(0))]), "\x1b[38;5;238mx\x1b[0m");
    }

    #[test]
    fn bionic_survives_focus_and_play() {
        let dir = TempDir::new("bionic-focus");
        let mut p = FileView::open(dir.file("a.md", "Reading this quickly\n\nAnother paragraph follows\n")).unwrap();
        let look = Reading { bionic: true, focus: Focus::Paragraph, ..Reading::default() };
        let rows = file_lines(&mut p, 80, 20, look);
        let find = |t: &str| rows.iter().find(|l| texts(std::slice::from_ref(*l))[0].contains(t)).unwrap().clone();
        // Lit: bold heads at full strength. Dimmed: heads a bold grey, tails a softer grey.
        assert!(find("Reading").iter().any(|(t, s)| t == "Read" && *s == Sty::Bold));
        let other = find("Another");
        assert!(other.iter().any(|(t, s)| t == "Anot" && *s == Sty::ShadeBold(2)), "{other:?}");
        assert!(other.iter().any(|(t, s)| t == "her" && *s == Sty::Shade(1)), "{other:?}");
        // Line focus: the rest of the lit paragraph and other paragraphs keep bionic too.
        let l = dim(&other, 3);
        assert!(l.iter().any(|(t, s)| t == "Anot" && *s == Sty::ShadeBold(4)));
        // Play: the word playing is bold; the rest of its row keeps bionic, dimmed.
        let m = mark_word(&find("Reading"), 1, 1);
        assert_eq!(texts(std::slice::from_ref(&m))[0], texts(std::slice::from_ref(&find("Reading")))[0]);
        assert!(m.iter().any(|(t, s)| t == "this" && *s == Sty::Bold) || (m.iter().any(|(t, s)| t == "th" && *s == Sty::Bold) && m.iter().any(|(t, s)| t == "is" && *s == Sty::Bold)), "{m:?}");
        assert!(m.iter().any(|(t, s)| t == "Read" && *s == Sty::ShadeBold(2)), "{m:?}");
    }

    #[tokio::test]
    async fn links_are_found_followed_by_key_or_click_and_back_returns() {
        let dir = TempDir::new("links");
        std::fs::create_dir_all(dir.join(".obsidian")).unwrap();
        std::fs::create_dir_all(dir.join("wiki/deep")).unwrap();
        let filler = "word ".repeat(12);
        dir.file("wiki/a.md", &format!("Start {filler} see [[Economic moat|moats]] and [the bee page in the deep folder](deep/b%20page.md) or [web](https://x.y).\n"));
        dir.file("wiki/deep/Economic Moat.md", "# Moat\n\nmoat body\n");
        dir.file("wiki/deep/b page.md", "bee body\n");
        // Shown by their text, found whole even when a wrap splits them.
        let mut p = FileView::open(dir.join("wiki/a.md")).unwrap();
        p.lines(40, Reading { measure: 3, ..Reading::default() });
        let targets: Vec<&str> = p.links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["Economic moat", "deep/b%20page.md", "https://x.y"]);
        let text = texts(&p.lines).join(" ");
        assert!(text.contains("see moats and the bee page") && !text.contains('['), "{text}");
        p.lines(24, Reading { measure: 3, ..Reading::default() });
        assert!(p.links.iter().any(|l| l.spans.len() == 2), "one wraps: {:?}", p.links);
        p.lines(40, Reading { measure: 3, ..Reading::default() });
        // Wiki links by name anywhere in the vault (any case); paths from the file's folder.
        let from = dir.join("wiki/a.md");
        assert_eq!(resolve_link(&p.links[0], &from).unwrap(), dir.join("wiki/deep/Economic Moat.md"));
        assert_eq!(resolve_link(&p.links[1], &from).unwrap(), dir.join("wiki/deep/b page.md"));
        assert!(resolve_link(&p.links[2], &from).unwrap_err().contains("web link"));
        // l chooses, enter follows, ← comes back where it was; then ← goes to the tree.
        let mut a = board_app(120, 24);
        a.files = Some(Files::new(dir.join("wiki")));
        key(&mut a, KeyCode::Tab).await;
        key(&mut a, KeyCode::End).await; // a.md, below the folder
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        key(&mut a, KeyCode::Char('l')).await;
        assert_eq!(a.home_file.as_ref().unwrap().link_sel, Some(0));
        key(&mut a, KeyCode::Enter).await;
        assert!(a.home_file.as_ref().unwrap().path.ends_with("Economic Moat.md"));
        assert_eq!(a.home_file.as_ref().unwrap().back.len(), 1);
        key(&mut a, KeyCode::Left).await;
        let p = a.home_file.as_ref().unwrap();
        assert!(p.path.ends_with("a.md") && p.back.is_empty());
        // A click on a link follows it.
        a.draw();
        let rows = plain_rows(&a);
        let (y, row) = rows.iter().enumerate().find(|(_, r)| r.contains("the bee")).unwrap();
        let x = width(&row[..row.find("the bee").unwrap()]) + 1;
        a.click_board(x, y).await.unwrap();
        assert!(a.home_file.as_ref().unwrap().path.ends_with("b page.md"), "{row}");
        key(&mut a, KeyCode::Backspace).await;
        key(&mut a, KeyCode::Backspace).await;
        assert!(a.home_file.is_none(), "back past the first file is the tree");
    }
}
