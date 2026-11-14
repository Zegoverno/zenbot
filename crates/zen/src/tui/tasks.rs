//! The Tasks view: the vault's open tasks from `GET /api/tasks`, by project, the most pressing
//! first (the kernel's `tasks.rs` groups and sorts them), each with the agent it was handed to.
//! Both panes share one list, refreshed from the kernel while it's shown. Read-only: zen never
//! edits a task line; the owner ticks it in Obsidian.
//! `a` (pressed twice: a stray key in the side panel mustn't start an agent) hands the chosen task
//! to an agent: the kernel starts a session briefed with it (`POST /api/tasks/{id}/agent`) and
//! keeps the link, and zen goes into that session. Enter (or `a`) on a task that has an agent goes
//! into its session. A task needs its 🆔 to be handed over: that is what the link is keyed by.

use super::*;

/// How often the Tasks view asks the kernel again while it's shown.
pub(super) const TASKS_REFRESH: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct TaskList {
    /// The kernel's last answer (`/api/tasks`); None until the first one.
    pub(super) data: Option<Value>,
    pub(super) error: Option<String>,
    pub(super) loading: bool,
    /// Counts fetches; an answer to an older one is dropped.
    pub(super) fetch: u64,
    /// The chosen task: its place among the tasks, headings not counted.
    pub(super) sel: usize,
    /// First row shown.
    pub(super) top: usize,
    /// `a` was pressed once on this task: a second `a` hands it to an agent.
    pub(super) arming: Option<String>,
}

impl TaskList {
    /// Every task in order, across projects.
    pub(super) fn tasks(&self) -> Vec<&Value> {
        let projects = self.data.as_ref().and_then(|d| d["projects"].as_array());
        projects.into_iter().flatten().flat_map(|p| p["tasks"].as_array().into_iter().flatten()).collect()
    }

    pub(super) fn selected(&self) -> Option<&Value> {
        self.tasks().get(self.sel).copied()
    }

    pub(super) fn step(&mut self, by: isize) {
        let n = self.tasks().len();
        self.sel = (self.sel as isize).saturating_add(by).clamp(0, n.saturating_sub(1) as isize) as usize;
        self.arming = None;
    }

    /// The rows at `w` columns: a heading per project, then its tasks (with the task's index).
    fn rows(&self, w: usize, focus: bool) -> Vec<(Line, Option<usize>)> {
        let mut out = Vec::new();
        let mut i = 0;
        let projects = self.data.as_ref().and_then(|d| d["projects"].as_array());
        for p in projects.into_iter().flatten() {
            let tasks = p["tasks"].as_array().map(Vec::as_slice).unwrap_or_default();
            if !out.is_empty() {
                out.push((Vec::new(), None));
            }
            out.push((vec![(p["project"].as_str().unwrap_or("").to_string(), Sty::Bold), (format!("  {}", tasks.len()), Sty::Dim)], None));
            for t in tasks {
                out.push((task_row(t, w, i == self.sel, focus), Some(i)));
                i += 1;
            }
        }
        out
    }

    /// The view at `w` columns in at most `rows` rows: a summary line, then the rows that fit,
    /// scrolled so the chosen task shows.
    pub(super) fn lines(&mut self, w: usize, rows: usize, focus: bool) -> Vec<Line> {
        let head = match (&self.data, &self.error) {
            (_, Some(e)) => line(format!("can't read the tasks: {e}"), Sty::Err),
            (None, _) => line("loading…", Sty::Dim),
            (Some(d), _) => {
                let n = d["count"].as_u64().unwrap_or(0);
                let handed = self.tasks().iter().filter(|t| t["agent"].is_object()).count();
                let mut l = format!("{n} open task{} · {}", if n == 1 { "" } else { "s" }, d["today"].as_str().unwrap_or(""));
                if handed > 0 {
                    l.push_str(&format!(" · {handed} with an agent"));
                }
                line(l, Sty::Dim)
            }
        };
        let mut out = vec![head];
        if let Some(d) = &self.data {
            for e in d["problems"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                out.push(line(format!("can't read {e}"), Sty::Err));
            }
            if self.tasks().is_empty() {
                out.push(line("No open tasks. They live in the vault: Projects/<project>/tasks.md", Sty::Dim));
                return out;
            }
        }
        self.sel = self.sel.min(self.tasks().len().saturating_sub(1));
        let all = self.rows(w, focus);
        let shown = rows.saturating_sub(out.len()).max(1);
        let at = all.iter().position(|(_, i)| *i == Some(self.sel)).unwrap_or(0);
        // Keep the chosen task in view, with its project's heading when it fits.
        if at < self.top {
            self.top = at.saturating_sub(1);
        } else if at >= self.top + shown {
            self.top = at + 1 - shown;
        }
        self.top = self.top.min(all.len().saturating_sub(shown));
        out.extend(all.into_iter().skip(self.top).take(shown).map(|(l, _)| l));
        out
    }
}

