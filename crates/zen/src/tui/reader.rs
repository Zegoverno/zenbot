//! The file viewer's model: a file read from disk and laid out for reading (`FileView`), the
//! layout settings (`Reading`: line width, spacing, bionic, focus) and one word at a time
//! (`Rsvp`). `viewer.rs` draws it and handles its keys; either pane shows it in place of its
//! Files tree.

use super::*;
use unicode_width::UnicodeWidthStr;

/// Largest file the viewer loads.
pub(super) const VIEW_MAX_BYTES: u64 = 2 << 20;

/// How prose is laid out in the file viewer, in the spirit of Reader's appearance settings
/// (line width, line spacing) within what a terminal can do (the font and its size are the
/// terminal's). Prose (markdown, text) wraps at `MEASURES[measure]` columns, centered in the pane:
/// 50-75 characters a line is the classic readable measure; a wider pane doesn't make longer
/// lines. `spaced` puts a blank row after each prose row, the terminal's line height. Code and
/// other files keep the whole width.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Reading {
    pub(super) measure: usize,
    pub(super) spaced: bool,
    /// Bionic: the first half of each word in bold (`b`).
    pub(super) bionic: bool,
    /// Focus (`f` goes round): off, one paragraph lit, or one line lit (a reading ruler).
    pub(super) focus: Focus,
}

/// The grey (`Sty::Shade`) text out of focus is drawn in; bold text there is a level stronger, so
/// bionic still reads in it.
pub(super) const DIM: u8 = 1;

/// What focus lights; ↑↓ move it.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(super) enum Focus {
    #[default]
    Off,
    /// One paragraph lit, the others dimmed.
    Paragraph,
    /// One line in bold with a mark in the margin, the rest of its paragraph as it is, the
    /// others dimmed.
    Line,
}

impl Focus {
    pub(super) fn next(self) -> Focus {
        match self {
            Focus::Off => Focus::Paragraph,
            Focus::Paragraph => Focus::Line,
            Focus::Line => Focus::Off,
        }
    }
}

/// Line widths `<` and `>` go through; 0 is the whole pane.
pub(super) const MEASURES: [usize; 4] = [60, 72, 90, 0];

/// Silent reading speed for non-fiction, words a minute (Brysbaert 2019, a meta-analysis of 190
/// studies): what the minutes left are estimated from.
const WPM: usize = 238;

impl Default for Reading {
    fn default() -> Reading {
        Reading { measure: 1, spaced: false, bionic: false, focus: Focus::Off }
    }
}

impl Reading {
    /// The prose width in a pane `w` columns wide.
    fn width(self, w: usize) -> usize {
        match MEASURES[self.measure.min(MEASURES.len() - 1)] {
            0 => w,
            m => m.min(w),
        }
    }

    /// One step narrower (`by` -1) or wider (1), stopping at the ends.
    pub(super) fn step(&mut self, by: isize) {
        self.measure = (self.measure as isize + by).clamp(0, MEASURES.len() as isize - 1) as usize;
    }

    pub(super) fn label(self) -> String {
        let w = match MEASURES[self.measure.min(MEASURES.len() - 1)] {
            0 => "full width".to_string(),
            m => format!("{m} columns"),
        };
        let on = [(self.spaced, " · spaced"), (self.bionic, " · bionic"), (self.focus == Focus::Paragraph, " · paragraph focus"), (self.focus == Focus::Line, " · line focus")];
        format!("{w}{}", on.iter().filter(|(b, _)| *b).map(|(_, t)| *t).collect::<String>())
    }

    /// Settings from the reader file, over `self`: `width` (60, 72, 90 or 0 for the pane),
    /// `spaced`, `bionic`, `focus` ("off", "paragraph", "line").
    pub(super) fn with(mut self, v: &Value) -> Reading {
        if let Some(w) = v["width"].as_u64() {
            self.measure = MEASURES.iter().position(|m| *m as u64 == w).unwrap_or(self.measure);
        }
        self.spaced = v["spaced"].as_bool().unwrap_or(self.spaced);
        self.bionic = v["bionic"].as_bool().unwrap_or(self.bionic);
        self.focus = match v["focus"].as_str() {
            Some("paragraph") => Focus::Paragraph,
            Some("line") => Focus::Line,
            Some("off") => Focus::Off,
            _ => self.focus,
        };
        self
    }
}

