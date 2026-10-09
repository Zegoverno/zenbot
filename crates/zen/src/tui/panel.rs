//! The side panel next to the chat (full screen): the Files tab (`files.rs`) and the Viewer tab
//! with an open file, reloaded when it changes; `/open`, `/files`, `/close`, ctrl+b.

use super::*;

/// Which tab of the side panel is showing.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Tab {
    Files,
    Viewer,
}

/// Narrowest terminal (columns) that fits the chat and the side panel next to each other.
pub(super) const SPLIT_MIN: usize = 60;

/// Largest file the side panel loads.
pub(super) const PANEL_MAX_BYTES: u64 = 2 << 20;

/// A file shown next to the chat.
pub(super) struct Panel {
    pub(super) path: PathBuf,
    pub(super) text: String,
    pub(super) modified: Option<SystemTime>,
    /// First line shown.
    pub(super) scroll: usize,
    /// Rendered lines, and the width they were rendered at.
    pub(super) lines: Vec<Line>,
    pub(super) lines_w: usize,
}

impl Panel {
    pub(super) fn open(path: PathBuf) -> std::result::Result<Panel, String> {
        let mut p = Panel { path, text: String::new(), modified: None, scroll: 0, lines: Vec::new(), lines_w: 0 };
        p.load()?;
        Ok(p)
    }

    pub(super) fn load(&mut self) -> std::result::Result<(), String> {
        let meta = std::fs::metadata(&self.path).map_err(|e| format!("can't open {}: {e}", self.path.display()))?;
        if !meta.is_file() {
            return Err(format!("{} is not a file", self.path.display()));
        }
        if meta.len() > PANEL_MAX_BYTES {
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

    pub(super) fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.display().to_string())
    }

    /// The file's lines at `width` columns: markdown files rendered, others wrapped as they are.
    pub(super) fn lines(&mut self, width: usize) -> &[Line] {
        if self.lines_w != width {
            let is_md = self.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
            self.lines = if is_md {
                Md::default().render(&self.text, width)
            } else {
                let none = || (String::new(), Sty::Plain);
                self.text.split('\n').flat_map(|l| md::wrap(vec![(l.to_string(), Sty::Plain)], width, none(), none())).collect()
            };
            self.lines_w = width;
        }
        &self.lines
    }
}

/// A path the owner typed, with a leading `~` meaning the home folder.
pub(super) fn expand_home(raw: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    match raw.strip_prefix("~/") {
        Some(rest) => PathBuf::from(&home).join(rest),
        None if raw == "~" => PathBuf::from(&home),
        None => PathBuf::from(raw),
    }
}

impl App {
    pub(super) fn side_open(&self) -> bool {
        self.side || self.panel.is_some()
    }

    /// The side panel shows only in full screen: say so inline. True when it can show.
    pub(super) fn side_allowed(&mut self) -> bool {
        if self.inline {
            self.note("the side panel needs full screen: run zen without --inline", Sty::Warn);
        }
        !self.inline
    }

    /// The side panel is about to open: warn when the terminal is too narrow to show it.
    pub(super) fn warn_if_narrow(&mut self) {
        if !self.side_open() && self.width() < SPLIT_MIN {
            self.note(format!("the terminal is too narrow for the side panel (needs {SPLIT_MIN} columns)"), Sty::Warn);
        }
    }

    pub(super) fn close_side(&mut self) {
        self.side = false;
        self.panel = None;
        self.side_focus = false;
    }

    /// Open the side panel on the Files tab with the keys, or close it (and the file in it).
    pub(super) fn toggle_side(&mut self) {
        if !self.side_allowed() {
            return;
        }
        if self.side_open() {
            self.close_side();
        } else {
            self.warn_if_narrow();
            self.side = true;
            self.tab = Tab::Files;
            self.side_focus = true;
        }
    }

    /// Show a file in the Viewer tab.
    pub(super) fn open_file(&mut self, path: PathBuf) {
        match Panel::open(path) {
            Ok(p) => {
                self.warn_if_narrow();
                self.panel = Some(p);
                self.tab = Tab::Viewer;
            }
            Err(e) => self.note(e, Sty::Err),
        }
    }