/// One task at `w` columns: the priority, the text (cut to fit), when it's due and its agent.
fn task_row(t: &Value, w: usize, chosen: bool, focus: bool) -> Line {
    let mark = match t["priority"].as_str() {
        Some("urgent") => "🔺",
        Some("important") => "⏫",
        Some("medium") => "🔼",
        Some("not_urgent") => "🔽",
        Some("parking_lot") => "⏬",
        _ => "  ",
    };
    let mut tail: Line = Vec::new();
    if let Some(due) = t["due"].as_str() {
        let overdue = t["overdue"] == true;
        tail.push((format!("  {} {due}", if overdue { "overdue" } else { "due" }), if overdue { Sty::Err } else { Sty::Dim }));
    }
    match t["agent"]["state"].as_str() {
        Some("running") => tail.push(("  ● working".into(), Sty::Accent)),
        Some("waiting") => tail.push(("  ? waiting on you".into(), Sty::Warn)),
        Some(_) => tail.push(("  ✓ reported".into(), Sty::Accent)),
        None => {}
    }
    let lead = if chosen { "› " } else { "  " };
    let used = UnicodeWidthStr::width(lead) + UnicodeWidthStr::width(mark) + 1 + tail.iter().map(|(s, _)| UnicodeWidthStr::width(s.as_str())).sum::<usize>();
    let text = t["text"].as_str().filter(|s| !s.is_empty()).unwrap_or("(no text)");
    let sty = if chosen && focus { Sty::Accent } else if chosen { Sty::Bold } else { Sty::Plain };
    let mut out = vec![(lead.to_string(), sty), (mark.to_string(), Sty::Plain), (" ".into(), Sty::Plain), (cut(text, w.saturating_sub(used).max(8)), sty)];
    out.extend(tail);
    out
}

