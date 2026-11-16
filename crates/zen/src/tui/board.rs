//! The sessions board: zen's home screen in full screen, with the tab bar of `views.rs` on top
//! (Sessions · Files · Tasks). Its Sessions view lists every session in sections (Channels,
//! Sessions, Jobs, Archived), whether each is running, idle or waiting on the owner's answers
//! (questions asked with `ask` and not yet answered), and the subagents and verifiers working
//! under it. Enter dives into the chosen session (subagents and verifiers read-only); `/board`, or
//! esc on an empty input, comes back. The same list (`Board`, less the open session) is the side
//! panel's Sessions view. Whichever list is shown refreshes from `GET /api/board`, in a task of
//! its own: the answer comes back as a `board` event, and the rows are rebuilt only if it changed.
//! The kernel sends the latest sessions and children (`api::board` says how many), archived ones
//! only while they're shown. The first row of Sessions is a button, `+ new session` (enter on it
//! is `n`; hidden while filtering); archived jobs (the frequent ones) stay out of Archived until
//! `J` shows them.

use super::*;
use std::collections::HashMap;

/// How often the board asks the kernel what's running while it's shown.
pub(super) const BOARD_REFRESH: Duration = Duration::from_secs(2);

/// What the home screen's top row starts with, before the tab bar.
pub(super) const HOME_PREFIX: &str = "zen ·";

/// Finished child sessions shown under their parent (the newest); running ones always show.
const FINISHED_CHILDREN: usize = 3;

/// The id of the `+ new session` row: a button among the sessions, not a session (no real id has a NUL).
pub(super) const NEW_SESSION: &str = "\0new";

/// One row of the board: a section heading (no id), a session, or the new-session button.
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
    /// Show archived jobs in the Archived section (`J`); hidden otherwise.
    pub(super) jobs: bool,
    /// First row shown, when the list is taller than the screen.
    pub(super) top: usize,
    pub(super) error: Option<String>,
    /// Sessions (with their children) running at the last load.
    pub(super) running: usize,
    /// Sessions waiting on the owner's answers at the last load (archived ones aside).
    pub(super) waiting: usize,
    /// Archived sessions the kernel has, when it didn't send them (`a` asks for them).
    pub(super) archived_count: usize,
    /// A fetch is on its way; the poll waits for it.
    pub(super) loading: bool,
    /// Left out of the list: the session open next to it (the side panel's list).
    pub(super) hide: Option<String>,
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

pub(super) fn now_secs() -> i64 {
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
    match (s["archived"] == true, s["kind"].as_str()) {
        (true, _) => "Archived",
        (false, Some("channel")) => "Channels",
        (false, Some("job")) => "Jobs",
        (false, _) => "Sessions",
    }
}

fn model_of(s: &Value) -> &str {
    let m = s["model"].as_str().unwrap_or("");
    m.split('/').next_back().unwrap_or(m)
}

/// `@agent · ` for a session that isn't zenbot's, else nothing.
pub(super) fn agent_tag(agent: Option<&str>) -> String {
    agent.filter(|a| !a.is_empty() && *a != "zenbot").map(|a| format!("@{a} · ")).unwrap_or_default()
}