    /// The folder tree, listed the first time it's needed.
    pub(super) fn files(&mut self) -> &mut Files {
        self.files.get_or_insert_with(|| Files::new(self.files_root.clone()))
    }

    /// The tab showing: the Viewer only while a file is open.
    pub(super) fn current_tab(&mut self) -> Tab {
        if self.tab == Tab::Viewer && self.panel.is_none() {
            self.tab = Tab::Files;
        }
        self.tab
    }

    /// Open or fold the selected row of the tree, as Enter and a click do.
    pub(super) fn activate_row(&mut self) {
        let Some(f) = &mut self.files else { return };
        let Some(r) = f.selected() else { return };
        if r.dir {
            if !f.expand() {
                f.collapse_or_parent();
            }
        } else {
            let p = r.path.clone();
            self.open_file(p);
        }
    }

    /// A key while the side panel has the keys; false lets it through to the input.
    pub(super) fn side_key(&mut self, k: KeyEvent) -> bool {
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        if k.code == KeyCode::Esc {
            self.side_focus = false;
            return true;
        }
        let page = self.viewport_rows().saturating_sub(3).max(1) as isize;
        match self.current_tab() {
            Tab::Files => {
                let f = self.files();
                match k.code {
                    KeyCode::Up => f.move_by(-1),
                    KeyCode::Down => f.move_by(1),
                    KeyCode::PageUp => f.move_by(-page),
                    KeyCode::PageDown => f.move_by(page),
                    KeyCode::Home => f.sel = 0,
                    KeyCode::End => f.sel = f.rows.len().saturating_sub(1),
                    KeyCode::Right => {
                        if !f.expand() && f.selected().is_some_and(|r| !r.dir) {
                            self.activate_row();
                        }
                    }
                    KeyCode::Enter => self.activate_row(),
                    KeyCode::Left => f.collapse_or_parent(),
                    KeyCode::Char('.') => f.toggle_hidden(),
                    KeyCode::Char('r') => f.rebuild(),
                    KeyCode::Char(_) => {
                        self.side_focus = false; // typing goes to the input
                        return false;
                    }
                    _ => {}
                }
            }
            Tab::Viewer => match k.code {
                KeyCode::Up => self.scroll_panel(true, 1),
                KeyCode::Down => self.scroll_panel(false, 1),
                KeyCode::PageUp => self.scroll_panel(true, page as usize),
                KeyCode::PageDown => self.scroll_panel(false, page as usize),
                KeyCode::Home => self.scroll_panel(true, usize::MAX / 2),
                KeyCode::Left | KeyCode::Backspace => self.tab = Tab::Files,
                KeyCode::Char('x') => {
                    self.panel = None;
                    self.tab = Tab::Files;
                }
                KeyCode::Char(_) => {
                    self.side_focus = false;
                    return false;
                }
                _ => {}
            },
        }
        true
    }

    /// A click at (`x`, `row`) inside the side panel: a tab, or a row of the tree.
    pub(super) fn click_side(&mut self, x: usize, row: usize) {
        if row == 0 {
            if x < 7 {
                self.tab = Tab::Files;
            } else if self.panel.is_some() {
                self.tab = Tab::Viewer;
            }
            self.side_focus = true;
            return;
        }
        if self.tab != Tab::Files {
            self.side_focus = true;
            return;
        }
        if let Some(f) = &mut self.files {
            let idx = f.scroll + row.saturating_sub(2); // row 1 is the folder's path
            if row >= 2 && idx < f.rows.len() {
                f.sel = idx;
                self.side_focus = true;
                self.activate_row();
            }
        }
    }

    /// The tab strip: Files, and the open file when there is one.
    pub(super) fn tab_bar(&self) -> Line {
        let style = |on: bool| if on { Sty::Accent } else { Sty::Dim };
        let mut bar = vec![(" Files ".to_string(), style(self.tab == Tab::Files))];
        if let Some(p) = &self.panel {
            bar.push((format!(" {} ", p.name()), style(self.tab == Tab::Viewer)));
        }
        if self.side_focus {
            bar.push(("  ●".to_string(), Sty::Accent));
        }
        bar
    }