/// `s` in at most `w` columns, with … when cut.
fn cut(s: &str, w: usize) -> String {
    if UnicodeWidthStr::width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

impl App {
    /// The Tasks view is on screen (the board's, or the side panel's).
    pub(super) fn tasks_shown(&self) -> bool {
        if self.board.is_some() {
            self.home_view == View::Tasks
        } else {
            self.side_open() && self.side_view == View::Tasks && self.columns().1 > 0
        }
    }

    /// Ask the kernel for the tasks, in a task of its own; `tasks_loaded` takes the answer.
    pub(super) fn refresh_tasks(&mut self) {
        self.tasks.loading = true;
        self.tasks.fetch += 1;
        let fetch = self.tasks.fetch;
        let (c, tx) = (self.c.clone(), self.tx.clone());
        tokio::spawn(async move {
            let ev = match c.get("/api/tasks").await {
                Ok(v) => json!({ "type": "tasks", "fetch": fetch, "tasks": v }),
                Err(e) => json!({ "type": "tasks", "fetch": fetch, "error": format!("{e:#}") }),
            };
            let _ = tx.send((String::new(), 0, ev));
        });
    }

    /// The kernel's answer to the latest fetch: draw only if something changed.
    pub(super) fn tasks_loaded(&mut self, ev: Value) {
        let t = &mut self.tasks;
        if ev["fetch"].as_u64() != Some(t.fetch) {
            return;
        }
        t.loading = false;
        let changed = match ev["error"].as_str() {
            Some(e) => t.error.as_deref() != Some(e) && {
                t.error = Some(e.to_string());
                true
            },
            None => {
                let changed = t.error.is_some() || t.data.as_ref() != Some(&ev["tasks"]);
                // Keep the same task chosen when the list moves under it.
                let chosen = t.selected().and_then(|x| x["id"].as_str().map(String::from));
                t.data = Some(ev["tasks"].clone());
                t.error = None;
                if let Some(i) = chosen.and_then(|id| t.tasks().iter().position(|x| x["id"].as_str() == Some(id.as_str()))) {
                    t.sel = i;
                }
                changed
            }
        };
        if changed {
            self.draw();
        }
    }

    /// Say something about the Tasks view where `pane` shows it: the board's footer, or the chat.
    fn tell(&mut self, pane: Pane, text: impl Into<String>, sty: Sty) {
        match pane {
            Pane::Home => self.notice = Some((text.into(), sty)),
            Pane::Side => self.note(text, sty),
        }
    }

    /// Go into session `id` from `pane`.
    async fn go_to(&mut self, pane: Pane, id: String) -> Result<()> {
        match pane {
            Pane::Home => {
                if let Some(b) = &mut self.board {
                    b.selected = Some(id);
                }
                self.open_from_board().await
            }
            Pane::Side => {
                if self.read_only {
                    self.editor.clear();
                }
                self.switch_session(id).await
            }
        }
    }

    /// A key for the Tasks view; false when it has no use for it.
    pub(super) async fn tasks_key(&mut self, pane: Pane, k: KeyEvent, page: usize) -> Result<bool> {
        let arming = self.tasks.arming.take();
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.tasks.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.tasks.step(1),
            KeyCode::PageUp => self.tasks.step(-(page as isize)),
            KeyCode::PageDown => self.tasks.step(page as isize),
            KeyCode::Home => self.tasks.step(isize::MIN / 2),
            KeyCode::End => self.tasks.step(isize::MAX / 2),
            KeyCode::Char('r') => self.refresh_tasks(),
            KeyCode::Enter => {
                let Some(t) = self.tasks.selected().cloned() else { return Ok(true) };
                match t["agent"]["session"].as_str() {
                    Some(id) => self.go_to(pane, id.to_string()).await?,
                    None => self.tell(pane, "no agent on this task yet · a: hand it to one", Sty::Dim),
                }
            }
            KeyCode::Char('a') => {
                let Some(t) = self.tasks.selected().cloned() else { return Ok(true) };
                let text = t["text"].as_str().unwrap_or("").to_string();
                if let Some(id) = t["agent"]["session"].as_str() {
                    return self.go_to(pane, id.to_string()).await.map(|_| true);
                }
                let Some(id) = t["id"].as_str().map(String::from) else {
                    self.tell(pane, "this task has no 🆔: add one in Obsidian to hand it to an agent", Sty::Warn);
                    return Ok(true);
                };
                if arming.as_deref() != Some(id.as_str()) {
                    self.tasks.arming = Some(id);
                    self.tell(pane, format!("a again: hand “{}” to an agent", cut(&text, 50)), Sty::Accent);
                    return Ok(true);
                }
                match self.c.post(&format!("/api/tasks/{id}/agent"), json!({})).await {
                    Ok(v) => {
                        self.refresh_tasks();
                        if let Some(s) = v["session"].as_str() {
                            self.go_to(pane, s.to_string()).await?;
                        }
                    }
                    Err(e) => self.tell(pane, format!("couldn't hand it to an agent: {e:#}"), Sty::Err),
                }
            }
            _ => {
                self.tasks.arming = arming;
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A click on row `row` of the Tasks view (0: its summary line): choose that task; a click
    /// on the chosen task goes into its agent.
    pub(super) async fn tasks_click(&mut self, pane: Pane, row: usize, w: usize) -> Result<()> {
        let rows = self.tasks.rows(w, false);
        let Some(Some(i)) = row.checked_sub(1).and_then(|r| rows.get(self.tasks.top + r)).map(|(_, i)| *i) else { return Ok(()) };
        if i == self.tasks.sel {
            if let Some(id) = self.tasks.selected().and_then(|t| t["agent"]["session"].as_str()).map(String::from) {
                return self.go_to(pane, id).await;
            }
        }
        self.tasks.sel = i;
        self.tasks.arming = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    fn sample() -> Value {
        json!({ "today": "2026-10-28", "count": 3, "problems": [], "projects": [
            { "project": "self", "tasks": [
                { "text": "Pay the water bill", "id": "s9", "priority": "urgent", "due": "2026-10-27", "overdue": true },
                { "text": "Install Obsidian on the Mac", "id": "s001", "due": "2026-10-29", "overdue": false,
                  "agent": { "session": "sess-1", "title": "task: Install", "state": "waiting" } },
            ]},
            { "project": "garden", "tasks": [
                { "text": "Draft the founding cohort message", "priority": "important" },
            ]},
        ]})
    }

    fn tasks_app() -> App {
        let mut a = app(100, 20);
        a.board = Some(Board::default());
        a.home_view = View::Tasks;
        a.tasks.data = Some(sample());
        a.draw();
        a
    }

    #[test]
    fn tasks_show_by_project_with_priority_due_and_agent() {
        let a = tasks_app();
        let rows = plain_rows(&a).join("\n");
        for want in ["3 open tasks · 2026-10-28 · 1 with an agent", "self  2", "› 🔺 Pay the water bill  overdue 2026-10-27", "Install Obsidian on the Mac  due 2026-10-29  ? waiting on you", "garden  1", "⏫ Draft the founding cohort message"] {
            assert!(rows.contains(want), "{want} missing:\n{rows}");
        }
        assert!(rows.find("self").unwrap() < rows.find("garden").unwrap());
    }

    #[test]
    fn a_long_task_is_cut_so_its_due_date_still_shows() {
        let t = json!({ "text": "x".repeat(200), "due": "2026-11-01", "overdue": false });
        let l: String = task_row(&t, 40, false, false).iter().map(|(s, _)| s.as_str()).collect();
        assert!(l.ends_with("…  due 2026-11-01") && width(&l) <= 40, "{l}");
    }

    #[tokio::test]
    async fn keys_move_over_tasks_and_skip_headings() {
        let mut a = tasks_app();
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Down).await;
        assert_eq!(a.tasks.selected().unwrap()["text"], "Draft the founding cohort message");
        key(&mut a, KeyCode::Down).await;
        assert_eq!(a.tasks.sel, 2, "stays on the last task");
        key(&mut a, KeyCode::Home).await;
        assert_eq!(a.tasks.sel, 0);
    }

    #[tokio::test]
    async fn handing_a_task_to_an_agent_takes_a_second_a() {
        let mut a = tasks_app();
        key(&mut a, KeyCode::Char('a')).await;
        assert_eq!(a.tasks.arming.as_deref(), Some("s9"));
        assert!(a.notice.as_ref().is_some_and(|(t, _)| t.contains("a again: hand “Pay the water bill” to an agent")));
        // Any other key disarms it.
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Up).await;
        assert!(a.tasks.arming.is_none());
        // Twice: the kernel is asked (none here, so the failure is said, and zen keeps running).
        key(&mut a, KeyCode::Char('a')).await;
        key(&mut a, KeyCode::Char('a')).await;
        assert!(a.notice.as_ref().is_some_and(|(t, sty)| t.starts_with("couldn't hand it to an agent") && *sty == Sty::Err), "{:?}", a.notice);
        assert!(a.board.is_some(), "still on the board");
    }

    #[tokio::test]
    async fn a_task_without_an_id_cant_be_handed_over() {
        let mut a = tasks_app();
        a.tasks.sel = 2;
        key(&mut a, KeyCode::Char('a')).await;
        assert!(a.notice.as_ref().is_some_and(|(t, _)| t.contains("has no 🆔")));
        assert!(a.tasks.arming.is_none());
    }

    #[test]
    fn an_answer_keeps_the_same_task_chosen() {
        let mut a = tasks_app();
        a.tasks.sel = 1; // s001
        a.tasks.fetch = 3;
        let mut moved = sample();
        moved["projects"][0]["tasks"].as_array_mut().unwrap().remove(0);
        a.tasks_loaded(json!({ "type": "tasks", "fetch": 3, "tasks": moved }));
        assert_eq!(a.tasks.selected().unwrap()["id"], "s001");
        a.tasks_loaded(json!({ "type": "tasks", "fetch": 2, "error": "stale" }));
        assert!(a.tasks.error.is_none(), "an older answer is dropped");
    }
}
