//! The sessions board: zen's home screen in full screen. Every session in sections (Main, Jobs,
//! Archived), whether each is running, idle or waiting on the owner's answers (questions asked
//! with `ask` and not yet answered), and the subagents and verifiers working under it.
//! Enter dives into the chosen session (subagents and verifiers read-only); `/board`, or esc on
//! an empty input, comes back. It refreshes from `GET /api/board` while it's shown.

use super::*;
use std::collections::HashMap;

/// How often the board asks the kernel what's running while it's shown.
pub(super) const BOARD_REFRESH: Duration = Duration::from_secs(2);

/// Finished child sessions shown under their parent (the newest); running ones always show.
const FINISHED_CHILDREN: usize = 3;

/// One row of the board: a section heading (no id) or a session.
pub(super) struct BoardRow {
    pub(super) id: Option<String>,
    pub(super) line: Line,
}

#[derive(Default)]
pub(super) struct Board {
    /// The kernel's sessions, newest activity first (`/api/board`); None until the first load.
    pub(super) sessions: Option<Vec<Value>>,
    pub(super) rows: Vec<BoardRow>,
    /// The highlighted session, kept by id across refreshes.
    pub(super) selected: Option<String>,
    pub(super) filter: String,
    /// Keys type into the filter (after `/`) instead of acting.
    pub(super) filtering: bool,
    /// Show the Archived section's sessions.
    pub(super) archived: bool,
    /// First row shown, when the list is taller than the screen.
    pub(super) top: usize,
    pub(super) error: Option<String>,
    /// Sessions (with their children) running at the last load.
    pub(super) running: usize,
    /// Sessions waiting on the owner's answers at the last load (archived ones aside).
    pub(super) waiting: usize,
}

/// A session isn't the owner's to type into: the kernel or a parent session drives it.
pub(super) fn read_only_kind(kind: &str) -> bool {
    matches!(kind, "subagent" | "verifier")
}