/// One word at a time (RSVP, as Spritz does it), with the text kept in sight: a player at the
/// bottom shows each word in the same place, its recognition letter (a little left of center) in
/// red under a fixed mark so the eyes needn't move, two words before and after it dimmed; above,
/// the whole text dimmed with the word playing in bold, scrolling along, so the place is never
/// lost. `p` starts it from the top of the view, space pauses, ↑↓ pause and step a word, `+` `-` change the speed, esc
/// stops and leaves the view at the word reached.
pub(super) struct Rsvp {
    /// Each word, the row it's on and which word of the row it is.
    pub(super) words: Vec<(String, usize, usize)>,
    pub(super) at: usize,
    pub(super) wpm: usize,
    pub(super) playing: bool,
    /// Longer stays at punctuation and long words (Spritz's rhythm); off, every word the same.
    pub(super) pauses: bool,
    /// When the next word goes up. Each deadline follows the last one, not the moment the word
    /// was drawn, so late wake-ups don't add up into an uneven pace.
    pub(super) due: std::time::Instant,
}

impl Rsvp {
    pub(super) fn new(words: Vec<(String, usize, usize)>, wpm: usize) -> Rsvp {
        let mut r = Rsvp { words, at: 0, wpm, playing: false, pauses: false, due: std::time::Instant::now() };
        r.play();
        r
    }

    /// Play (again) from the word showing, which gets its full time.
    pub(super) fn play(&mut self) {
        self.playing = true;
        self.due = std::time::Instant::now() + self.dwell();
    }

    /// How long the current word stays: longer at the end of a sentence or clause and for long
    /// words, as Spritz does, so the rhythm follows the sense.
    pub(super) fn dwell(&self) -> std::time::Duration {
        let base = 60_000.0 / self.wpm.max(1) as f64;
        let w = self.words.get(self.at).map(|(w, _, _)| w.as_str()).unwrap_or("");
        let mut f = 1.0;
        if !self.pauses {
            return std::time::Duration::from_millis(base as u64);
        }
        if w.ends_with(['.', '!', '?', ':']) {
            f = 2.2;
        } else if w.ends_with([',', ';', ')']) {
            f = 1.5;
        }
        if w.chars().count() > 8 {
            f += 0.3;
        }
        std::time::Duration::from_millis((base * f) as u64)
    }

    /// Move on when the word has been up long enough; true when the word changed.
    pub(super) fn advance(&mut self) -> bool {
        let now = std::time::Instant::now();
        if !self.playing || now < self.due {
            return false;
        }
        if self.at + 1 >= self.words.len() {
            self.playing = false;
            return true;
        }
        self.at += 1;
        // On the beat; but far behind (the terminal stalled), start again from now rather than
        // flash the words missed.
        let next = self.due + self.dwell();
        self.due = if next < now { now + self.dwell() } else { next };
        true
    }

    /// The row the word reached is on.
    pub(super) fn row(&self) -> usize {
        self.words.get(self.at).map(|(_, r, _)| *r).unwrap_or(0)
    }
}

/// The letter a word is recognized by (Spritz's optimal recognition point): the first of 1, the
/// second of 2-5 letters, the third of 6-9, the fourth of 10-13, else the fifth.
pub(super) fn orp(word: &str) -> usize {
    match word.chars().count() {
        0 | 1 => 0,
        2..=5 => 1,
        6..=9 => 2,
        10..=13 => 3,
        _ => 4,
    }
}

/// Bionic: the first half of every word in plain prose made bold (one letter of a word up to 3),
/// the rest a shade softer, so it shows even in a terminal font without a bold face.
fn bionic(l: Line) -> Line {
    let mut out: Line = Vec::new();
    for (text, sty) in l {
        if sty != Sty::Plain {
            out.push((text, sty));
            continue;
        }
        for (i, word) in text.split(' ').enumerate() {
            if i > 0 {
                out.push((" ".into(), Sty::Plain));
            }
            let n = word.chars().filter(|c| c.is_alphanumeric()).count();
            let head = if n <= 3 { 1 } else { n.div_ceil(2) };
            let cut = word.char_indices().filter(|(_, c)| c.is_alphanumeric()).nth(head).map(|(i, _)| i).unwrap_or(word.len());
            if n > 0 {
                out.push((word[..cut].to_string(), Sty::Bold));
            }
            out.push((word[if n > 0 { cut } else { 0 }..].to_string(), if n > 0 { Sty::Shade(3) } else { Sty::Plain }));
        }
    }
    out.retain(|(t, _)| !t.is_empty());
    out
}

