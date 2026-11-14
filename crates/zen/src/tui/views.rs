//! The views a pane shows, with a tab bar on top: Sessions · Files · Tasks. Two panes have them:
//! the home screen (the board, `board.rs`, the whole screen) and the side panel next to a session
//! (`panel.rs`); the same code here draws each view into either and handles its keys and clicks.
//! Sessions is the board's list (`Board`); the side panel's leaves out the session that's open,
//! and enter there switches to the chosen one. Files is the folder tree (`files.rs`), shared by
//! both panes. Tasks is the vault's open tasks (`tasks.rs`), shared by both panes too.
//! The file viewer is not a view: enter on a file in Files shows it in place of the tree, in the
//! same pane (`App::viewer`; `viewer.rs` draws it and has its keys).
//! Keys: on the board, tab and shift+tab go round the views; in either pane, 1 2 3 pick one (in
//! the side panel while it has the keys); a click on the tab bar picks one too.
//! Files also makes things: `a` a new file, `A` a new folder, `R` renames the selected row, each
//! asking for the name on the pane's key line (`NamePrompt`, which takes the keys until enter or
//! esc; the rules are `Files::create` and `Files::rename`).

use super::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum View {
    Sessions,
    Files,
    Tasks,
}

pub(super) const VIEWS: [View; 3] = [View::Sessions, View::Files, View::Tasks];

impl View {
    pub(super) fn name(self) -> &'static str {
        match self {
            View::Sessions => "Sessions",
            View::Files => "Files",
            View::Tasks => "Tasks",
        }
    }

    fn label(self) -> String {
        format!(" {} ", self.name())
    }

    /// The next view (`by` 1) or the previous one (-1), going round.
    pub(super) fn step(self, by: isize) -> View {
        let i = VIEWS.iter().position(|v| *v == self).unwrap_or(0) as isize;
        VIEWS[(i + by).rem_euclid(VIEWS.len() as isize) as usize]
    }

    /// The view whose tab is at column `x` of the tab bar.
    pub(super) fn at(x: usize) -> Option<View> {
        let mut end = 0;
        VIEWS.into_iter().find(|v| {
            end += UnicodeWidthStr::width(v.label().as_str());
            x < end
        })
    }
}

/// The tab bar: every view, the one showing highlighted.
pub(super) fn tab_bar(current: View) -> Line {
    VIEWS.iter().map(|v| (v.label(), if *v == current { Sty::Accent } else { Sty::Dim })).collect()
}

/// What the name being typed is for.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum NameFor {
    File,
    Folder,
    Rename(PathBuf),
}

/// The one-line name prompt of the Files view: what it makes or renames, where, and the text so far.
pub(super) struct NamePrompt {
    pane: Pane,
    what: NameFor,
    /// The folder a new file or folder goes in.
    dir: PathBuf,
    text: String,
    /// The user's home, shown as `~`.
    home: PathBuf,
}

impl NamePrompt {
    pub(super) fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// The prompt as the key line shows it: `new file in ~/x: name▏`.
    pub(super) fn line(&self) -> String {
        let home = self.home.as_path();
        let short = |p: &Path| match p.strip_prefix(home) {
            Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()).trim_end_matches('/').to_string(),
            _ => p.display().to_string(),
        };
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let head = match &self.what {
            NameFor::File => format!("new file in {}", short(&self.dir)),
            NameFor::Folder => format!("new folder in {}", short(&self.dir)),
            NameFor::Rename(from) => format!("rename {}", name(from)),
        };
        format!("{head}: {}▏ (enter ok · esc cancel)", self.text)
    }
}

/// Where a view is shown: the home screen or the side panel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Pane {
    Home,
    Side,
}

impl App {
    pub(super) fn view_of(&self, pane: Pane) -> View {
        match pane {
            Pane::Home => self.home_view,
            Pane::Side => self.side_view,
        }
    }

    /// Show view `v` in `pane`; a list of sessions shown for the first time is fetched.
    pub(super) fn set_view(&mut self, pane: Pane, v: View) {
        match pane {
            Pane::Home => self.home_view = v,
            Pane::Side => self.side_view = v,
        }
        if v == View::Tasks && self.tasks.data.is_none() && !self.tasks.loading {
            self.refresh_tasks();
        }
        if v == View::Sessions {
            self.rebuild_board();
            if self.list().is_some_and(|b| b.sessions.is_none() && !b.loading) {
                self.refresh_board();
            }
        }
    }