    /// The side panel's rows (`vh` of them) at `pw` columns.
    pub(super) fn side_lines(&mut self, pw: usize, vh: usize) -> Vec<Line> {
        let tab = self.current_tab();
        let mut out = vec![self.tab_bar()];
        let body = vh.saturating_sub(2); // below the tab strip, above the hint row
        match tab {
            Tab::Files => {
                let focus = self.side_focus;
                let f = self.files();
                f.follow(body);
                let root = f.root.to_string_lossy().into_owned();
                out.push(vec![(root, Sty::Dim)]);
                for (i, r) in f.rows.iter().enumerate().skip(f.scroll).take(body.saturating_sub(1)) {
                    let name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let mark = if !r.dir { "  " } else if f.is_open(r) { "▾ " } else { "▸ " };
                    let text = format!("{}{mark}{name}{}", "  ".repeat(r.depth), if r.dir { "/" } else { "" });
                    let sty = if i == f.sel && focus { Sty::Accent } else if i == f.sel { Sty::Bold } else if r.dir { Sty::Plain } else { Sty::Dim };
                    out.push(vec![(if i == f.sel { "› " } else { "  " }.to_string(), sty), (text, sty)]);
                }
            }
            Tab::Viewer => {
                if let Some(p) = &mut self.panel {
                    let rows = body.saturating_sub(1);
                    let n = p.lines(pw).len();
                    p.scroll = p.scroll.min(n.saturating_sub(rows));
                    let s = p.scroll;
                    let pos = if n > rows { format!(" · {}-{} of {n}", s + 1, (s + rows).min(n)) } else { String::new() };
                    out.push(vec![(p.path.display().to_string(), Sty::Bold), (pos, Sty::Dim)]);
                    out.extend(p.lines(pw).iter().skip(s).take(rows).cloned());
                }
            }
        }
        if vh >= 3 {
            out.truncate(vh - 1);
            while out.len() < vh - 1 {
                out.push(Vec::new());
            }
            let hint = match (self.side_focus, self.tab) {
                (false, _) => "shift+tab: use this panel · ctrl+b: close",
                (true, Tab::Files) => "↑↓ move · →/enter open · ← fold · . hidden · esc/tab chat",
                (true, Tab::Viewer) => "↑↓ scroll · ← files · x close file · esc/tab chat",
            };
            out.push(vec![(hint.to_string(), Sty::Dim)]);
        }
        out
    }

    /// The pointer at column `x`, row `y` is over the side panel (not the divider between it and
    /// the chat, nor the live region below).
    pub(super) fn in_panel(&self, x: usize, y: usize) -> bool {
        let (cw, pw) = self.columns();
        pw > 0 && x >= cw + 3 && y < self.viewport_rows()
    }

    pub(super) fn scroll_panel(&mut self, up: bool, n: usize) {
        if self.tab == Tab::Files {
            if let Some(f) = &mut self.files {
                f.move_by(if up { -(n.min(1000) as isize) } else { n.min(1000) as isize });
            }
            return;
        }
        if let Some(p) = &mut self.panel {
            p.scroll = if up { p.scroll.saturating_sub(n) } else { p.scroll + n };
        }
    }

    /// `/open [path]`: show a file next to the chat; with no path, the last file a tool touched.
    pub(super) fn open_panel(&mut self, arg: &str) {
        if !self.side_allowed() {
            return;
        }
        let Some(raw) = (if arg.is_empty() { self.last_file.clone() } else { Some(arg.to_string()) }) else {
            self.note("usage: /open <path> (no file touched yet in this session)", Sty::Warn);
            return;
        };
        self.open_file(expand_home(&raw));
    }