impl Board {
    /// Build the rows from the sessions, the filter and the archived switch, keeping the selection.
    pub(super) fn rebuild(&mut self, now: i64, spin: usize) {
        let Some(all) = &self.sessions else {
            self.rows.clear();
            return;
        };
        let hide = self.hide.as_deref();
        let all: Vec<&Value> = all.iter().filter(|s| Some(s_id(s)) != hide).collect();
        let mut children: HashMap<&str, Vec<&Value>> = HashMap::new();
        for s in all.iter().copied().filter(|s| s["parent"].is_string()) {
            children.entry(s["parent"].as_str().unwrap_or("")).or_default().push(s);
        }
        let top: Vec<&Value> = all.iter().copied().filter(|s| !s["parent"].is_string()).collect();
        let kids = |s: &Value| children.get(s_id(s)).cloned().unwrap_or_default();
        let active = |s: &Value| busy(s) || kids(s).iter().any(|c| busy(c));
        self.running = top.iter().filter(|s| active(s)).count();
        self.waiting = top.iter().filter(|s| waiting(s) && section(s) != "Archived").count();
        let needle = self.filter.to_lowercase();
        let hit = |s: &Value| needle.is_empty() || title_of(s).to_lowercase().contains(&needle) || task_of(s).to_lowercase().contains(&needle);

        let mut rows = Vec::new();
        for name in ["Channels", "Sessions", "Jobs", "Archived"] {
            let mut group: Vec<&Value> = top.iter().copied().filter(|s| section(s) == name).filter(|s| hit(s) || kids(s).iter().any(|c| hit(c))).collect();
            let archived = name == "Archived";
            // Archived jobs stay out until `J`; the section says how many.
            let hidden_jobs = if archived && !self.jobs { group.iter().filter(|s| s["kind"] == "job").count() } else { 0 };
            if hidden_jobs > 0 {
                group.retain(|s| s["kind"] != "job");
            }
            // Waiting on the owner first, then running; otherwise the kernel's order (latest activity first).
            group.sort_by_key(|s| (!waiting(s), !active(s)));
            let hidden = archived && !self.archived && needle.is_empty();
            // Archived sessions the kernel didn't send still count.
            let count = if archived && !self.archived { group.len().max(self.archived_count) } else { group.len() };
            let mut head = vec![(format!(" {}", name.to_uppercase()), Sty::Bold), (format!("  {count}"), Sty::Dim)];
            if archived {
                let jobs = match (self.archived, self.jobs) {
                    (false, _) => "",
                    (true, true) => " · J hides jobs",
                    (true, false) => &format!(" · J shows jobs ({hidden_jobs})"),
                };
                head.push((format!("{}{jobs}", if self.archived { "  · a hides" } else { "  · a shows" }), Sty::Dim));
            }
            if count == 0 && hidden_jobs == 0 && name != "Sessions" {
                continue;
            }
            rows.push(BoardRow { id: None, line: head });
            if hidden {
                continue;
            }
            // The button that starts a session, `n`'s twin; a filter looks for sessions only.
            if name == "Sessions" && needle.is_empty() {
                rows.push(BoardRow { id: Some(NEW_SESSION.into()), line: line("  + new session", Sty::Dim) });
            }
            if group.is_empty() && !archived {
                rows.push(BoardRow { id: None, line: line("   nothing here · n starts a session", Sty::Dim) });
            }
            for s in group {
                let mut ks = kids(s);
                ks.retain(|c| needle.is_empty() || hit(s) || hit(c));
                let (run, done): (Vec<&Value>, Vec<&Value>) = ks.into_iter().partition(|c| busy(c));
                let shown_done = if archived { 0 } else { FINISHED_CHILDREN };
                // The kernel sends only the latest children; `children` counts them all.
                let sent = done.len() + run.len();
                let all_kids = if needle.is_empty() { s["children"].as_u64().map_or(sent, |n| n as usize).max(sent) } else { sent };
                let more = all_kids - run.len() - done.len().min(shown_done);
                rows.push(BoardRow { id: Some(s_id(s).to_string()), line: Self::session_line(s, 0, now, spin, run.len(), all_kids, more) });
                for c in run.iter().chain(done.iter().take(shown_done)) {
                    rows.push(BoardRow { id: Some(s_id(c).to_string()), line: Self::session_line(c, 1, now, spin, 0, 0, 0) });
                }
            }
            rows.push(BoardRow { id: None, line: Vec::new() });
        }
        self.rows = rows;
        // The button is never the default choice: enter on a fresh board opens the first session.
        let ids: Vec<&str> = self.rows.iter().filter_map(|r| r.id.as_deref()).collect();
        if !self.selected.as_deref().is_some_and(|s| ids.contains(&s)) {
            self.selected = ids.iter().find(|id| **id != NEW_SESSION).map(|s| s.to_string());
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
        let mut meta = format!("{}· {}{}", if waiting(s) { " " } else { "  " }, agent_tag(s["agent"].as_str()), model_of(s));
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

    /// The list's rows that fit in `room` rows, scrolled to keep the highlighted one in view.
    pub(super) fn list_lines(&mut self, room: usize) -> Vec<Line> {
        let mut out = Vec::new();
        if self.sessions.is_none() {
            let what = match &self.error {
                Some(e) => line(format!(" can't reach zenbot: {e}"), Sty::Err),
                None => line(" loading…", Sty::Dim),
            };
            return vec![what];
        }
        let room = room.max(1);
        let sel = self.selected_row().unwrap_or(0);
        if sel < self.top {
            self.top = sel.saturating_sub(1);
        } else if sel >= self.top + room {
            self.top = sel + 1 - room;
        }
        self.top = self.top.min(self.rows.len().saturating_sub(room));
        for (i, r) in self.rows.iter().enumerate().skip(self.top).take(room) {
            if i == sel && r.id.is_some() {
                let mut l: Line = vec![("›".into(), Sty::Accent)];
                let button = r.id.as_deref() == Some(NEW_SESSION);
                l.extend(r.line.iter().map(|(t, s)| (t.clone(), if *s == Sty::Plain || button { Sty::Accent } else { *s })));
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
        if self.rows.iter().all(|r| r.id.is_none()) && !self.filter.is_empty() {
            out.push(line(format!("   no session matches “{}”", self.filter), Sty::Dim));
        }
        out
    }
}

impl App {
    /// Show the board (leaving the current session; a turn running in it keeps going).
    pub(super) fn open_board(&mut self) {
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
        self.refresh_board();
    }

    /// The list of sessions on screen: the board's, or the side panel's on its Sessions view.
    pub(super) fn list(&self) -> Option<&Board> {
        match &self.board {
            Some(b) => Some(b),
            None => self.side_sessions_shown().then_some(&self.side_list),
        }
    }

    pub(super) fn list_mut(&mut self) -> Option<&mut Board> {
        let side = self.side_sessions_shown();
        match &mut self.board {
            Some(b) => Some(b),
            None => side.then_some(&mut self.side_list),
        }
    }

    /// The side panel is open on its Sessions view.
    fn side_sessions_shown(&self) -> bool {
        self.side_view == View::Sessions && self.columns().1 > 0
    }

    /// Ask the kernel what's running, in a task of its own; `board_loaded` takes the answer.
    pub(super) fn refresh_board(&mut self) {
        let Some(b) = self.list_mut() else { return };
        b.loading = true;
        let archived = b.archived;
        self.board_fetch += 1;
        let fetch = self.board_fetch;
        let (c, tx) = (self.c.clone(), self.tx.clone());
        tokio::spawn(async move {
            let path = if archived { "/api/board?archived=true" } else { "/api/board" };
            let ev = match c.get(path).await {
                Ok(v) => json!({ "type": "board", "fetch": fetch, "board": v }),
                Err(e) => json!({ "type": "board", "fetch": fetch, "error": format!("{e:#}") }),
            };
            let _ = tx.send((String::new(), 0, ev));
        });
    }

    /// The kernel's answer to the latest board fetch: rebuild and draw only if something changed.
    pub(super) fn board_loaded(&mut self, ev: Value) {
        if ev["fetch"].as_u64() != Some(self.board_fetch) {
            return;
        }
        let Some(b) = self.list_mut() else { return };
        b.loading = false;
        let changed = match ev["error"].as_str() {
            Some(e) => b.error.as_deref() != Some(e) && {
                b.error = Some(e.to_string());
                true
            },
            None => {
                let v = &ev["board"];
                let sessions = v["sessions"].as_array().cloned().unwrap_or_default();
                let archived_count = v["archived"].as_u64().unwrap_or(0) as usize;
                let changed = b.error.is_some() || b.sessions.as_ref() != Some(&sessions) || b.archived_count != archived_count;
                b.sessions = Some(sessions);
                b.archived_count = archived_count;
                b.error = None;
                changed
            }
        };
        if changed {
            self.rebuild_board();
            self.draw();
        }
    }

    /// Rebuild the list on screen; the side panel's leaves out the session that's open.
    pub(super) fn rebuild_board(&mut self) {
        let (spin, open) = (self.spin, self.session.clone());
        let side = self.board.is_none();
        if let Some(b) = self.list_mut() {
            if side {
                b.hide = open;
            }
            b.rebuild(now_secs(), spin);
        }
    }

    /// Anything on the list on screen is running (so its spinners turn).
    pub(super) fn board_running(&self) -> bool {
        self.list().is_some_and(|b| b.running > 0)
    }

    /// Start a new session (`n` on the board, `/new`): an empty chat on the default model; the
    /// kernel creates the session with the first prompt.
    pub(super) fn fresh_session(&mut self) {
        self.fresh_session_as(None);
    }

    /// Start a new session with agent `agent` (`/new @garry`; None: zenbot).
    pub(super) fn fresh_session_as(&mut self, agent: Option<String>) {
        self.board = None;
        self.reset_session();
        self.model = self.default_model.clone();
        self.model_pinned = false;
        self.effort = None;
        let head = agent.as_deref().map(|a| format!("── new session with {a} ──")).unwrap_or_else(|| "── new session ──".into());
        self.agent = agent;
        self.commit(vec![line(head, Sty::Dim), Vec::new()]);
    }

    /// Dive into the session highlighted on the board.
    pub(super) async fn open_from_board(&mut self) -> Result<()> {
        let Some(id) = self.board.as_ref().and_then(|b| b.selected.clone()) else { return Ok(()) };
        if id == NEW_SESSION {
            self.fresh_session();
            return Ok(());
        }
        self.board = None;
        if let Err(e) = self.switch_session(id).await {
            self.open_board();
            return Err(e);
        }
        Ok(())
    }

    /// A key on the board. Returns Ok after handling every key (nothing reaches the input).
    pub(super) async fn board_key(&mut self, k: KeyEvent) -> Result<()> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.page(Pane::Home) as isize;
        self.notice = None;
        let filtering = self.board.as_ref().is_some_and(|b| b.filtering);
        if !filtering {
            // Tab and shift+tab go round the views; 1, 2, 3 pick one (`view_key`).
            match k.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    let by = if k.code == KeyCode::Tab { 1 } else { -1 };
                    self.set_view(Pane::Home, self.home_view.step(by));
                    return Ok(());
                }
                _ if self.home_view != View::Sessions || matches!(k.code, KeyCode::Char('1'..='4')) => {
                    if !ctrl && self.view_key(Pane::Home, k).await? {
                        return Ok(());
                    }
                    match k.code {
                        KeyCode::Char('n') if !ctrl => self.fresh_session(),
                        KeyCode::Char('q') if !ctrl => self.quit = true,
                        KeyCode::Char('c' | 'd') if ctrl => self.quit = true,
                        _ => {}
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
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
            // The kernel sends archived sessions only when asked.
            KeyCode::Char('a') if !ctrl => {
                b.archived = !b.archived;
                self.rebuild_board();
                self.refresh_board();
            }
            // Archived jobs (a sleep or a vault sync every few minutes) stay out of the way.
            KeyCode::Char('J') if !ctrl => {
                b.jobs = !b.jobs;
                self.rebuild_board();
            }
            KeyCode::Char('r') if !ctrl => self.refresh_board(),
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

    /// A click on the board: a tab of the tab bar, or a row of the view (a session opens).
    pub(super) async fn click_board(&mut self, x: usize, y: usize) -> Result<()> {
        if y == 0 {
            if let Some(v) = x.checked_sub(UnicodeWidthStr::width(HOME_PREFIX)).and_then(View::at) {
                self.set_view(Pane::Home, v);
            }
            return Ok(());
        }
        if self.home_view != View::Sessions {
            // The view starts below the tab bar and a blank row.
            if let Some(row) = y.checked_sub(2) {
                self.view_click(Pane::Home, x, row).await?;
            }
            return Ok(());
        }
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

    /// The board's frame: the tab bar with the sessions' counts, the view (for Sessions: the
    /// filter and the sessions in sections), and the keys at the bottom.
    pub(super) fn board_lines(&mut self) -> Vec<Line> {
        let (w, h) = (self.width(), self.height());
        let view = self.home_view;
        let viewing = self.home_file.is_some();
        let notice = self.notice.clone();
        let prompt = self.name_line(Pane::Home);
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
        let mut head: Line = vec![(HOME_PREFIX.into(), Sty::Bold)];
        head.extend(tab_bar(view));
        let used: usize = head.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum();
        let gap = w.saturating_sub(used + UnicodeWidthStr::width(ask.as_str()) + UnicodeWidthStr::width(right.as_str()) + 1).max(2);
        head.extend([(" ".repeat(gap), Sty::Plain), (ask, Sty::Warn), (right, if b.running > 0 { Sty::Accent } else { Sty::Dim })]);
        let mut out = vec![head, Vec::new()];
        let filtering = view == View::Sessions && (b.filtering || !b.filter.is_empty());
        if filtering {
            let cursor = if b.filtering { "▏" } else { "" };
            out.push(vec![(" filter: ".into(), Sty::Dim), (format!("{}{cursor}", b.filter), Sty::Accent)]);
        }
        let footer: Vec<Line> = {
            let mut f = Vec::new();
            if let Some(e) = &b.error {
                f.push(line(format!(" can't reach zenbot: {e}"), Sty::Err));
            }
            if let Some((t, sty)) = notice {
                f.push(md::linked(&format!(" {t}"), sty));
            }
            let keys = match view {
                View::Sessions if b.filtering => " type to filter · enter keep · esc clear",
                View::Sessions => " ↑↓ choose · enter open · n new session · / filter · a archived · J jobs · r refresh · tab views · q quit",
                View::Files if viewing => " ↑↓ scroll · e web · esc back to the files · tab views · q quit",
                View::Files => " ↑↓ move · →/enter open · ← fold · . hidden · a file · A folder · R rename · tab views · q quit",
                View::Tasks => " ↑↓ choose · a hand to an agent · enter its session · r refresh · n new session · tab views · q quit",
            };
            match prompt {
                Some(p) => f.push(line(format!(" {p}"), Sty::Accent)),
                None => f.push(line(keys, Sty::Dim)),
            }
            f
        };
        let room = h.saturating_sub(out.len() + footer.len()).max(1);
        let body = if view == View::Sessions { b.list_lines(room) } else { self.view_lines(Pane::Home, w, room, true) };
        out.extend(body.into_iter().take(room));
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
    fn channels_are_listed_first_apart_from_sessions_and_jobs() {
        let mut a = app(100, 30);
        let mut all = sample();
        all.push(s("chan", "Matrix", json!({ "kind": "channel" })));
        let mut b = Board { sessions: Some(all), ..Board::default() };
        b.rebuild(epoch_secs(NOW).unwrap(), 0);
        a.board = Some(b);
        a.draw();
        let rows = plain_rows(&a);
        let text = rows.join("\n");
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("no {needle}: {text}"));
        assert!(at("CHANNELS") < at("Matrix") && at("Matrix") < at("SESSIONS") && at("SESSIONS") < at("JOBS"), "{text}");
    }

    #[test]
    fn the_board_shows_sections_running_first_with_subagents_nested() {
        let a = board_app(100, 30);
        let rows = plain_rows(&a);
        let text = rows.join("\n");
        assert!(rows[0].starts_with("zen · Sessions  Files  Tasks") && rows[0].contains("4 sessions · 1 running"), "{text}");
        let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("no {needle}: {text}"));
        assert!(at("SESSIONS") < at("sessions board feature") && at("sessions board feature") < at("garden research"), "running first: {text}");
        assert!(at("sessions board feature") < at("subagent: Read the kernel's session code") && at("subagent: Read") < at("verifier: Check the diff"), "children under their parent, running first, task on one line: {text}");
        assert!(rows[at("sessions board feature")].contains("1/2 subagents working"), "{text}");
        assert!(rows[at("garden research")].contains("· 5m"), "idle sessions say how long ago: {text}");
        assert!(at("garden research") < at("JOBS") && at("JOBS") < at("job: sleep") && at("job: sleep") < at("ARCHIVED"), "jobs apart from mains: {text}");
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
        assert!(at("SESSIONS") < at("pricing questions") && at("pricing questions") < at("sessions board feature"), "waiting before running: {text}");
        assert!(rows[at("pricing questions")].contains("? pricing questions  · waiting on you · claude-opus-5-5 · 2h"), "{text}");
        assert!(!rows[at("garden research")].contains("waiting"), "{text}");
        // Nothing running: the title still says who waits, not "all idle".
        let mut b = Board { sessions: Some(vec![s("asks", "pricing questions", json!({ "waiting": true }))]), ..Board::default() };
        b.rebuild(epoch_secs(NOW).unwrap(), 0);
        a.board = Some(b);
        a.draw();
        let top = &plain_rows(&a)[0];
        assert!(top.contains("? 1 waiting on you · 1 sessions") && !top.contains("idle"), "{top}");
    }

    #[tokio::test]
    async fn the_board_has_a_tab_bar_and_a_file_opens_in_place_of_the_tree() {
        let dir = TempDir::new("home-files");
        dir.file("a.md", "# A\n\nalpha body\n");
        dir.file("b.txt", "bravo\n");
        let mut a = board_app(100, 24);
        a.files = Some(Files::new(dir.path().to_path_buf()));
        // Tab goes round the views; the counts stay on the right.
        key(&mut a, KeyCode::Tab).await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(a.home_view == View::Files && text.contains("a.md") && text.contains("b.txt") && !text.contains("garden"), "{text}");
        assert!(plain_rows(&a)[0].contains("4 sessions"), "{text}");
        assert!(plain_rows(&a).last().unwrap().contains("enter open"), "the keys are the tree's: {text}");
        // Enter opens the file in place of the tree; esc goes back to it, on the same row.
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Up).await;
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(a.home_file.is_some() && text.contains("alpha body") && !text.contains("b.txt"), "{text}");
        key(&mut a, KeyCode::Esc).await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(a.home_file.is_none() && a.files.as_ref().unwrap().sel == 0 && text.contains("b.txt"), "{text}");
        assert!(a.board.is_some() && !a.quit, "still on the board");
        // 3 picks Tasks, shift+tab goes back, 1 picks Sessions.
        key(&mut a, KeyCode::Char('3')).await;
        a.draw();
        assert!(a.home_view == View::Tasks && plain_rows(&a).join("\n").contains("loading…"));
        a.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)).await.unwrap();
        assert!(a.home_view == View::Files);
        key(&mut a, KeyCode::Char('1')).await;
        a.draw();
        assert!(a.home_view == View::Sessions && plain_rows(&a).join("\n").contains("garden"));
        // A click on the tab bar picks a view: "zen ·" then " Sessions " " Files " " Tasks ".
        let click = |column: u16| Event::Mouse(crossterm::event::MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row: 0, modifiers: KeyModifiers::NONE });
        a.on_terminal(click(17)).await.unwrap();
        assert!(a.home_view == View::Files);
        a.on_terminal(click(23)).await.unwrap();
        assert!(a.home_view == View::Tasks);
        // A secret file is neither listed nor opened.
        let home = TempDir::new("home-secrets");
        home.file("token", "s3cret");
        home.file("notes.md", "fine");
        a.files = Some(Files::with_secrets(home.path().to_path_buf(), files::secret_paths(home.path(), &home.join("user"))));
        key(&mut a, KeyCode::Char('2')).await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("notes.md") && !text.contains("token"), "{text}");
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
        assert!(!text.contains("garden") && !text.contains("job: sleep") && !text.contains("subagent: Read"), "{text}");
        key(&mut a, KeyCode::Esc).await;
        a.draw();
        assert!(plain_rows(&a).join("\n").contains("garden"), "esc clears the filter");
        assert!(!a.quit, "letters typed into the filter don't act");
        key(&mut a, KeyCode::Char('q')).await;
        assert!(a.quit);
    }

    #[tokio::test]
    async fn the_new_session_row_starts_a_session_by_enter_or_click_and_hides_while_filtering() {
        let mut a = board_app(100, 30);
        let rows = plain_rows(&a);
        let text = rows.join("\n");
        let at = |rows: &[String], needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("no {needle}: {text}"));
        assert!(at(&rows, "SESSIONS") + 1 == at(&rows, "+ new session") && at(&rows, "+ new session") < at(&rows, "sessions board feature"), "the first row of SESSIONS: {text}");
        assert_eq!(a.board.as_ref().unwrap().selected.as_deref(), Some("busy-main"), "a fresh board still highlights the first session");
        // Up from the first session reaches the button; enter on it is `n`.
        key(&mut a, KeyCode::Up).await;
        a.draw();
        assert_eq!(a.board.as_ref().unwrap().selected.as_deref(), Some(NEW_SESSION));
        assert!(plain_rows(&a).iter().any(|r| r.starts_with("› + new session")), "highlighted like a row");
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        assert!(a.board.is_none() && a.session.is_none() && !a.read_only);
        let chat = plain_rows(&a).join("\n");
        assert!(chat.contains("new session") && chat.contains('╰'), "the chat with its input: {chat}");
        // A click on it does the same.
        let mut b = board_app(100, 30);
        let y = at(&plain_rows(&b), "+ new session");
        b.click_board(5, y).await.unwrap();
        assert!(b.board.is_none() && b.session.is_none(), "a click starts a session");
        // The selection survives a rebuild, and a filter (which looks for sessions) hides it.
        let mut c = board_app(100, 30);
        key(&mut c, KeyCode::Up).await;
        c.rebuild_board();
        assert_eq!(c.board.as_ref().unwrap().selected.as_deref(), Some(NEW_SESSION));
        key(&mut c, KeyCode::Char('/')).await;
        typed(&mut c, "garde").await;
        c.draw();
        let text = plain_rows(&c).join("\n");
        assert!(!text.contains("+ new session") && text.contains("garden"), "{text}");
        assert_eq!(c.board.as_ref().unwrap().selected.as_deref(), Some("idle-main"), "the selection moves to a session");
    }