    /// The file open in `pane`'s Files view, shown in place of the tree.
    pub(super) fn viewer(&mut self, pane: Pane) -> &mut Option<FileView> {
        match pane {
            Pane::Home => &mut self.home_file,
            Pane::Side => &mut self.side_file,
        }
    }

    /// The list of sessions `pane` shows.
    fn pane_list(&mut self, pane: Pane) -> Option<&mut Board> {
        match pane {
            Pane::Home => self.board.as_mut(),
            Pane::Side => Some(&mut self.side_list),
        }
    }

    /// Rows a page key moves in `pane`.
    pub(super) fn page(&self, pane: Pane) -> usize {
        match pane {
            Pane::Home => self.height().saturating_sub(4),
            Pane::Side => self.viewport_rows().saturating_sub(3),
        }
        .max(1)
    }

    /// `pane`'s view, below its tab bar: at most `rows` rows at `w` columns. `focus`: the pane
    /// has the keys, so the tree's highlight stands out.
    pub(super) fn view_lines(&mut self, pane: Pane, w: usize, rows: usize, focus: bool) -> Vec<Line> {
        match self.view_of(pane) {
            View::Sessions => {
                if pane == Pane::Side && self.side_list.hide != self.session {
                    self.rebuild_board();
                }
                self.pane_list(pane).map(|b| b.list_lines(rows)).unwrap_or_default()
            }
            View::Files => {
                let look = self.reading;
                match self.viewer(pane) {
                    Some(p) => file_lines(p, w, rows, look),
                    None => self.tree_lines(rows, focus),
                }
            }
            View::Tasks => self.tasks.lines(w, rows, focus),
        }
    }

    /// The folder's path, then as much of the tree as fits.
    fn tree_lines(&mut self, rows: usize, focus: bool) -> Vec<Line> {
        let f = self.files();
        let shown = rows.saturating_sub(1);
        f.follow(shown);
        let mut out = vec![vec![(f.root.to_string_lossy().into_owned(), Sty::Dim)]];
        for (i, r) in f.rows.iter().enumerate().skip(f.scroll).take(shown) {
            let name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mark = if !r.dir { "  " } else if f.is_open(r) { "▾ " } else { "▸ " };
            let text = format!("{}{mark}{name}{}", "  ".repeat(r.depth), if r.dir { "/" } else { "" });
            let sty = if i == f.sel && focus { Sty::Accent } else if i == f.sel { Sty::Bold } else if r.dir { Sty::Plain } else { Sty::Dim };
            out.push(vec![(if i == f.sel { "› " } else { "  " }.to_string(), sty), (text, sty)]);
        }
        out
    }

    /// Open or fold the selected row of the tree, as enter and a click do; a file opens in
    /// `pane`, in place of the tree.
    pub(super) fn activate_row(&mut self, pane: Pane) {
        let Some(f) = &mut self.files else { return };
        let Some(r) = f.selected() else { return };
        if r.dir {
            if !f.expand() {
                f.collapse_or_parent();
            }
        } else {
            let p = r.path.clone();
            self.open_file(pane, p);
        }
    }