    /// `/files <dir>`: root the folder tree at `dir` and show it.
    pub(super) fn show_folder(&mut self, arg: &str) {
        if !self.side_allowed() {
            return;
        }
        let dir = expand_home(arg);
        if !dir.is_dir() {
            self.note(format!("not a folder: {arg}"), Sty::Err);
            return;
        }
        self.files_root = dir.clone();
        self.files = Some(Files::new(dir));
        if !self.side_open() {
            self.toggle_side();
        }
        self.tab = Tab::Files;
        self.side_focus = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    const DIVIDER: &str = "\x1b[2m │ \x1b[0m";

    #[tokio::test]
    async fn open_shows_a_file_beside_the_chat_and_close_hides_it() {
        let mut a = app(100, 20);
        let dir = TempDir::new("panel");
        let path = dir.file("USER.md", "# Who I am\n\nJose, a builder.\n");
        a.commit(vec![line("chat text", Sty::Plain)]);
        a.command(&format!("/open {}", path.display())).await.unwrap();
        a.draw();
        let rows = a.screen.rows().to_vec();
        assert!(rows[0].contains("chat text") && rows[0].contains("USER.md"), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains(DIVIDER) && r.contains("Jose, a builder.")), "{rows:#?}");
        assert_eq!(a.columns(), (48, 48));
        // It follows the file as it changes.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, "# Who I am\n\nJose (Ze), a builder.\n").unwrap();
        let later = SystemTime::now() + Duration::from_secs(1);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        assert!(a.panel.as_mut().unwrap().reload_if_changed());
        a.draw();
        assert!(a.screen.rows().iter().any(|r| r.contains("Jose (Ze), a builder.")));
        a.command("/close").await.unwrap();
        a.draw();
        assert!(a.screen.rows().iter().all(|r| !r.contains(DIVIDER)));
        assert_eq!(a.columns(), (99, 0));
    }

