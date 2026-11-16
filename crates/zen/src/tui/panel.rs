//! The side panel next to the chat (full screen): the same views as the home screen (Sessions,
//! Files, Tasks; `views.rs` draws and drives them) and the file open from its Files view, reloaded
//! when it changes; `/open`, `/files`, `/close`, ctrl+b.

use super::*;

/// Narrowest terminal (columns) that fits the chat and the side panel next to each other.
pub(super) const SPLIT_MIN: usize = 60;

impl App {
    pub(super) fn side_open(&self) -> bool {
        self.side || self.side_file.is_some()
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
        self.side_file = None;
        self.side_focus = false;
    }

    /// Open the side panel on the view it last showed (Files at first) with the keys, or close it
    /// (and the file in it).
    pub(super) fn toggle_side(&mut self) {
        if !self.side_allowed() {
            return;
        }
        if self.side_open() {
            self.close_side();
        } else {
            self.warn_if_narrow();
            self.side = true;
            self.side_focus = true;
            self.set_view(Pane::Side, self.side_view);
        }
    }

    /// A click in the conversation: a file path or link under it opens in the side panel (a
    /// folder shows in the tree). Anything else does nothing.
    pub(super) fn click_chat(&mut self, x: usize, y: usize) {
        let Some((target, wiki)) = target_at(&self.chat_rows, y, x) else { return };
        let vault = self.vault();
        let found = if wiki {
            let link = Link { target: target.clone(), wiki: true, spans: Vec::new() };
            resolve_link(&link, &vault.join("AGENTS.md")).ok()
        } else {
            let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
            resolve_path(&target, &[vault, self.files_root.clone(), zen_client::zen_home(), home])
        };
        match found {
            Some(p) if p.is_dir() => self.show_folder(&p.to_string_lossy()),
            // Like /open: the panel shows it, the keys stay with the input (the wheel scrolls it).
            Some(p) if self.side_allowed() => self.open_file(Pane::Side, p),
            Some(_) => {}
            None if wiki => self.note(format!("no page named {target}"), Sty::Warn),
            None => {}
        }
    }

    /// The shared vault: zenbot's `global` folder.
    fn vault(&self) -> PathBuf {
        zen_client::zen_home().join("global")
    }