/// A file open in a viewer (either pane): its text as read, and laid out at the pane's width.
#[derive(Default)]
pub(super) struct FileView {
    pub(super) path: PathBuf,
    pub(super) text: String,
    pub(super) modified: Option<SystemTime>,
    /// First line shown.
    pub(super) scroll: usize,
    /// Rendered lines, and the width they were rendered at.
    pub(super) lines: Vec<Line>,
    pub(super) lines_w: usize,
    /// The layout the lines were rendered with.
    pub(super) look: Reading,
    /// Rows where a heading starts, for `[` and `]`.
    pub(super) headings: Vec<usize>,
    /// Each row's paragraph, for focus (`usize::MAX`: none).
    pub(super) para: Vec<usize>,
    /// The words from each row to the end, for the minutes left (counted once per layout, not
    /// on every frame).
    pub(super) words_left: Vec<usize>,
    /// The paragraph focus lights, once chosen (↑↓ move it in focus mode).
    pub(super) focus_para: Option<usize>,
    /// The row line focus lights, once chosen.
    pub(super) focus_line: Option<usize>,
    /// The front matter's properties, for the pinned header.
    pub(super) props: Vec<(String, String)>,
    /// The header open, one row per property (`i`); else folded to one row.
    pub(super) props_open: bool,
    /// The links the reader showed by their text: `Sty::Link(i)` is `link_table[i]`.
    pub(super) link_table: Vec<(String, bool)>,
    /// The changelog, rendered: the footer `h` shows below the text.
    pub(super) log: Vec<Line>,
    /// The footer shown, with the keys: ↑↓ and the wheel scroll it, not the text.
    pub(super) show_log: bool,
    /// The footer's own scroll: its first row shown.
    pub(super) log_scroll: usize,
    /// One word at a time, when playing.
    pub(super) rsvp: Option<Rsvp>,
    /// The links in the rendered lines (`[[wiki link]]`, `[text](path)`), in order.
    pub(super) links: Vec<Link>,
    /// The link `l` / `L` chose, which enter follows.
    pub(super) link_sel: Option<usize>,
    /// The files followed here from, with where each was left: ← and backspace go back.
    pub(super) back: Vec<(PathBuf, usize)>,
}

impl FileView {
    pub(super) fn open(path: PathBuf) -> std::result::Result<FileView, String> {
        FileView::open_unless(path, &files::default_secrets())
    }

    /// Open `path` unless it holds secrets (`files::secret_paths`): the panel shows files in clear.
    pub(super) fn open_unless(path: PathBuf, secrets: &[PathBuf]) -> std::result::Result<FileView, String> {
        let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if files::is_secret(&path, secrets) || files::is_secret(&real, secrets) {
            return Err(format!("{} holds secrets; zen doesn't show it", path.display()));
        }
        let mut p = FileView { path, ..Default::default() };
        p.load()?;
        Ok(p)
    }

    pub(super) fn load(&mut self) -> std::result::Result<(), String> {
        let meta = std::fs::metadata(&self.path).map_err(|e| format!("can't open {}: {e}", self.path.display()))?;
        if !meta.is_file() {
            return Err(format!("{} is not a file", self.path.display()));
        }
        if meta.len() > VIEW_MAX_BYTES {
            return Err(format!("{} is too big to show ({} MB)", self.path.display(), meta.len() >> 20));
        }
        let bytes = std::fs::read(&self.path).map_err(|e| format!("can't read {}: {e}", self.path.display()))?;
        self.text = md::sanitize(&String::from_utf8_lossy(&bytes)).into_owned();
        self.modified = meta.modified().ok();
        self.lines_w = 0;
        Ok(())
    }

    /// Reload the file if it changed on disk; true when it did.
    pub(super) fn reload_if_changed(&mut self) -> bool {
        let now = std::fs::metadata(&self.path).ok().and_then(|m| m.modified().ok());
        if now.is_some() && now != self.modified {
            return self.load().is_ok();
        }
        false
    }

    /// Prose: markdown or plain text, laid out for reading. Anything else is shown as it is.
    fn is_prose(&self) -> bool {
        self.path.extension().is_some_and(|e| ["md", "markdown", "txt"].iter().any(|x| e.eq_ignore_ascii_case(x)))
    }