    #[tokio::test]
    async fn open_with_no_path_shows_the_last_file_a_tool_touched() {
        let mut a = app(100, 20);
        a.command("/open").await.unwrap();
        assert!(a.panel.is_none() && a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("usage: /open")));
        let dir = TempDir::new("last-file");
        let path = dir.file("notes.txt", "first line\nsecond line\n");
        a.busy = true;
        a.on_event(json!({ "type": "tool_start", "name": "edit", "args": { "path": path.display().to_string() } }));
        a.command("/open").await.unwrap();
        assert_eq!(a.panel.as_ref().map(|p| p.path.clone()), Some(path.clone()));
        a.command("/open /no/such/file").await.unwrap();
        assert!(a.notice.as_ref().is_some_and(|(t, s)| t.starts_with("can't open") && *s == Sty::Err), "{:?}", a.notice);
    }

    #[tokio::test]
    async fn the_files_tab_is_a_tree_you_walk_with_the_keyboard() {
        let dir = TempDir::new("dash");
        dir.file("docs/guide.md", "# The guide\n\nhello from the guide\n");
        dir.file("notes.txt", "plain notes\n");
        let mut a = app(110, 24);
        a.files = Some(Files::new(dir.path().to_path_buf()));
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        a.on_key(ctrl_b).await.unwrap();
        a.draw();
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("Files") && rows.contains("docs/") && rows.contains("notes.txt"), "{rows}");
        assert!(a.side_focus && a.columns().1 > 0);
        // Right opens the folder, down walks into it, enter opens the file in the Viewer tab.
        key(&mut a, KeyCode::Right).await;
        key(&mut a, KeyCode::Down).await;
        a.draw();
        assert!(a.screen.rows().join("\n").contains("guide.md"));
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        let rows = a.screen.rows().join("\n");
        assert!(a.tab == Tab::Viewer && rows.contains("hello from the guide"), "{rows}");
        // Left goes back to the tree, which keeps its place; the file stays a tab.
        key(&mut a, KeyCode::Left).await;
        a.draw();
        assert!(a.tab == Tab::Files && a.screen.rows().join("\n").contains("guide.md"));
        // Tab hands the keys back to the chat, where typing reaches the input again.
        key(&mut a, KeyCode::Tab).await;
        assert!(!a.side_focus);
        typed(&mut a, "hi").await;
        assert_eq!(a.editor.buf, "hi");
        // ctrl+b closes the whole panel.
        a.on_key(ctrl_b).await.unwrap();
        a.draw();
        assert_eq!(a.columns().1, 0);
    }

    #[tokio::test]
    async fn the_tree_starts_at_its_root_and_files_dir_moves_it() {
        let base = TempDir::new("root");
        std::fs::create_dir_all(base.join("home/skills")).unwrap();
        std::fs::create_dir_all(base.join("other/elsewhere")).unwrap();
        let mut a = app(110, 24);
        a.files_root = base.join("home");
        a.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)).await.unwrap();
        a.draw();
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("skills/") && !rows.contains("elsewhere"), "{rows}");
        // `/files <dir>` re-roots the tree; the panel stays open on the Files tab.
        a.command(&format!("/files {}", base.join("other").display())).await.unwrap();
        a.draw();
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("elsewhere/") && !rows.contains("skills/"), "{rows}");
        assert!(a.tab == Tab::Files && a.side_focus && a.columns().1 > 0);
        // A missing folder is refused and the tree stays where it was.
        a.command("/files /no/such/dir").await.unwrap();
        assert_eq!(a.files.as_ref().unwrap().root, base.join("other"));
        // Closed and reopened, the tree keeps its new root.
        a.command("/files").await.unwrap();
        assert_eq!(a.columns().1, 0);
        a.command("/files").await.unwrap();
        a.draw();
        assert!(a.screen.rows().join("\n").contains("elsewhere/"));
    }

    #[tokio::test]
    async fn clicking_a_tree_row_opens_it_and_a_tab_switches() {
        let dir = TempDir::new("click");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        dir.file("a.txt", "alpha text\n");
        let mut a = app(110, 24);
        a.files = Some(Files::new(dir.path().to_path_buf()));
        a.toggle_side();
        a.draw();
        let (cw, _) = a.columns();
        let x = cw + 3;
        // Row 0 is the tabs, row 1 the folder's path, row 2 the first entry (sub/), row 3 a.txt.
        let click = |row: u16| Event::Mouse(crossterm::event::MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x as u16 + 2, row, modifiers: KeyModifiers::NONE });
        a.on_terminal(click(3)).await.unwrap();
        let rows = a.screen.rows().join("\n");
        assert!(a.tab == Tab::Viewer && rows.contains("alpha text"), "{rows}");
        let files_tab = Event::Mouse(crossterm::event::MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x as u16 + 1, row: 0, modifiers: KeyModifiers::NONE });
        a.on_terminal(files_tab).await.unwrap();
        assert!(a.tab == Tab::Files);
        // Click and wheel agree on where the panel starts: the divider belongs to neither.
        assert!(!a.in_panel(cw + 2, 5) && a.in_panel(cw + 3, 5) && !a.in_panel(cw + 3, 23));
        let wheel_up = |column: usize| Event::Mouse(crossterm::event::MouseEvent { kind: MouseEventKind::ScrollUp, column: column as u16, row: 5, modifiers: KeyModifiers::NONE });
        assert_eq!(a.files.as_ref().unwrap().sel, 1, "a.txt, clicked above");
        a.on_terminal(wheel_up(cw + 2)).await.unwrap();
        assert_eq!(a.files.as_ref().unwrap().sel, 1, "the wheel over the divider doesn't move the tree");
        a.on_terminal(wheel_up(cw + 3)).await.unwrap();
        assert_eq!(a.files.as_ref().unwrap().sel, 0);
    }

    #[test]
    fn the_conversation_rewraps_when_the_panel_narrows_it() {
        let mut a = app(100, 30);
        a.push(Entry::User("word ".repeat(30).trim().to_string()));
        a.draw();
        let wide = a.view.len();
        a.panel = Some(Panel { path: "x.txt".into(), text: "x".into(), modified: None, scroll: 0, lines: Vec::new(), lines_w: 0 });
        a.draw();
        assert!(a.view.len() > wide, "narrower chat, more rows");
        assert!(a.view.iter().all(|l| l.iter().map(|(t, _)| width(t)).sum::<usize>() <= a.columns().0));
        a.panel = None;
        a.draw();
        assert_eq!(a.view.len(), wide, "closing the panel widens it again");
    }
}