    /// `e` in the viewer: the file in `pane`, when it's a note of the vault, as a zen web link
    /// (the web editor). zen may run in a browser terminal, far from the owner's browser, so it
    /// opens nothing: the link goes to the clipboard (OSC 52) and shows as a hyperlink (OSC 8).
    pub(super) fn open_in_web(&mut self, pane: Pane) {
        let Some(path) = self.viewer(pane).as_ref().map(|p| p.path.clone()) else { return };
        let base = std::env::var("ZEN_WEB_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| ZEN_WEB_URL.into());
        match web_url(&base, &self.vault(), &path) {
            Some(url) => {
                self.flush(format!("\x1b]52;c;{}\x07", base64(url.as_bytes())));
                self.note(format!("zen web: {url} (copied; click or paste to open)"), Sty::Dim);
            }
            None => self.note("zen web opens only Markdown notes of the vault (~/.zenbot/global)", Sty::Warn),
        }
    }

    /// The folder tree, listed the first time it's needed.
    pub(super) fn files(&mut self) -> &mut Files {
        self.files.get_or_insert_with(|| Files::new(self.files_root.clone()))
    }

    /// The pointer at column `x`, row `y` is over the side panel (not the divider between it and
    /// the chat, nor the live region below).
    pub(super) fn in_panel(&self, x: usize, y: usize) -> bool {
        let (cw, pw) = self.columns();
        pw > 0 && x >= cw + 3 && y < self.viewport_rows()
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
        self.open_file(Pane::Side, expand_home(&raw));
    }

    /// `/files` and `/tasks`: the side panel on that view, with the keys; closed when that's what
    /// it already shows.
    pub(super) fn side_view_command(&mut self, v: View) {
        if self.side_open() && self.side_view == v {
            self.toggle_side();
        } else if self.side_allowed() {
            self.warn_if_narrow();
            self.side = true;
            self.side_focus = true;
            self.set_view(Pane::Side, v);
        }
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
        self.side_file = None;
        self.home_file = None;
        if !self.side_open() {
            self.toggle_side();
        }
        self.set_view(Pane::Side, View::Files);
        self.side_focus = true;
    }
}

/// Where the zen web app is served, unless `ZEN_WEB_URL` says otherwise.
const ZEN_WEB_URL: &str = "http://127.0.0.1:4400";

/// The web app's link to `path`, when it's a Markdown file inside `vault` that it lists (not
/// in a hidden folder nor in Archive).
fn web_url(base: &str, vault: &std::path::Path, path: &std::path::Path) -> Option<String> {
    let real = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let rel = real(path).strip_prefix(real(vault)).ok()?.to_path_buf();
    let listed = rel.components().all(|c| {
        let c = c.as_os_str().to_string_lossy();
        !c.starts_with('.') && c != "Archive"
    });
    (listed && rel.extension().is_some_and(|e| e == "md")).then(|| format!("{}/?path={}", base.trim_end_matches('/'), zen_client::enc(&rel.to_string_lossy())))
}

/// Standard base64, for OSC 52.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
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
        let path = dir.file("USER.md", "# Who I am\n\nSam, a builder.\n");
        a.commit(vec![line("chat text", Sty::Plain)]);
        a.command(&format!("/open {}", path.display())).await.unwrap();
        a.draw();
        let rows = a.screen.rows().to_vec();
        assert!(rows[0].contains("chat text") && rows[0].contains("Files") && rows[1].contains("USER.md"), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains(DIVIDER) && r.contains("Sam, a builder.")), "{rows:#?}");
        assert_eq!(a.columns(), (48, 48));
        // It follows the file as it changes.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, "# Who I am\n\nSam, a builder.\n").unwrap();
        let later = SystemTime::now() + Duration::from_secs(1);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        assert!(a.side_file.as_mut().unwrap().reload_if_changed());
        a.draw();
        assert!(a.screen.rows().iter().any(|r| r.contains("Sam, a builder.")));
        a.command("/close").await.unwrap();
        a.draw();
        assert!(a.screen.rows().iter().all(|r| !r.contains(DIVIDER)));
        assert_eq!(a.columns(), (99, 0));
    }

    #[tokio::test]
    async fn open_with_no_path_shows_the_last_file_a_tool_touched() {
        let mut a = app(100, 20);
        a.command("/open").await.unwrap();
        assert!(a.side_file.is_none() && a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("usage: /open")));
        let dir = TempDir::new("last-file");
        let path = dir.file("notes.txt", "first line\nsecond line\n");
        a.busy = true;
        a.on_event(json!({ "type": "tool_start", "name": "edit", "args": { "path": path.display().to_string() } }));
        a.command("/open").await.unwrap();
        assert_eq!(a.side_file.as_ref().map(|p| p.path.clone()), Some(path.clone()));
        a.command("/open /no/such/file").await.unwrap();
        assert!(a.notice.as_ref().is_some_and(|(t, s)| t.starts_with("can't open") && *s == Sty::Err), "{:?}", a.notice);
    }

    #[tokio::test]
    async fn the_files_tab_never_lists_or_shows_secret_files() {
        let home = TempDir::new("zenhome");
        home.file("token", "zen-token-s3cret");
        home.file("env", "ANTHROPIC_API_KEY=s3cret");
        home.file("matrix/recovery-key", "s3cret");
        home.file("config.toml", "model = \"x\"");
        let secrets = files::secret_paths(home.path(), &home.join("user"));
        let mut a = app(110, 24);
        a.files = Some(Files::with_secrets(home.path().to_path_buf(), secrets.clone()));
        a.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)).await.unwrap();
        a.draw();
        // The panel's side of each row (the chat's footer counts tokens).
        let rows = plain_rows(&a).iter().filter_map(|r| r.split_once(" │ ").map(|(_, p)| p.to_string())).collect::<Vec<_>>().join("\n");
        assert!(rows.contains("config.toml"), "{rows}");
        assert!(!rows.contains("token") && !rows.contains("env") && !rows.contains("matrix"), "{rows}");
        // Opened by path (`/open`), a secret file is refused, also through a symlink.
        let err = FileView::open_unless(home.join("token"), &secrets).err().unwrap();
        assert!(err.contains("holds secrets"), "{err}");
        std::os::unix::fs::symlink(home.join("matrix"), home.join("m")).unwrap();
        let real: Vec<PathBuf> = secrets.iter().filter_map(|p| std::fs::canonicalize(p).ok()).collect();
        assert!(FileView::open_unless(home.join("m/recovery-key"), &real).is_err());
        assert!(FileView::open_unless(home.join("config.toml"), &secrets).is_ok());
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
        // Right opens the folder, down walks into it, enter opens the file in place of the tree.
        key(&mut a, KeyCode::Right).await;
        key(&mut a, KeyCode::Down).await;
        a.draw();
        assert!(a.screen.rows().join("\n").contains("guide.md"));
        key(&mut a, KeyCode::Enter).await;
        a.draw();
        let rows = a.screen.rows().join("\n");
        assert!(a.side_view == View::Files && a.side_file.is_some() && rows.contains("hello from the guide") && !rows.contains("notes.txt"), "{rows}");
        // Left goes back to the tree, which keeps its place.
        key(&mut a, KeyCode::Left).await;
        a.draw();
        assert!(a.side_file.is_none() && a.files.as_ref().unwrap().sel == 1 && a.screen.rows().join("\n").contains("guide.md"));
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
        assert!(a.side_view == View::Files && a.side_focus && a.columns().1 > 0);
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
    async fn clicking_a_path_in_the_chat_opens_it_in_the_side_panel() {
        let dir = TempDir::new("chat-click");
        let f = dir.file("page.md", "# Page\nthe page text\n");
        let mut a = app(110, 24);
        a.chat_rows = vec![vec![("open ".into(), Sty::Plain), (f.to_string_lossy().into_owned(), Sty::Code)], vec![("plain words".into(), Sty::Plain)]];
        let click = |col: u16, row: u16| Event::Mouse(crossterm::event::MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE });
        a.last_vh = 20;
        a.on_terminal(click(2, 1)).await.unwrap();
        assert!(a.side_file.is_none(), "a word that isn't a file does nothing");
        a.chat_rows = vec![vec![("open ".into(), Sty::Plain), (f.to_string_lossy().into_owned(), Sty::Code)], vec![("plain words".into(), Sty::Plain)]];
        a.on_terminal(click(8, 0)).await.unwrap();
        assert!(a.side_file.as_ref().is_some_and(|p| p.path == f) && a.side_view == View::Files);
        assert!(!a.side_focus, "the keys stay with the input");
        assert!(plain_rows(&a).join("\n").contains("the page text"));
    }

    #[tokio::test]
    async fn clicking_a_tree_row_opens_it_and_a_tab_switches_the_view() {
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
        assert!(a.side_file.is_some() && rows.contains("alpha text"), "{rows}");
        // The tab bar: " Sessions " then " Files " then " Tasks ".
        let tab = |col: usize| Event::Mouse(crossterm::event::MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: (x + col) as u16, row: 0, modifiers: KeyModifiers::NONE });
        a.on_terminal(tab(1)).await.unwrap();
        assert!(a.side_view == View::Sessions);
        a.on_terminal(tab(19)).await.unwrap();
        assert!(a.side_view == View::Tasks && plain_rows(&a).join("\n").contains("loading…"));
        a.on_terminal(tab(12)).await.unwrap();
        assert!(a.side_view == View::Files && a.side_file.is_some(), "the file is still open in Files");
        key(&mut a, KeyCode::Esc).await;
        assert!(a.side_file.is_none() && a.side_focus, "esc goes back to the tree, the keys stay");
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
        a.side_file = Some(FileView { path: "x.txt".into(), text: "x".into(), ..Default::default() });
        a.draw();
        assert!(a.view.len() > wide, "narrower chat, more rows");
        assert!(a.view.iter().all(|l| l.iter().map(|(t, _)| width(t)).sum::<usize>() <= a.columns().0));
        a.side_file = None;
        a.draw();
        assert_eq!(a.view.len(), wide, "closing the panel widens it again");
    }

    #[test]
    fn web_links_only_markdown_notes_of_the_vault() {
        let vault = TempDir::new("web-vault");
        let note = vault.file("wiki/Ideas and me.md", "# Ideas\n");
        let base = "https://zenbot.example/";
        assert_eq!(web_url(base, vault.path(), &note).as_deref(), Some("https://zenbot.example/?path=wiki%2FIdeas%20and%20me.md"));
        for rel in ["notes.txt", ".versions/wiki/v1.md", "Archive/old.md"] {
            assert_eq!(web_url(base, vault.path(), &vault.file(rel, "x")), None, "{rel}");
        }
        let other = TempDir::new("web-elsewhere");
        assert_eq!(web_url(base, vault.path(), &other.file("a.md", "x")), None);
        assert_eq!([base64(b""), base64(b"f"), base64(b"fo"), base64(b"foo"), base64(b"foob")], ["", "Zg==", "Zm8=", "Zm9v", "Zm9vYg=="]);
    }

    #[tokio::test]
    async fn e_in_the_viewer_refuses_a_file_outside_the_vault() {
        let mut a = app(110, 24);
        let dir = TempDir::new("web-outside");
        a.command(&format!("/open {}", dir.file("notes.md", "hello").display())).await.unwrap();
        a.side_focus = true;
        a.capture = Some(String::new());
        key(&mut a, KeyCode::Char('e')).await;
        assert!(a.notice.as_ref().is_some_and(|(t, s)| t.starts_with("zen web opens only") && *s == Sty::Warn), "{:?}", a.notice);
        assert!(!a.capture.as_ref().unwrap().contains("\x1b]52"), "nothing copied");
        assert!(a.side_file.is_some(), "the file stays open");
    }

    #[test]
    fn a_link_in_the_notice_is_a_terminal_hyperlink() {
        let mut a = app(110, 24);
        a.note("zen web: https://zenbot.example/?path=wiki%2FIdeas.md (copied; click or paste to open)", Sty::Dim);
        a.draw();
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("\x1b]8;;https://zenbot.example/?path=wiki%2FIdeas.md\x1b\\"), "{rows:?}");
        assert!(plain_rows(&a).iter().any(|r| r.contains("zen web: https://zenbot.example/?path=wiki%2FIdeas.md (copied")), "{:#?}", plain_rows(&a));
    }
}