    /// The file's text laid out in a pane `width` columns wide as `look` says; laid out again
    /// only when either changed.
    pub(super) fn lines(&mut self, width: usize, look: Reading) -> &[Line] {
        if self.lines_w != width || self.look != look {
            let is_md = self.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
            let mut l = if self.is_prose() { lay_out_prose(&self.text, is_md, width, look) } else { lay_out_plain(&self.text, width) };
            l.para.resize(l.lines.len(), usize::MAX);
            self.links = find_links_in(&l.lines, &l.link_table);
            self.words_left = words_left(&l.lines);
            (self.lines, self.para, self.headings, self.props, self.log, self.link_table) = (l.lines, l.para, l.headings, l.props, l.log, l.link_table);
            self.link_sel = self.link_sel.filter(|i| *i < self.links.len());
            self.log_scroll = self.log_scroll.min(self.log.len().saturating_sub(1));
            self.lines_w = width;
            self.look = look;
        }
        &self.lines
    }

    /// The pinned header: the front matter's properties, folded to one row (`key value`, side
    /// by side, cut at the width) or, `open`, one row per property with the keys aligned. Empty
    /// when the file has none.
    pub(super) fn header_lines(&self, width: usize, open: bool) -> Vec<Line> {
        if self.props.is_empty() {
            return Vec::new();
        }
        if !open {
            let mut l: Line = Vec::new();
            let mut used = 0;
            for (k, v) in &self.props {
                let item = format!("{k} {v}   ");
                let w = UnicodeWidthStr::width(item.as_str());
                if used + w > width.saturating_sub(4) && used > 0 {
                    l.push(("…".to_string(), Sty::Dim));
                    break;
                }
                l.push((format!("{k} "), Sty::Dim));
                l.push((format!("{v}   "), Sty::Plain));
                used += w;
            }
            let mut l = md::wrap(l, width, (" ".to_string(), Sty::Plain), (" ".to_string(), Sty::Plain));
            l.truncate(1);
            return l;
        }
        let kw = self.props.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0) + 2;
        let mut out = Vec::new();
        for (k, v) in &self.props {
            out.extend(md::wrap(vec![(v.clone(), Sty::Plain)], width, (format!(" {k:<kw$}"), Sty::Dim), (" ".repeat(kw + 1), Sty::Plain)));
        }
        out
    }

    /// The words from row `from` on, each with its row and place in the row, for one word at a
    /// time.
    pub(super) fn words_from(&self, from: usize) -> Vec<(String, usize, usize)> {
        let mut out = Vec::new();
        for (r, l) in self.lines.iter().enumerate().skip(from) {
            let text: String = l.iter().map(|(t, _)| t.as_str()).collect();
            // Marks with no letter or digit (bullets, dashes, rules) aren't words to read.
            out.extend(text.split_whitespace().enumerate().filter(|(_, w)| w.chars().any(char::is_alphanumeric)).map(|(i, w)| (w.to_string(), r, i)));
        }
        out
    }

    /// The first row of each paragraph of prose, in order.
    pub(super) fn paragraphs(&self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (r, para) in self.para.iter().enumerate() {
            let blank = self.lines.get(r).is_none_or(|l| l.is_empty());
            if *para != usize::MAX && !blank && out.last().is_none_or(|(p, _)| p != para) {
                out.push((*para, r));
            }
        }
        out
    }

    /// Focus on the paragraph before (`by` -1) or after (1) the lit one, bringing it into view.
    pub(super) fn focus_step(&mut self, by: isize) {
        let paras = self.paragraphs();
        let Some(i) = self.focus_para.and_then(|f| paras.iter().position(|(p, _)| *p == f)) else { return };
        let j = (i as isize + by).clamp(0, paras.len() as isize - 1) as usize;
        self.focus_para = Some(paras[j].0);
        self.scroll = paras[j].1.saturating_sub(2);
    }

    /// A row of prose focus can light: past the front matter, not blank.
    pub(super) fn focusable(&self, r: usize) -> bool {
        self.lines.get(r).is_some_and(|l| !l.is_empty()) && self.para.get(r).is_some_and(|p| *p != usize::MAX)
    }

    /// Light the line before (`by` -1) or after (1) the lit one, keeping it `rows` rows in view
    /// with a little of what's around it.
    pub(super) fn line_step(&mut self, by: isize, rows: usize) {
        let Some(at) = self.focus_line else { return };
        let next = if by < 0 { (0..at).rev().find(|r| self.focusable(*r)) } else { (at + 1..self.lines.len()).find(|r| self.focusable(*r)) };
        let Some(r) = next else { return };
        self.focus_line = Some(r);
        if r < self.scroll + 2 {
            self.scroll = r.saturating_sub(2);
        } else if r + 3 > self.scroll + rows {
            self.scroll = r + 3 - rows;
        }
    }

    /// Choose the next link (`forward`) or the previous one: from the chosen link, else from
    /// what's in view (`rows` rows), and bring it into view.
    pub(super) fn choose_link(&mut self, forward: bool, rows: usize) {
        let n = self.links.len();
        if n == 0 {
            return;
        }
        let i = match self.link_sel {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None if forward => self.links.iter().position(|l| l.spans[0].0 >= self.scroll).unwrap_or(0),
            None => self.links.iter().rposition(|l| l.spans[0].0 < self.scroll + rows).unwrap_or(n - 1),
        };
        self.link_sel = Some(i);
        let row = self.links[i].spans[0].0;
        if row < self.scroll || row + 2 > self.scroll + rows {
            self.scroll = row.saturating_sub(rows / 3);
        }
    }

    /// The minutes it takes to read the prose from row `at` on (front and back matter aside).
    pub(super) fn minutes_left(&self, at: usize) -> usize {
        self.words_left.get(at).copied().unwrap_or(0).div_ceil(WPM)
    }
}