    #[tokio::test]
    async fn archived_jobs_stay_hidden_until_shift_j_and_the_header_says_so() {
        let mut a = app(100, 30);
        let mut list = sample();
        list.push(s("old-job", "job: vault-sync", json!({ "kind": "job", "archived": true })));
        list.push(s("old-job-2", "job: vault-sync", json!({ "kind": "job", "archived": true })));
        let mut b = Board { sessions: Some(list), ..Board::default() };
        b.rebuild(epoch_secs(NOW).unwrap(), 0);
        a.board = Some(b);
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("ARCHIVED  1  · a shows") && !text.contains("J shows jobs"), "while archived is hidden the header only says a shows: {text}");
        key(&mut a, KeyCode::Char('a')).await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("ARCHIVED  1  · a hides · J shows jobs (2)"), "{text}");
        assert!(text.contains("archived thing") && !text.contains("vault-sync"), "{text}");
        key(&mut a, KeyCode::Char('J')).await;
        a.draw();
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("ARCHIVED  3  · a hides · J hides jobs") && text.contains("vault-sync"), "{text}");
        key(&mut a, KeyCode::Char('J')).await;
        a.draw();
        assert!(!plain_rows(&a).join("\n").contains("vault-sync"), "J again hides them");
        // Plain j still moves down, and the footer names J.
        let before = a.board.as_ref().unwrap().selected.clone();
        key(&mut a, KeyCode::Char('j')).await;
        assert_ne!(a.board.as_ref().unwrap().selected, before);
        assert!(!a.board.as_ref().unwrap().jobs);
        assert!(plain_rows(&a).last().unwrap().contains("a archived · J jobs"));
        // Only archived jobs are hidden; with every archived session a job the section still says so.
        let mut only = Board { sessions: Some(vec![s("j", "job: vault-sync", json!({ "kind": "job", "archived": true }))]), archived: true, ..Board::default() };
        only.rebuild(epoch_secs(NOW).unwrap(), 0);
        let head: String = only.rows.iter().flat_map(|r| r.line.iter().map(|(t, _)| t.as_str())).collect();
        assert!(head.contains("ARCHIVED  0  · a hides · J shows jobs (1)") && !head.contains("vault-sync"), "{head}");
    }

    #[tokio::test]
    async fn files_a_makes_a_file_and_opens_it_capital_a_a_folder_and_capital_r_renames() {
        let dir = TempDir::new("files-make");
        dir.file("src/main.rs", "fn main() {}");
        dir.file("README.md", "readme");
        let mut a = board_app(100, 24);
        a.files = Some(Files::with_secrets(dir.path().to_path_buf(), vec![]));
        key(&mut a, KeyCode::Char('2')).await;
        a.draw();
        assert!(plain_rows(&a).last().unwrap().contains("a file · A folder · R rename"), "{:?}", plain_rows(&a).last());
        // `a` on the folder `src` (the first row): a file inside it; typing doesn't quit or switch views.
        key(&mut a, KeyCode::Char('a')).await;
        typed(&mut a, "quiz1.md").await;
        a.draw();
        let last = plain_rows(&a).last().unwrap().clone();
        assert!(last.contains("new file in ") && last.contains("src: quiz1.md▏"), "the key line asks for the name: {last}");
        assert!(!a.quit && a.home_view == View::Files);
        key(&mut a, KeyCode::Backspace).await;
        typed(&mut a, "d").await;
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        let made = dir.join("src/quiz1.md");
        assert!(made.is_file() && a.name_prompt.is_none(), "created in the selected folder");
        assert!(a.home_file.as_ref().is_some_and(|p| p.path == made), "a new file opens like enter does");
        key(&mut a, KeyCode::Esc).await;
        a.draw();
        let f = a.files.as_ref().unwrap();
        assert_eq!(f.selected().unwrap().path, made, "and is the selected row");
        assert!(plain_rows(&a).join("\n").contains("quiz1.md"));
        // `A` on a file: a folder next to it; a trailing `/` after `a` makes one too.
        key(&mut a, KeyCode::Char('A')).await;
        typed(&mut a, "sub").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(dir.join("src/sub").is_dir() && a.files.as_ref().unwrap().selected().unwrap().path == dir.join("src/sub"));
        key(&mut a, KeyCode::Char('a')).await;
        typed(&mut a, "more/").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(dir.join("src/sub/more").is_dir() && a.home_file.is_none(), "a folder, so no file opens");
        // `R` prefills the name; enter renames and keeps the row selected.
        key(&mut a, KeyCode::Char('R')).await;
        a.draw();
        assert!(plain_rows(&a).last().unwrap().contains("rename more: more▏"), "{:?}", plain_rows(&a).last());
        for _ in 0..4 {
            key(&mut a, KeyCode::Backspace).await;
        }
        typed(&mut a, "less").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(dir.join("src/sub/less").is_dir() && !dir.join("src/sub/more").exists());
        assert_eq!(a.files.as_ref().unwrap().selected().unwrap().path, dir.join("src/sub/less"));
    }

    #[tokio::test]
    async fn the_name_prompt_refuses_bad_names_and_overwrites_and_esc_cancels() {
        let dir = TempDir::new("files-refuse");
        dir.file("keep.md", "mine");
        dir.file("other.md", "theirs");
        let mut a = board_app(100, 24);
        a.files = Some(Files::with_secrets(dir.path().to_path_buf(), vec![]));
        key(&mut a, KeyCode::Char('2')).await;
        for (name, why) in [("..", "not a valid name"), ("a/b", "not a valid name"), ("keep.md", "already exists")] {
            key(&mut a, KeyCode::Char('a')).await;
            typed(&mut a, name).await;
            key(&mut a, KeyCode::Enter).await;
            a.draw();
            let text = plain_rows(&a).join("\n");
            assert!(a.notice.as_ref().is_some_and(|(t, _)| t.contains(why)), "{name}: {:?}", a.notice);
            assert!(text.contains(why) && a.name_prompt.is_some(), "the refusal shows and the prompt stays to fix it: {text}");
            key(&mut a, KeyCode::Esc).await;
            assert!(a.name_prompt.is_none() && a.board.is_some());
        }
        assert_eq!(std::fs::read_to_string(dir.join("keep.md")).unwrap(), "mine", "never overwritten");
        assert!(!dir.join("a").exists() && !dir.join("a/b").exists());
        // Rename onto an existing name is refused too; the files stay as they were.
        a.files.as_mut().unwrap().sel = 0; // keep.md
        key(&mut a, KeyCode::Char('R')).await;
        for _ in 0..7 {
            key(&mut a, KeyCode::Backspace).await;
        }
        typed(&mut a, "other.md").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(a.notice.as_ref().is_some_and(|(t, _)| t.contains("already exists")));
        assert_eq!(std::fs::read_to_string(dir.join("other.md")).unwrap(), "theirs");
        assert!(dir.join("keep.md").exists());
        key(&mut a, KeyCode::Esc).await;
        // A click leaves the prompt too.
        key(&mut a, KeyCode::Char('a')).await;
        assert!(a.name_prompt.is_some());
        a.on_terminal(Event::Mouse(crossterm::event::MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 5, row: 4, modifiers: KeyModifiers::NONE })).await.unwrap();
        assert!(a.name_prompt.is_none());
    }

    #[tokio::test]
    async fn protected_folders_can_t_be_renamed_from_the_files_view() {
        let dir = TempDir::new("files-protected");
        dir.file("zen/global/wiki/page.md", "p");
        let mut a = board_app(100, 24);
        let mut f = Files::with_secrets(dir.path().to_path_buf(), vec![]);
        f.zen_home = dir.join("zen");
        a.files = Some(f);
        key(&mut a, KeyCode::Char('2')).await;
        key(&mut a, KeyCode::Right).await; // open zen
        key(&mut a, KeyCode::Down).await; // zen/global
        key(&mut a, KeyCode::Char('R')).await;
        assert!(a.name_prompt.is_none() && a.notice.as_ref().is_some_and(|(t, _)| t.contains("global: protected")), "{:?}", a.notice);
        assert!(dir.join("zen/global/wiki").is_dir());
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
    async fn new_with_an_agent_starts_its_session_and_a_plain_new_goes_back_to_zenbot() {
        let mut a = app(80, 24);
        typed(&mut a, "/new @Garry office hours").await;
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        assert_eq!(a.agent.as_deref(), Some("garry"));
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("new session with garry") && text.contains("@garry · "), "{text}");
        typed(&mut a, "/new").await;
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.agent, None);
        assert_eq!(agent_tag(Some("zenbot")), "");
        assert_eq!(agent_tag(Some("garry")), "@garry · ");
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

    #[tokio::test]
    async fn the_board_takes_only_the_latest_fetch_and_counts_what_the_kernel_left_out() {
        let mut a = app(100, 30);
        a.open_board();
        assert!(a.board.as_ref().unwrap().loading, "fetched in the background");
        let mine = a.board_fetch;
        let mut list: Vec<Value> = sample().into_iter().filter(|s| s["archived"] == false).collect();
        list[1]["children"] = json!(9);
        a.board_loaded(json!({ "type": "board", "fetch": mine - 1, "board": { "sessions": [], "archived": 0 } }));
        assert!(a.board.as_ref().unwrap().sessions.is_none(), "an older fetch's answer is dropped");
        a.board_loaded(json!({ "type": "board", "fetch": mine, "board": { "sessions": list, "archived": 7 } }));
        let b = a.board.as_ref().unwrap();
        assert!(!b.loading && b.sessions.is_some());
        let text = plain_rows(&a).join("\n");
        assert!(text.contains("ARCHIVED  7  · a shows"), "archived sessions not sent still count: {text}");
        assert!(text.contains("1/9 subagents working (7 older not shown)"), "children not sent still count: {text}");
        // The same answer again changes nothing; an error shows until the kernel answers again.
        a.board.as_mut().unwrap().rows.clear();
        a.board_loaded(json!({ "type": "board", "fetch": mine, "board": { "sessions": list, "archived": 7 } }));
        assert!(a.board.as_ref().unwrap().rows.is_empty(), "not rebuilt");
        a.board_loaded(json!({ "type": "board", "fetch": mine, "error": "connection refused" }));
        assert!(plain_rows(&a).join("\n").contains("can't reach zenbot: connection refused"));
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