/// Seconds since the Unix epoch of an RFC 3339 time in UTC (`2026-10-09T12:00:00.5Z`).
pub(super) fn epoch_secs(t: &str) -> Option<i64> {
    let num = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// "now", "5m", "3h", "2d" since `t`.
pub(super) fn ago(t: &str, now: i64) -> String {
    let Some(s) = epoch_secs(t).map(|t| (now - t).max(0)) else { return String::new() };
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn s_id(s: &Value) -> &str {
    s["id"].as_str().unwrap_or("")
}

fn busy(s: &Value) -> bool {
    s["busy"] == true
}

/// The session asked the owner questions (`ask`) they haven't answered yet.
fn waiting(s: &Value) -> bool {
    s["waiting"] == true
}

fn title_of(s: &Value) -> String {
    clean(s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(untitled)"))
}

/// What a child row says: its task (the first message it got), else its title.
fn task_of(s: &Value) -> String {
    let t = s["task"].as_str().filter(|t| !t.trim().is_empty()).map(clean).unwrap_or_else(|| title_of(s));
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The board section a top-level session is listed in.
fn section(s: &Value) -> &'static str {
    match (s["archived"] == true, s["kind"] == "job") {
        (true, _) => "Archived",
        (false, true) => "Jobs",
        (false, false) => "Main",
    }
}

fn model_of(s: &Value) -> &str {
    let m = s["model"].as_str().unwrap_or("");
    m.split('/').next_back().unwrap_or(m)
}

impl Board {
    /// Build the rows from the sessions, the filter and the archived switch, keeping the selection.
    pub(super) fn rebuild(&mut self, now: i64, spin: usize) {
        let Some(all) = &self.sessions else {
            self.rows.clear();
            return;
        };
        let mut children: HashMap<&str, Vec<&Value>> = HashMap::new();
        for s in all.iter().filter(|s| s["parent"].is_string()) {
            children.entry(s["parent"].as_str().unwrap_or("")).or_default().push(s);
        }
        let top: Vec<&Value> = all.iter().filter(|s| !s["parent"].is_string()).collect();
        let kids = |s: &Value| children.get(s_id(s)).cloned().unwrap_or_default();
        let active = |s: &Value| busy(s) || kids(s).iter().any(|c| busy(c));
        self.running = top.iter().filter(|s| active(s)).count();
        self.waiting = top.iter().filter(|s| waiting(s) && section(s) != "Archived").count();
        let needle = self.filter.to_lowercase();
        let hit = |s: &Value| needle.is_empty() || title_of(s).to_lowercase().contains(&needle) || task_of(s).to_lowercase().contains(&needle);

        let mut rows = Vec::new();
        for name in ["Main", "Jobs", "Archived"] {
            let mut group: Vec<&Value> = top.iter().copied().filter(|s| section(s) == name).filter(|s| hit(s) || kids(s).iter().any(|c| hit(c))).collect();
            // Waiting on the owner first, then running; otherwise the kernel's order (latest activity first).
            group.sort_by_key(|s| (!waiting(s), !active(s)));
            let archived = name == "Archived";
            let hidden = archived && !self.archived && needle.is_empty();
            let mut head = vec![(format!(" {}", name.to_uppercase()), Sty::Bold), (format!("  {}", group.len()), Sty::Dim)];
            if archived {
                head.push((if self.archived { "  · a hides" } else { "  · a shows" }.into(), Sty::Dim));
            }
            if group.is_empty() && name != "Main" {
                continue;
            }
            rows.push(BoardRow { id: None, line: head });
            if hidden {
                continue;
            }
            if group.is_empty() {
                rows.push(BoardRow { id: None, line: line("   nothing here · n starts a session", Sty::Dim) });
            }
            for s in group {
                let mut ks = kids(s);
                ks.retain(|c| needle.is_empty() || hit(s) || hit(c));
                let (run, done): (Vec<&Value>, Vec<&Value>) = ks.into_iter().partition(|c| busy(c));
                let shown_done = if archived { 0 } else { FINISHED_CHILDREN };
                let more = done.len().saturating_sub(shown_done);
                rows.push(BoardRow { id: Some(s_id(s).to_string()), line: Self::session_line(s, 0, now, spin, run.len(), done.len() + run.len(), more) });
                for c in run.iter().chain(done.iter().take(shown_done)) {
                    rows.push(BoardRow { id: Some(s_id(c).to_string()), line: Self::session_line(c, 1, now, spin, 0, 0, 0) });
                }
            }
            rows.push(BoardRow { id: None, line: Vec::new() });
        }
        self.rows = rows;
        let ids: Vec<&str> = self.rows.iter().filter_map(|r| r.id.as_deref()).collect();
        if !self.selected.as_deref().is_some_and(|s| ids.contains(&s)) {
            self.selected = ids.first().map(|s| s.to_string());
        }
    }

    /// A session's row: state, title (or a child's task), then model, age and its subagents.
    fn session_line(s: &Value, depth: usize, now: i64, spin: usize, running_kids: usize, kids: usize, more: usize) -> Line {
        let (mark, sty) = if waiting(s) && !busy(s) {
            ("?", Sty::Warn)
        } else if busy(s) || running_kids > 0 {
            (SPINNER[spin % SPINNER.len()], Sty::Accent)
        } else {
            ("○", Sty::Dim)
        };
        let mut l: Line = Vec::new();
        if depth > 0 {
            l.push(("    ↳ ".into(), Sty::Dim));
            l.push((format!("{mark} "), sty));
            l.push((format!("{}: ", s["kind"].as_str().unwrap_or("child")), Sty::Dim));
            l.push((task_of(s), Sty::Plain));
        } else {
            l.push((format!("  {mark} "), sty));
            l.push((title_of(s), if busy(s) || running_kids > 0 || waiting(s) { Sty::Bold } else { Sty::Plain }));
        }
        if waiting(s) {
            l.push(("  · waiting on you".into(), Sty::Warn));
        }
        let mut meta = format!("{}· {}", if waiting(s) { " " } else { "  " }, model_of(s));
        if busy(s) {
            meta.push_str(" · running");
        } else {
            let a = ago(s["updated_at"].as_str().unwrap_or(""), now);
            if !a.is_empty() {
                meta.push_str(&format!(" · {a}"));
            }
        }
        if kids > 0 {
            let what = if running_kids > 0 { format!("{running_kids}/{kids} subagents working") } else { format!("{kids} subagent{}", if kids == 1 { "" } else { "s" }) };
            meta.push_str(&format!(" · {what}"));
        }
        if more > 0 {
            meta.push_str(&format!(" ({more} older not shown)"));
        }
        l.push((meta, Sty::Dim));
        l
    }

    /// Indexes of rows that are sessions (the ones you can select).
    fn selectable(&self) -> Vec<usize> {
        self.rows.iter().enumerate().filter(|(_, r)| r.id.is_some()).map(|(i, _)| i).collect()
    }

    pub(super) fn selected_row(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.id.is_some() && r.id == self.selected)
    }

    /// Move the highlight by `by` sessions (negative: up), stopping at the ends.
    pub(super) fn step(&mut self, by: isize) {
        let sel = self.selectable();
        if sel.is_empty() {
            return;
        }
        let at = self.selected_row().and_then(|r| sel.iter().position(|&i| i == r)).unwrap_or(0) as isize;
        let to = (at + by).clamp(0, sel.len() as isize - 1) as usize;
        self.selected = self.rows[sel[to]].id.clone();
    }
}

impl App {
    /// Show the board (leaving the current session; a turn running in it keeps going).
    pub(super) async fn open_board(&mut self) {
        if self.inline {
            self.note("the sessions board needs full screen (zen without --inline); /resume switches sessions", Sty::Dim);
            return;
        }
        let selected = self.session.clone();
        // Text typed in a read-only session can't have been meant for it; don't carry it along.
        if self.read_only {
            self.editor.clear();
        }
        self.reset_session();
        self.picker = None;
        self.side_focus = false;
        self.board = Some(Board { selected, ..Board::default() });
        self.refresh_board().await;
    }

    /// Fetch what's running from the kernel and rebuild the board's rows.
    pub(super) async fn refresh_board(&mut self) {
        let got = self.c.get("/api/board").await;
        let Some(b) = &mut self.board else { return };
        match got {
            Ok(v) => {
                b.sessions = Some(v["sessions"].as_array().cloned().unwrap_or_default());
                b.error = None;
            }
            Err(e) => b.error = Some(format!("{e:#}")),
        }
        self.rebuild_board();
    }

    pub(super) fn rebuild_board(&mut self) {
        let spin = self.spin;
        if let Some(b) = &mut self.board {
            b.rebuild(now_secs(), spin);
        }
    }

    /// Anything on the board is running (so its spinners turn).
    pub(super) fn board_running(&self) -> bool {
        self.board.as_ref().is_some_and(|b| b.running > 0)
    }

    /// Start a new session (`n` on the board, `/new`): an empty chat on the default model; the
    /// kernel creates the session with the first prompt.
    pub(super) fn fresh_session(&mut self) {
        self.board = None;
        self.reset_session();
        self.model = self.default_model.clone();
        self.effort = None;
        self.commit(vec![line("── new session ──", Sty::Dim), Vec::new()]);
    }

    /// Dive into the session highlighted on the board.
    pub(super) async fn open_from_board(&mut self) -> Result<()> {
        let Some(id) = self.board.as_ref().and_then(|b| b.selected.clone()) else { return Ok(()) };
        self.board = None;
        if let Err(e) = self.switch_session(id).await {
            self.open_board().await;
            return Err(e);
        }
        Ok(())
    }

    /// A key on the board. Returns Ok after handling every key (nothing reaches the input).
    pub(super) async fn board_key(&mut self, k: KeyEvent) -> Result<()> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.height().saturating_sub(4).max(1) as isize;
        let Some(b) = &mut self.board else { return Ok(()) };
        if b.filtering {
            match k.code {
                KeyCode::Esc => {
                    b.filter.clear();
                    b.filtering = false;
                }
                KeyCode::Enter => b.filtering = false,
                KeyCode::Backspace => {
                    b.filter.pop();
                }
                KeyCode::Up => b.step(-1),
                KeyCode::Down => b.step(1),
                KeyCode::Char('c' | 'd') if ctrl => self.quit = true,
                KeyCode::Char(ch) if !ctrl => b.filter.push(ch),
                _ => {}
            }
            self.rebuild_board();
            return Ok(());
        }
        match k.code {
            KeyCode::Up | KeyCode::Char('k') if !ctrl => b.step(-1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => b.step(1),
            KeyCode::PageUp => b.step(-page),
            KeyCode::PageDown => b.step(page),
            KeyCode::Home => b.step(isize::MIN / 2),
            KeyCode::End => b.step(isize::MAX / 2),
            KeyCode::Enter => return self.open_from_board().await,
            KeyCode::Char('n') if !ctrl => self.fresh_session(),
            KeyCode::Char('/') => b.filtering = true,
            KeyCode::Char('a') if !ctrl => {
                b.archived = !b.archived;
                self.rebuild_board();
            }
            KeyCode::Char('r') if !ctrl => self.refresh_board().await,
            KeyCode::Esc if !b.filter.is_empty() => {
                b.filter.clear();
                self.rebuild_board();
            }
            KeyCode::Char('q') if !ctrl => self.quit = true,
            KeyCode::Char('c' | 'd') if ctrl => self.quit = true,
            _ => {}
        }
        Ok(())
    }

    /// A click on the board opens the session in that row.
    pub(super) async fn click_board(&mut self, y: usize) -> Result<()> {
        let Some(b) = &mut self.board else { return Ok(()) };
        // Rows start below the title (2 lines) and the filter line, if any.
        let first = 2 + usize::from(b.filtering || !b.filter.is_empty());
        let Some(r) = y.checked_sub(first).map(|r| r + b.top) else { return Ok(()) };
        if let Some(id) = b.rows.get(r).and_then(|r| r.id.clone()) {
            b.selected = Some(id);
            return self.open_from_board().await;
        }
        Ok(())
    }

    /// The board's frame: title, filter, sessions in sections, and the keys at the bottom.
    pub(super) fn board_lines(&mut self) -> Vec<Line> {
        let (w, h) = (self.width(), self.height());
        let Some(b) = &mut self.board else { return Vec::new() };
        let total = b.sessions.as_ref().map(|s| s.iter().filter(|s| !s["parent"].is_string()).count()).unwrap_or(0);
        let right = match (&b.sessions, b.running) {
            (None, _) => "loading…".to_string(),
            (_, 0) if b.waiting == 0 => format!("{total} sessions · all idle"),
            (_, 0) => format!("{total} sessions"),
            (_, n) => format!("{total} sessions · {n} running"),
        };
        // Sessions waiting on the owner stand out in the title, before the count.
        let ask = if b.sessions.is_some() && b.waiting > 0 { format!("? {} waiting on you · ", b.waiting) } else { String::new() };
        let left = "zen · sessions";
        let gap = w.saturating_sub(UnicodeWidthStr::width(left) + UnicodeWidthStr::width(ask.as_str()) + UnicodeWidthStr::width(right.as_str()) + 1).max(2);
        let mut out = vec![
            vec![(left.into(), Sty::Bold), (" ".repeat(gap), Sty::Plain), (ask, Sty::Warn), (right, if b.running > 0 { Sty::Accent } else { Sty::Dim })],
            Vec::new(),
        ];
        if b.filtering || !b.filter.is_empty() {
            let cursor = if b.filtering { "▏" } else { "" };
            out.push(vec![(" filter: ".into(), Sty::Dim), (format!("{}{cursor}", b.filter), Sty::Accent)]);
        }
        let footer: Vec<Line> = {
            let mut f = Vec::new();
            if let Some(e) = &b.error {
                f.push(line(format!(" can't reach zenbot: {e}"), Sty::Err));
            }
            f.push(line(
                if b.filtering { " type to filter · enter keep · esc clear" } else { " ↑↓ choose · enter open · n new session · / filter · a archived · r refresh · q quit" },
                Sty::Dim,
            ));
            f
        };
        let room = h.saturating_sub(out.len() + footer.len()).max(1);
        // Keep the highlighted row in view.
        let sel = b.selected_row().unwrap_or(0);
        if sel < b.top {
            b.top = sel.saturating_sub(1);
        } else if sel >= b.top + room {
            b.top = sel + 1 - room;
        }
        b.top = b.top.min(b.rows.len().saturating_sub(room));
        for (i, r) in b.rows.iter().enumerate().skip(b.top).take(room) {
            if i == sel && r.id.is_some() {
                let mut l: Line = vec![("›".into(), Sty::Accent)];
                l.extend(r.line.iter().map(|(t, s)| (t.clone(), if *s == Sty::Plain { Sty::Accent } else { *s })));
                // The first cell of a row is a space; the marker takes it.
                if let Some((t, _)) = l.get_mut(1) {
                    if t.starts_with(' ') {
                        t.remove(0);
                    }
                }
                out.push(l);
            } else {
                out.push(r.line.clone());
            }
        }
        if b.sessions.is_some() && b.rows.iter().all(|r| r.id.is_none()) && !b.filter.is_empty() {
            out.push(line(format!("   no session matches “{}”", b.filter), Sty::Dim));
        }
        while out.len() < h - footer.len() {
            out.push(Vec::new());
        }
        out.extend(footer);
        out
    }

    /// Draw the board as the whole frame (no caret).
    pub(super) fn board_frame(&mut self) {
        let w = self.width();
        let rows: Vec<String> = self.board_lines().iter().map(|l| md::to_ansi(&screen::fit(l, w, false))).collect();
        self.last_vh = 0;
        let out = self.screen.frame(rows, None);
        self.flush(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    const NOW: &str = "2026-10-09T12:00:00Z";

    fn s(id: &str, title: &str, extra: Value) -> Value {
        let mut v = json!({ "id": id, "title": title, "model": "claude/claude-opus-5-5", "archived": false, "kind": null, "parent": null, "busy": false, "updated_at": "2026-10-09T11:00:00Z" });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    fn sample() -> Vec<Value> {
        vec![
            s("idle-main", "zenfitness research", json!({ "updated_at": "2026-10-09T11:55:00Z" })),
            s("busy-main", "sessions board feature", json!({})),
            s("kid-1", "subagent: sessions board feature", json!({ "kind": "subagent", "parent": "busy-main", "busy": true, "task": "Read the kernel's\nsession code" })),
            s("kid-2", "verifier: x", json!({ "kind": "verifier", "parent": "busy-main", "task": "Check the diff" })),
            s("job-1", "job: sleep", json!({ "kind": "job" })),
            s("old", "archived thing", json!({ "archived": true })),
        ]
    }

    fn board_app(cols: usize, rows: usize) -> App {
        let mut a = app(cols, rows);
        let mut b = Board { sessions: Some(sample()), ..Board::default() };
        b.rebuild(epoch_secs(NOW).unwrap(), 0);
        a.board = Some(b);
        a.draw();
        a
    }

    #[test]
    fn times_parse_and_read_as_ages() {
        assert_eq!(epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_secs("2026-10-09T12:00:00.123456Z"), Some(1_791_547_200));
        let now = epoch_secs(NOW).unwrap();
        assert_eq!(ago("2026-10-09T11:59:30Z", now), "now");
        assert_eq!(ago("2026-10-09T11:55:00Z", now), "5m");
        assert_eq!(ago("2026-10-09T09:00:00Z", now), "3h");
        assert_eq!(ago("2026-10-07T12:00:00Z", now), "2d");
        assert_eq!(ago("garbage", now), "");
    }

    #[test]
    fn the_board_shows_sections_running_first_with_subagents_nested() {
        let a = board_app(100, 30);
        let rows = plain_rows(&a);
        let text = rows.join("\n");
        assert!(rows[0].starts_with("zen · sessions") && rows[0].contains("4 sessions · 1 running"), "{text}");
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("no {needle}: {text}"));
        assert!(at("MAIN") < at("sessions board feature") && at("sessions board feature") < at("zenfitness research"), "running first: {text}");
        assert!(at("sessions board feature") < at("subagent: Read the kernel's session code") && at("subagent: Read") < at("verifier: Check the diff"), "children under their parent, running first, task on one line: {text}");
        assert!(rows[at("sessions board feature")].contains("1/2 subagents working"), "{text}");
        assert!(rows[at("zenfitness research")].contains("· 5m"), "idle sessions say how long ago: {text}");
        assert!(at("zenfitness research") < at("JOBS") && at("JOBS") < at("job: sleep") && at("job: sleep") < at("ARCHIVED"), "jobs apart from mains: {text}");
        assert!(!text.contains("archived thing") && rows[at("ARCHIVED")].contains("a shows"), "archived hidden until asked: {text}");
        assert!(rows[at("sessions board feature")].starts_with("›"), "the first session is highlighted: {text}");
        assert!(rows.last().unwrap().contains("enter open · n new session"), "{text}");
        assert!(rows.iter().all(|r| width(r) < 100), "rows fit");
    }

    #[test]
    fn sessions_waiting_on_the_owner_are_flagged_and_listed_first() {
        let mut a = app(100, 30);
        let mut list = sample();
        list.push(s("asks", "pricing questions", json!({ "waiting": true, "updated_at": "2026-10-09T10:00:00Z" })));
        list.push(s("old-ask", "archived question", json!({ "waiting": true, "archived": true })));
        let mut b = Board { sessions: Some(list), ..Board::default() };
        b.rebuild(epoch_secs(NOW).unwrap(), 0);
        a.board = Some(b);
        a.draw();
        let rows = plain_rows(&a);
        let text = rows.join("\n");
        assert!(rows[0].contains("? 1 waiting on you · 6 sessions · 1 running"), "counted in the title, archived aside: {text}");
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("no {needle}: {text}"));
        assert!(at("MAIN") < at("pricing questions") && at("pricing questions") < at("sessions board feature"), "waiting before running: {text}");
        assert!(rows[at("pricing questions")].contains("? pricing questions  · waiting on you · claude-opus-5-5 · 2h"), "{text}");
        assert!(!rows[at("zenfitness research")].contains("waiting"), "{text}");
        // Nothing running: the title still says who waits, not "all idle".
        let mut b = Board { sessions: Some(vec![s("asks", "pricing questions", json!({ "waiting": true }))]), ..Board::default() };
        b.rebuild(epoch_secs(NOW).unwrap(), 0);
        a.board = Some(b);
        a.draw();
        let top = &plain_rows(&a)[0];
        assert!(top.contains("? 1 waiting on you · 1 sessions") && !top.contains("idle"), "{top}");
    }

    #[tokio::test]
    async fn keys_move_filter_and_show_archived() {
        let mut a = board_app(100, 30);
        key(&mut a, KeyCode::Down).await;
        assert_eq!(a.board.as_ref().unwrap().selected.as_deref(), Some("kid-1"), "children can be chosen");
        key(&mut a, KeyCode::End).await;
        assert_eq!(a.board.as_ref().unwrap().selected.as_deref(), Some("job-1"));
        key(&mut a, KeyCode::Char('a')).await;
        a.draw();
        assert!(plain_rows(&a).join("\n").contains("archived thing"));
        key(&mut a, KeyCode::Char('/')).await;
        typed(&mut a, "check").await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("filter: check") && text.contains("verifier: Check the diff") && text.contains("sessions board feature"), "a matching child shows with its parent: {text}");
        assert!(!text.contains("zenfitness") && !text.contains("job: sleep") && !text.contains("subagent: Read"), "{text}");
        key(&mut a, KeyCode::Esc).await;
        a.draw();
        assert!(plain_rows(&a).join("\n").contains("zenfitness"), "esc clears the filter");
        assert!(!a.quit, "letters typed into the filter don't act");
        key(&mut a, KeyCode::Char('q')).await;
        assert!(a.quit);
    }

    #[tokio::test]
    async fn n_starts_a_new_session_from_the_board() {
        let mut a = board_app(80, 24);
        key(&mut a, KeyCode::Char('n')).await;
        a.draw();
        assert!(a.board.is_none() && a.session.is_none() && !a.read_only);
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("new session") && text.contains('╰'), "the chat with its input: {text}");
    }

    #[tokio::test]
    async fn esc_on_an_empty_input_goes_back_to_the_board_and_a_read_only_session_never_interrupts() {
        let mut a = app(80, 24);
        a.session = Some("kid-1".into());
        a.read_only = true;
        a.busy = true;
        typed(&mut a, "hello").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(a.entries.iter().all(|e| !matches!(e, Entry::User(_))), "nothing is sent to a subagent");
        assert!(a.notice.as_ref().is_some_and(|(t, _)| t.contains("read-only")), "{:?}", a.notice);
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("this session is read-only") && text.contains("esc: back to the board") && !text.contains("esc to interrupt"), "said while its turn runs: {text}");
        key(&mut a, KeyCode::Esc).await;
        assert!(!a.aborting, "esc doesn't stop the subagent's turn");
        assert!(a.board.is_some() && a.session.is_none(), "it goes back to the board");
        assert!(a.editor.is_empty(), "what was typed there is dropped");
        // A session of your own: esc interrupts a running turn, and goes back only when idle.
        let mut b = app(80, 24);
        b.session = Some("s1".into());
        b.busy = true;
        key(&mut b, KeyCode::Esc).await;
        assert!(b.aborting && b.board.is_none());
        b.busy = false;
        b.aborting = false;
        key(&mut b, KeyCode::Esc).await;
        assert!(b.board.is_some());
    }

    #[tokio::test]
    async fn board_leaves_a_running_session_and_inline_mode_has_no_board() {
        let mut a = app(80, 24);
        a.session = Some("s1".into());
        a.busy = true;
        a.command("/board").await.unwrap();
        assert!(a.board.is_some() && a.session.is_none() && !a.busy, "the turn keeps running in the kernel; the board shows it");
        let mut b = app(80, 24);
        b.inline = true;
        b.command("/board").await.unwrap();
        assert!(b.board.is_none() && b.notice.as_ref().is_some_and(|(t, _)| t.contains("full screen")));
    }

    #[test]
    fn a_long_board_scrolls_to_keep_the_highlight_in_view() {
        let mut a = app(80, 12);
        let mut list: Vec<Value> = (0..30).map(|i| s(&format!("id{i}"), &format!("session {i}"), json!({}))).collect();
        list[0]["busy"] = json!(false);
        let mut b = Board { sessions: Some(list), ..Board::default() };
        b.rebuild(epoch_secs(NOW).unwrap(), 0);
        b.step(25);
        a.board = Some(b);
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("› ○ session 25"), "{text}");
        assert_eq!(plain_rows(&a).len(), 12);
    }
}