/// A file's text laid out at a width: the rows and what's known about them.
#[derive(Default)]
struct Layout {
    lines: Vec<Line>,
    /// Each row's paragraph.
    para: Vec<usize>,
    headings: Vec<usize>,
    props: Vec<(String, String)>,
    log: Vec<Line>,
    link_table: Vec<(String, bool)>,
    /// The paragraph rows pushed now belong to.
    at: usize,
}

impl Layout {
    fn push(&mut self, l: Line) {
        self.lines.push(l);
        self.para.push(self.at);
    }

    /// End the paragraph: one blank row after it, however many the file has.
    fn end_para(&mut self) {
        if self.lines.last().is_some_and(|l| !l.is_empty()) {
            self.push(Vec::new());
        }
        self.at += 1;
    }
}

/// Text that isn't prose (code, data): every line wrapped at the pane's width, as it is.
fn lay_out_plain(text: &str, width: usize) -> Layout {
    let none = || (String::new(), Sty::Plain);
    let lines = text.split('\n').flat_map(|l| md::wrap(vec![(l.to_string(), Sty::Plain)], width, none(), none())).collect();
    Layout { lines, ..Layout::default() }
}

/// Prose laid out for reading in a pane `width` columns wide, as `look` says: wrapped at the
/// reading width and centered, one blank row between paragraphs and before each heading,
/// markdown rendered. A markdown file's front matter goes to `props` (the pinned header) and its
/// changelog to `log` (the footer `h` shows, with its own scroll): neither is part of the text,
/// so the reading tools never touch them.
fn lay_out_prose(text: &str, is_md: bool, width: usize, look: Reading) -> Layout {
    let mut out = Layout::default();
    let tw = look.width(width);
    let pad = " ".repeat((width - tw) / 2);
    let centered = |mut l: Line| {
        if !pad.is_empty() {
            l.insert(0, (pad.clone(), Sty::Plain));
        }
        l
    };
    let none = || (String::new(), Sty::Plain);
    let mut md = if is_md { Md::reader() } else { Md::default() };
    let all: Vec<&str> = text.split('\n').collect();
    let mut skip = 0;
    if let Some((n, props)) = is_md.then(|| front_matter(&all)).flatten() {
        skip = n;
        out.props = props;
    }
    let src: Vec<String> = if is_md { md::reader_source(&all[skip..]) } else { all[skip..].iter().map(|l| l.to_string()).collect() };
    let mut i = 0;
    while i < src.len() {
        let line = src[i].as_str();
        i += 1;
        let heading = is_md && !md.in_code() && line.starts_with('#') && line.trim_start_matches('#').starts_with(' ');
        if heading && is_changelog(line) {
            out.log = changelog(&mut md, &src[i..], width);
            break;
        }
        // A table: its rows, from the header and its `|---|` line to the last `|` row.
        if is_md && !md.in_code() && line.trim_start().starts_with('|') && src.get(i).is_some_and(|n| is_table_rule(n)) {
            let end = (i + 1..src.len()).find(|k| !src[*k].trim_start().starts_with('|')).unwrap_or(src.len());
            let table: Vec<&str> = src[i - 1..end].iter().map(String::as_str).collect();
            out.end_para();
            for l in md.table(&table, tw) {
                out.push(centered(l));
            }
            out.push(Vec::new());
            out.at += 1;
            i = end;
            continue;
        }
        let rows = if is_md {
            if heading {
                out.end_para();
                out.headings.push(out.lines.len());
            }
            md.render_line(line, tw)
        } else {
            md::wrap(vec![(line.to_string(), Sty::Plain)], tw, none(), none())
        };
        let code = md.in_code();
        for l in rows {
            if l.iter().all(|(t, _)| t.trim().is_empty()) {
                out.end_para();
                continue;
            }
            out.push(centered(if look.bionic && !code { bionic(l) } else { l }));
            if look.spaced && !code {
                out.push(Vec::new());
            }
        }
    }
    while out.lines.last().is_some_and(|l| l.is_empty()) {
        out.lines.pop();
    }
    out.link_table = std::mem::take(&mut md.links);
    out
}