    /// Ask for the name of a new file or folder (in the selected folder, else the selected file's
    /// folder, else the root) or of the selected row's new name (`NameFor::Rename`, whose path is
    /// filled in here).
    fn name_start(&mut self, pane: Pane, what: NameFor) {
        let f = self.files();
        let dir = f.target_dir();
        let (what, text) = match what {
            NameFor::Rename(_) => {
                let Some(r) = f.selected() else { return };
                if f.protected(&r.path) {
                    let name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    return self.note(format!("{name}: protected"), Sty::Warn);
                }
                (NameFor::Rename(r.path.clone()), r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            }
            other => (other, String::new()),
        };
        let home = f.home.clone();
        self.name_prompt = Some(NamePrompt { pane, what, dir, text, home });
    }

    /// The name prompt as `pane`'s key line shows it, if it is open there.
    pub(super) fn name_line(&self, pane: Pane) -> Option<String> {
        self.name_prompt.as_ref().filter(|p| p.pane == pane).map(NamePrompt::line)
    }

    /// A key while the name prompt is open: it takes every key. Enter makes or renames (a refusal
    /// shows and keeps the prompt, so the name can be fixed), esc cancels.
    pub(super) fn name_key(&mut self, k: KeyEvent) {
        let Some(p) = &mut self.name_prompt else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.notice = None;
        match k.code {
            KeyCode::Esc => self.name_prompt = None,
            KeyCode::Char('c' | 'd') if ctrl => self.name_prompt = None,
            KeyCode::Char('u') if ctrl => p.text.clear(),
            KeyCode::Backspace => {
                p.text.pop();
            }
            KeyCode::Char(c) if !ctrl && !c.is_control() => p.text.push(c),
            KeyCode::Enter => self.name_done(),
            _ => {}
        }
    }

    fn name_done(&mut self) {
        let Some(p) = self.name_prompt.take() else { return };
        let text = p.text.trim();
        let f = self.files();
        let made = match &p.what {
            // A name ending in `/` is a folder, whichever key asked.
            NameFor::File | NameFor::Folder => {
                let folder = p.what == NameFor::Folder || text.ends_with('/');
                f.create(&p.dir, text.trim_end_matches('/'), folder).map(|path| (path, !folder))
            }
            NameFor::Rename(from) => f.rename(from, text).map(|path| (path, false)),
        };
        match made {
            // A new file opens as enter on it would.
            Ok((path, true)) => self.open_file(p.pane, path),
            Ok(_) => {}
            Err(e) => {
                self.note(e, Sty::Err);
                self.name_prompt = Some(p);
            }
        }
    }

    /// What enter or a click on a row of the side panel's Sessions view does: the `+ new session`
    /// button starts a session, any other row switches to it.
    async fn open_from_side(&mut self, id: String) -> Result<()> {
        if id == NEW_SESSION {
            self.fresh_session();
            return Ok(());
        }
        self.switch_from_side(id).await
    }

    /// Switch to session `id`, chosen in the side panel's Sessions view.
    async fn switch_from_side(&mut self, id: String) -> Result<()> {
        // Text typed in a read-only session can't have been meant for the next one.
        if self.read_only {
            self.editor.clear();
        }
        self.switch_session(id).await
    }

    /// A key for `pane`'s view; false when the view has no use for it (the caller decides).
    pub(super) async fn view_key(&mut self, pane: Pane, k: KeyEvent) -> Result<bool> {
        if let KeyCode::Char(c @ '1'..='3') = k.code {
            self.set_view(pane, VIEWS[c as usize - '1' as usize]);
            return Ok(true);
        }
        let page = self.page(pane);
        match self.view_of(pane) {
            View::Files if self.viewer(pane).is_some() => return Ok(self.viewer_key(pane, k, page)),
            View::Files => {
                let f = self.files();
                match k.code {
                    KeyCode::Up => f.move_by(-1),
                    KeyCode::Down => f.move_by(1),
                    KeyCode::PageUp => f.move_by(-(page as isize)),
                    KeyCode::PageDown => f.move_by(page as isize),
                    KeyCode::Home => f.sel = 0,
                    KeyCode::End => f.sel = f.rows.len().saturating_sub(1),
                    KeyCode::Right => {
                        if !f.expand() && f.selected().is_some_and(|r| !r.dir) {
                            self.activate_row(pane);
                        }
                    }
                    KeyCode::Enter => self.activate_row(pane),
                    KeyCode::Left => f.collapse_or_parent(),
                    KeyCode::Char('.') => f.toggle_hidden(),
                    KeyCode::Char('r') => f.rebuild(),
                    KeyCode::Char('a') => self.name_start(pane, NameFor::File),
                    KeyCode::Char('A') => self.name_start(pane, NameFor::Folder),
                    KeyCode::Char('R') => self.name_start(pane, NameFor::Rename(PathBuf::new())),
                    _ => return Ok(false),
                }
            }
            // The board drives its own list (`board_key`), filter and all.
            View::Sessions if pane == Pane::Home => return Ok(false),
            View::Sessions => {
                let b = &mut self.side_list;
                match k.code {
                    KeyCode::Up => b.step(-1),
                    KeyCode::Down => b.step(1),
                    KeyCode::PageUp => b.step(-(page as isize)),
                    KeyCode::PageDown => b.step(page as isize),
                    KeyCode::Home => b.step(isize::MIN / 2),
                    KeyCode::End => b.step(isize::MAX / 2),
                    KeyCode::Enter => {
                        if let Some(id) = b.selected.clone() {
                            self.open_from_side(id).await?;
                        }
                    }
                    KeyCode::Char('a') => {
                        b.archived = !b.archived;
                        self.rebuild_board();
                        self.refresh_board();
                    }
                    KeyCode::Char('J') => {
                        b.jobs = !b.jobs;
                        self.rebuild_board();
                    }
                    KeyCode::Char('r') => self.refresh_board(),
                    _ => return Ok(false),
                }
            }
            View::Tasks => return self.tasks_key(pane, k, page).await,
        }
        Ok(true)
    }

    /// A click on row `row` of `pane`'s view (0: the first row below the tab bar).
    pub(super) async fn view_click(&mut self, pane: Pane, x: usize, row: usize) -> Result<()> {
        match self.view_of(pane) {
            // A click on a link follows it (ctrl+click too, where the terminal lets it through).
            View::Files if self.viewer(pane).as_ref().is_some_and(|p| p.rsvp.is_none()) => self.viewer_click(pane, x, row),
            View::Files if self.viewer(pane).is_none() => {
                let Some(f) = &mut self.files else { return Ok(()) };
                let idx = f.scroll + row.saturating_sub(1); // row 0 is the folder's path
                if row >= 1 && idx < f.rows.len() {
                    f.sel = idx;
                    self.activate_row(pane);
                }
            }
            View::Sessions => {
                let Some(b) = self.pane_list(pane) else { return Ok(()) };
                let Some(id) = b.rows.get(b.top + row).and_then(|r| r.id.clone()) else { return Ok(()) };
                b.selected = Some(id.clone());
                match pane {
                    Pane::Home => self.open_from_board().await?,
                    Pane::Side => self.open_from_side(id).await?,
                }
            }
            View::Tasks => {
                let w = match pane {
                    Pane::Home => self.width(),
                    Pane::Side => self.columns().1,
                };
                self.tasks_click(pane, row, w).await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The wheel (or alt+↑↓) over `pane`: scroll the open file, or move through the list.
    pub(super) fn view_scroll(&mut self, pane: Pane, up: bool, n: usize) {
        let by = if up { -(n.min(1000) as isize) } else { n.min(1000) as isize };
        match self.view_of(pane) {
            View::Files => {
                if let Some(p) = self.viewer(pane).as_mut().filter(|p| p.show_log && p.rsvp.is_none()) {
                    // The changelog shown has its own scroll.
                    p.log_scroll = if up { p.log_scroll.saturating_sub(n) } else { p.log_scroll.saturating_add(n) };
                } else if let Some(p) = self.viewer(pane) {
                    p.scroll = if up { p.scroll.saturating_sub(n) } else { p.scroll.saturating_add(n) };
                } else if let Some(f) = &mut self.files {
                    f.move_by(by);
                }
            }
            View::Sessions => {
                if let Some(b) = self.pane_list(pane) {
                    b.step(by);
                }
            }
            View::Tasks => self.tasks.step(by),
        }
    }

    /// A key while the side panel has the keys; false lets it through to the input.
    pub(super) async fn side_key(&mut self, k: KeyEvent) -> Result<bool> {
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return Ok(false);
        }
        if self.view_key(Pane::Side, k).await? {
            return Ok(true);
        }
        Ok(match k.code {
            KeyCode::Esc => {
                self.side_focus = false;
                true
            }
            KeyCode::Char(_) => {
                self.side_focus = false; // typing goes to the input
                false
            }
            _ => true,
        })
    }

    /// A click at (`x`, `row`) inside the side panel: a tab, or a row of its view.
    pub(super) async fn click_side(&mut self, x: usize, row: usize) -> Result<()> {
        self.side_focus = true;
        if row == 0 {
            if let Some(v) = View::at(x) {
                self.set_view(Pane::Side, v);
            }
            return Ok(());
        }
        self.view_click(Pane::Side, x, row - 1).await
    }

    /// The side panel's rows (`vh` of them) at `pw` columns: the tab bar, the view, the keys.
    pub(super) fn side_lines(&mut self, pw: usize, vh: usize) -> Vec<Line> {
        let mut bar = tab_bar(self.side_view);
        if self.side_focus {
            bar.push(("  ●".to_string(), Sty::Accent));
        }
        let mut out = vec![bar];
        let body = vh.saturating_sub(2); // below the tab bar, above the hint row
        let focus = self.side_focus;
        out.extend(self.view_lines(Pane::Side, pw, body, focus));
        if vh >= 3 {
            out.truncate(vh - 1);
            while out.len() < vh - 1 {
                out.push(Vec::new());
            }
            let prompt = self.name_line(Pane::Side);
            let hint = match (self.side_focus, self.side_view) {
                _ if prompt.is_some() => prompt.as_deref().unwrap_or_default(),
                (false, _) => "shift+tab: use this panel · ctrl+b: close",
                (true, View::Files) if self.side_file.is_some() => "↑↓ scroll · e zenwrite · esc/← back to the files · tab chat",
                (true, View::Files) => "↑↓ move · →/enter open · ← fold · . hidden · a file · A folder · R rename · esc/tab chat",
                (true, View::Sessions) => "↑↓ choose · enter switch · a archived · J jobs · 1 2 3 views · esc/tab chat",
                (true, View::Tasks) => "↑↓ choose · a hand to an agent · enter its session · 1 2 3 views · esc/tab chat",
            };
            out.push(vec![(hint.to_string(), Sty::Dim)]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    fn s(id: &str, title: &str, parent: Option<&str>) -> Value {
        json!({ "id": id, "title": title, "model": "claude/claude-opus-5-5", "archived": false, "kind": if parent.is_some() { json!("subagent") } else { Value::Null }, "parent": parent, "busy": false, "updated_at": "2026-10-09T11:00:00Z" })
    }

    /// The side panel's rows (the chat's side cut off).
    fn side(a: &App) -> String {
        plain_rows(a).iter().filter_map(|r| r.split_once(" │ ").map(|(_, p)| p.to_string())).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn views_go_round_and_tabs_are_found_by_column() {
        assert_eq!(View::Sessions.step(1), View::Files);
        assert_eq!(View::Sessions.step(-1), View::Tasks);
        assert_eq!(View::Tasks.step(1), View::Sessions);
        let bar: String = tab_bar(View::Files).iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(bar, " Sessions  Files  Tasks ");
        assert_eq!((View::at(0), View::at(9), View::at(10), View::at(16), View::at(17), View::at(23), View::at(24)), (Some(View::Sessions), Some(View::Sessions), Some(View::Files), Some(View::Files), Some(View::Tasks), Some(View::Tasks), None));
    }

    #[tokio::test]
    async fn the_side_panel_has_the_same_tabs_and_its_sessions_leave_out_the_open_one() {
        let mut a = app(120, 24);
        a.session = Some("open-one".into());
        a.side_list.sessions = Some(vec![s("open-one", "the session I'm in", None), s("kid", "subagent: x", Some("open-one")), s("other", "garden plan", None), s("third", "recipes api", None)]);
        a.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)).await.unwrap();
        a.draw();
        let text = side(&a);
        assert!(a.side_view == View::Files && text.starts_with(" Sessions  Files  Tasks"), "ctrl+b opens on Files, under the tab bar: {text}");
        // 1 picks Sessions: every session but the one open (and its subagents).
        key(&mut a, KeyCode::Char('1')).await;
        a.draw();
        let text = side(&a);
        assert!(a.side_view == View::Sessions && text.contains("garden plan") && text.contains("recipes api"), "{text}");
        assert!(!text.contains("the session I'm in") && !text.contains("subagent: x"), "{text}");
        assert!(text.contains("enter switch"), "{text}");
        // The highlight moves; enter switches to the chosen session (here the kernel is away).
        assert_eq!(a.side_list.selected.as_deref(), Some("other"));
        key(&mut a, KeyCode::Down).await;
        assert_eq!(a.side_list.selected.as_deref(), Some("third"));
        let err = a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
        assert!(err.is_err(), "it asked the kernel for the session");
        assert_eq!(a.session.as_deref(), Some("open-one"), "a failed switch stays put");
        // 3 shows Tasks, 2 back to Files; typing a letter hands the keys back to the chat.
        key(&mut a, KeyCode::Char('3')).await;
        a.draw();
        assert!(a.side_view == View::Tasks && side(&a).contains("loading…"));
        key(&mut a, KeyCode::Char('2')).await;
        assert!(a.side_view == View::Files);
        typed(&mut a, "hi").await;
        assert!(!a.side_focus && a.editor.buf == "hi");
        // The list follows the session that's open.
        a.side_view = View::Sessions;
        a.session = Some("other".into());
        a.draw();
        let text = side(&a);
        assert!(text.contains("the session I'm in") && !text.contains("garden plan"), "{text}");
    }

    #[tokio::test]
    async fn files_lands_on_the_files_view_and_open_shows_a_file_in_place_of_the_tree() {
        let dir = TempDir::new("side-open");
        let path = dir.file("notes.md", "# Notes\n\nnote body\n");
        dir.file("other.txt", "x");
        let mut a = app(120, 24);
        a.files = Some(Files::new(dir.path().to_path_buf()));
        a.side = true;
        a.side_view = View::Tasks;
        a.command("/files").await.unwrap();
        assert!(a.side_open() && a.side_view == View::Files && a.side_focus, "/files switches to Files when the panel shows another view");
        a.command("/files").await.unwrap();
        assert!(!a.side_open(), "and closes it when it already shows Files");
        // /open shows the file in the side panel's Files view; esc goes back to the tree.
        a.command(&format!("/open {}", path.display())).await.unwrap();
        a.draw();
        let text = side(&a);
        assert!(a.side_view == View::Files && text.contains("note body") && !text.contains("other.txt"), "{text}");
        a.side_focus = true;
        key(&mut a, KeyCode::Backspace).await;
        a.draw();
        let text = side(&a);
        assert!(a.side_file.is_none() && a.side_open() && text.contains("other.txt"), "the tree is back: {text}");
        key(&mut a, KeyCode::Esc).await;
        assert!(!a.side_focus && a.side_open(), "esc on the tree hands the keys to the chat");
    }

    #[tokio::test]
    async fn the_side_panel_has_the_new_session_row_and_the_files_name_prompt() {
        let mut a = app(120, 24);
        a.session = Some("open-one".into());
        a.side_list.sessions = Some(vec![s("open-one", "the session I'm in", None), s("other", "garden plan", None)]);
        let dir = TempDir::new("side-make");
        dir.file("notes.md", "n");
        a.files = Some(Files::with_secrets(dir.path().to_path_buf(), vec![]));
        a.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)).await.unwrap();
        // Files: `a` asks for a name on the panel's key line and makes the file, opened in the panel.
        key(&mut a, KeyCode::Char('a')).await;
        a.draw();
        assert!(a.side_focus && side(&a).contains("new file in ") && a.editor.buf.is_empty(), "{}", side(&a));
        typed(&mut a, "todo.md").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(dir.join("todo.md").is_file() && a.side_file.as_ref().is_some_and(|p| p.path == dir.join("todo.md")));
        // Sessions: the first row is the button; enter on it starts a session.
        key(&mut a, KeyCode::Esc).await; // back from the file to the tree
        assert!(a.side_focus && a.side_file.is_none());
        key(&mut a, KeyCode::Char('1')).await;
        a.draw();
        assert!(side(&a).contains("+ new session") && a.side_list.selected.as_deref() == Some("other"), "{}", side(&a));
        key(&mut a, KeyCode::Up).await;
        assert_eq!(a.side_list.selected.as_deref(), Some(NEW_SESSION));
        key(&mut a, KeyCode::Enter).await;
        assert!(a.session.is_none() && a.board.is_none(), "a fresh chat");
    }
}