/// A table's `|---|:--:|` line, under its header.
fn is_table_rule(l: &str) -> bool {
    let t = l.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| "|-: ".contains(c))
}

/// The changelog (the source lines after its heading), rendered as it is: the heading isn't
/// repeated (the footer's bar names it), one blank row at most between blocks.
fn changelog(md: &mut Md, src: &[String], width: usize) -> Vec<Line> {
    let mut log: Vec<Line> = Vec::new();
    for s in src {
        for mut l in md.render_line(s, width) {
            if l.iter().all(|(t, _)| t.trim().is_empty()) {
                if log.last().is_some_and(|l| !l.is_empty()) {
                    log.push(Vec::new());
                }
                continue;
            }
            l.insert(0, (" ".to_string(), Sty::Plain));
            log.push(l);
        }
    }
    while log.last().is_some_and(|l| l.is_empty()) {
        log.pop();
    }
    log
}

/// The words from each row to the end (one more entry, 0, past the last row).
fn words_left(lines: &[Line]) -> Vec<usize> {
    let mut left = vec![0; lines.len() + 1];
    for (r, l) in lines.iter().enumerate().rev() {
        left[r] = left[r + 1] + l.iter().map(|(t, _)| t.split_whitespace().count()).sum::<usize>();
    }
    left
}

/// A markdown file's front matter (`---` to `---` at the top): the source lines it takes and its
/// properties, each `key` with its value as read (`[a, b]` and `- a` lists as `a · b`, quotes off);
/// empty ones left out. None when the file has none.
fn front_matter(lines: &[&str]) -> Option<(usize, Vec<(String, String)>)> {
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return None;
    }
    let end = lines.iter().skip(1).position(|l| l.trim_end() == "---")? + 1;
    let unquote = |v: &str| v.trim().trim_matches('"').trim_matches('\'').to_string();
    let mut props: Vec<(String, Vec<String>)> = Vec::new();
    for l in &lines[1..end] {
        if let Some(item) = l.trim_start().strip_prefix("- ").filter(|_| l.starts_with([' ', '-'])) {
            if let Some((_, vs)) = props.last_mut() {
                vs.push(unquote(item));
            }
        } else if let Some((k, v)) = l.split_once(':').filter(|(k, _)| !k.is_empty() && !k.starts_with(' ')) {
            let v = v.trim();
            let vs = match v.strip_prefix('[').and_then(|v| v.strip_suffix(']')).filter(|_| !v.starts_with("[[")) {
                Some(list) => list.split(',').map(unquote).filter(|x| !x.is_empty()).collect(),
                None if v.is_empty() => Vec::new(),
                None => vec![unquote(v)],
            };
            props.push((k.trim().to_string(), vs));
        }
    }
    let props = props.into_iter().filter(|(_, vs)| !vs.is_empty()).map(|(k, vs)| (k, vs.join(" · "))).collect();
    Some((end + 1, props))
}

/// A heading that opens a page's changelog (`## Changelog`, any level or case).
fn is_changelog(src: &str) -> bool {
    src.trim_start_matches('#').trim().eq_ignore_ascii_case("changelog")
}
