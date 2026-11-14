//! Keys, mouse and paste; the `/` menu, slash commands and sending a prompt.

use super::*;

pub(super) use zen_client::commands::Command;
use zen_client::commands::Scope;

/// The commands the terminal app offers: the shared table, less the command line's own.
pub(super) fn commands() -> impl Iterator<Item = &'static Command> {
    zen_client::commands::COMMANDS.iter().filter(|c| c.scope != Scope::Line)
}

/// Whether choosing it in the menu waits for its argument (it needs one).
pub(super) fn takes_arg(c: &Command) -> bool {
    c.args.starts_with('<')
}

impl App {
    /// Commands matching what's typed, while the input is a bare `/word`.
    pub(super) fn menu(&self) -> Vec<&'static Command> {
        let buf = &self.editor.buf;
        if !buf.starts_with('/') || buf.contains(' ') || buf.contains('\n') {
            return Vec::new();
        }
        commands().filter(|c| c.name.starts_with(&buf[1..])).collect()
    }

    /// Refuse to leave the session while a turn runs in it (its output would land in the next one).
    pub(super) fn still_working(&mut self) -> bool {
        if self.busy {
            self.note("zenbot is still working; press esc to interrupt first", Sty::Warn);
        }
        self.busy
    }

    /// A terminal event. A failed request or send (a timeout, a 5xx, a dropped connection) is
    /// shown as a note; only losing the terminal itself ends zen.
    pub(super) async fn handle_terminal(&mut self, ev: Event) {
        if let Err(e) = self.on_terminal(ev).await {
            self.note(format!("{e:#}"), Sty::Err);
            self.draw();
        }
    }

    pub(super) async fn on_terminal(&mut self, ev: Event) -> Result<()> {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k).await?,
            Event::Paste(s) if self.name_prompt.is_some() => {
                if let Some(p) = &mut self.name_prompt {
                    p.push_text(&clean(&s).replace('\n', " "));
                }
            }
            Event::Paste(s) if self.board.is_some() => {
                if let Some(b) = self.board.as_mut().filter(|b| b.filtering) {
                    b.filter.push_str(&clean(&s).replace('\n', " "));
                    self.rebuild_board();
                }
            }
            Event::Paste(s) if self.picker.is_none() => self.editor.insert(&s),
            Event::Resize(cols, rows) => {
                // The terminal has re-wrapped what was on screen; find the region again before redrawing.
                self.region.reflow(cols as usize);
                self.size = (cols as usize, rows as usize);
                self.filled = self.filled.min(self.height());
                self.screen.invalidate();
            }
            Event::Mouse(m) => {
                // A click leaves the name prompt.
                if m.kind == MouseEventKind::Down(MouseButton::Left) {
                    self.name_prompt = None;
                }
                // The wheel scrolls whichever side is under the pointer.
                let (x, y) = (m.column as usize, m.row as usize);
                if self.board.is_some() {
                    match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => self.click_board(x, y).await?,
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                            let up = m.kind == MouseEventKind::ScrollUp;
                            let n = if self.home_view == View::Files && self.home_file.is_some() { 3 } else { 1 };
                            self.view_scroll(Pane::Home, up, n);
                        }
                        _ => return Ok(()),
                    }
                    if !self.quit {
                        self.draw();
                    }
                    return Ok(());
                }
                if m.kind == MouseEventKind::Down(MouseButton::Left) {
                    if self.in_panel(x, y) {
                        self.click_side(x - (self.columns().0 + 3), y).await?;
                    } else if x < self.columns().0 && y < self.chat_rows.len() {
                        self.click_chat(x, y);
                    }
                    self.draw();
                    return Ok(());
                }
                let up = match m.kind {
                    MouseEventKind::ScrollUp => true,
                    MouseEventKind::ScrollDown => false,
                    _ => return Ok(()),
                };
                if self.in_panel(x, y) {
                    self.view_scroll(Pane::Side, up, 3);
                } else {
                    self.scroll_chat(up, 3);
                }
            }
            _ => return Ok(()),
        }
        if !self.quit {
            self.draw();
        }
        Ok(())
    }

    pub(super) async fn on_key(&mut self, k: KeyEvent) -> Result<()> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);

        // The name prompt of the Files view takes every key, on the board or beside a chat.
        if self.name_prompt.is_some() {
            self.name_key(k);
            return Ok(());
        }

        if self.board.is_some() {
            return self.board_key(k).await;
        }

        if let Some(p) = &mut self.picker {
            match k.code {
                KeyCode::Up => p.selected = p.selected.saturating_sub(1),
                KeyCode::Down => p.selected = (p.selected + 1).min(p.items.len().saturating_sub(1)),
                KeyCode::Enter => self.pick().await?,
                KeyCode::Esc => self.picker = None,
                KeyCode::Char('c') if ctrl => self.picker = None,
                _ => {}
            }
            return Ok(());
        }

        if !self.inline && ctrl && k.code == KeyCode::Char('o') {
            self.expand_work = !self.expand_work;
            self.view_w = 0;
            self.draw();
            return Ok(());
        }

        if !self.inline {
            let plain = !ctrl && !alt && !shift;
            if ctrl && k.code == KeyCode::Char('b') {
                self.toggle_side();
                return Ok(());
            }
            // Shift+tab moves the keys between the chat and the side panel; tab (or esc) in the
            // panel hands them back. Tab in the chat is for completing: the `/` menu, a suggestion.
            if self.side_open() && (k.code == KeyCode::BackTab || (k.code == KeyCode::Tab && plain && self.side_focus)) {
                self.side_focus = !self.side_focus;
                return Ok(());
            }
            if self.side_focus && self.side_open() && self.side_key(k).await? {
                return Ok(());
            }
        }

        // Scrolling (full screen): PgUp/PgDn move the chat, alt+↑↓ and alt+PgUp/PgDn the side panel.
        if !self.inline {
            let page = self.viewport_rows().saturating_sub(2).max(1);
            let panel = self.columns().1 > 0;
            match k.code {
                KeyCode::Up | KeyCode::Down if alt && panel => {
                    self.view_scroll(Pane::Side, k.code == KeyCode::Up, 1);
                    return Ok(());
                }
                KeyCode::PageUp | KeyCode::PageDown if alt && panel => {
                    self.view_scroll(Pane::Side, k.code == KeyCode::PageUp, page);
                    return Ok(());
                }
                KeyCode::PageUp | KeyCode::PageDown => {
                    self.scroll_chat(k.code == KeyCode::PageUp, page);
                    return Ok(());
                }
                _ => {}
            }
        }

        if !(ctrl && k.code == KeyCode::Char('c')) {
            self.ctrl_c_at = None;
            if !self.busy || self.read_only {
                self.notice = None;
            }
        }

        // The `/` menu: ↑↓ move the highlight, Tab completes it, Enter runs it.
        let menu = self.menu();
        if !menu.is_empty() && !ctrl && !alt && !shift {
            let sel = self.menu_sel.min(menu.len() - 1);
            // A recalled `/command` from history keeps ↑↓ for history, so you can step past it.
            let browsing = self.editor.browsing_history();
            match k.code {
                KeyCode::Up | KeyCode::Down if browsing => {}
                KeyCode::Up => {
                    self.menu_sel = sel.checked_sub(1).unwrap_or(menu.len() - 1);
                    return Ok(());
                }
                KeyCode::Down => {
                    self.menu_sel = (sel + 1) % menu.len();
                    return Ok(());
                }
                KeyCode::Tab => {
                    self.editor.set(&format!("/{}", menu[sel].name));
                    self.menu_sel = 0;
                    return Ok(());
                }
                KeyCode::Enter => {
                    let c = menu[sel];
                    self.menu_sel = 0;
                    if takes_arg(c) {
                        self.editor.set(&format!("/{} ", c.name));
                    } else {
                        self.editor.set(&format!("/{}", c.name));
                        self.submit().await?;
                    }
                    return Ok(());
                }
                _ => {}
            }
        }

        let before = self.editor.buf.clone();
        self.edit_key(k, ctrl, alt, shift).await?;
        if self.editor.buf != before {
            self.menu_sel = 0;
        }
        Ok(())
    }

    pub(super) async fn edit_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool, shift: bool) -> Result<()> {
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if self.busy && !self.read_only {
                    self.abort().await;
                } else if !self.editor.is_empty() {
                    self.editor.clear();
                } else if self.ctrl_c_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                    self.quit = true;
                } else {
                    self.ctrl_c_at = Some(Instant::now());
                    self.note("press ctrl-c again to exit", Sty::Dim);
                }
            }
            KeyCode::Char('d') if ctrl => {
                if self.editor.is_empty() {
                    self.quit = true;
                } else {
                    self.editor.delete();
                }
            }
            // A subagent's session is watched, never interrupted from here: esc goes back.
            KeyCode::Esc if self.read_only && !self.inline => self.open_board(),
            KeyCode::Esc if self.editor.is_empty() && !self.busy && !self.inline => self.open_board(),
            KeyCode::Esc => {
                if self.busy && !self.read_only {
                    self.abort().await;
                } else if self.editor.buf.starts_with('/') {
                    self.editor.clear();
                }
            }
            // Tab takes the suggested next prompt into the input, to send (enter) or edit first.
            KeyCode::Tab if self.editor.is_empty() => {
                if let Some(s) = &mut self.suggestion {
                    s.taken = true;
                    let text = s.text.clone();
                    self.editor.set(&text);
                }
            }
            KeyCode::Enter if alt || shift || ctrl => self.editor.insert("\n"),
            KeyCode::Char('j') if ctrl => self.editor.insert("\n"),
            KeyCode::Enter => {
                if self.editor.buf.ends_with('\\') {
                    self.editor.backspace();
                    self.editor.insert("\n");
                } else {
                    self.submit().await?;
                }
            }
            KeyCode::Backspace if alt || ctrl => self.editor.delete_word(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Char('h') if ctrl => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Left if ctrl || alt => self.editor.word_left(),
            KeyCode::Right if ctrl || alt => self.editor.word_right(),
            KeyCode::Char('b') if alt => self.editor.word_left(),
            KeyCode::Char('f') if alt => self.editor.word_right(),
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.home(),
            KeyCode::End => self.editor.end(),
            KeyCode::Char('a') if ctrl => self.editor.home(),
            KeyCode::Char('e') if ctrl => self.editor.end(),
            KeyCode::Char('u') if ctrl => self.editor.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.editor.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.editor.delete_word(),
            KeyCode::Up => self.editor.up(),
            KeyCode::Down => self.editor.down(),
            KeyCode::Char(ch) if !ctrl => {
                let mut b = [0u8; 4];
                self.editor.insert(ch.encode_utf8(&mut b));
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) async fn abort(&mut self) {
        if let Some(sink) = &mut self.sink {
            let _ = sink.send(Message::text(json!({ "type": "abort" }).to_string())).await;
        }
        self.status = "Stopping".into();
        self.aborting = true;
    }

    pub(super) async fn submit(&mut self) -> Result<()> {
        let raw = self.editor.buf.trim();
        let steer = raw.starts_with("/steer ");
        let text = raw.strip_prefix("/steer ").unwrap_or(raw).trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        if let Some((server, flow)) = self.mcp_login.take() {
            self.editor.take();
            if text.starts_with('/') {
                self.note(format!("sign-in to {server} cancelled"), Sty::Dim);
                return self.command(&text).await;
            }
            self.finish_mcp_login(&flow, &text).await;
            return Ok(());
        }
        if text.starts_with('/') && !steer {
            self.editor.take();
            return self.command(&text).await;
        }
        if self.read_only {
            self.note("this session is read-only: zenbot drives it · esc goes back to the board", Sty::Warn);
            return Ok(());
        }
        // What became of the suggested next prompt goes with the prompt (read before a reconnect
        // below forgets it).
        let mut prompt = prompt_message(&text, self.suggestion.as_ref());
        if steer { prompt["mode"] = json!("steer"); }
        // The prompt stays in the input until it can be sent.
        if self.session.is_none() {
            // Only a model the owner chose; otherwise the kernel's default.
            let model = self.model_pinned.then_some(&self.model);
            let id = self.c.new_session(json!({ "model": model, "effort": self.effort, "agent": self.agent })).await?;
            self.session = Some(id.clone());
            self.connect(&id).await?;
        } else if self.sink.is_none() {
            // Reconnecting: fetch the session again, so what happened while disconnected shows
            // and a turn that is still running is known before sending another prompt.
            let id = self.session.clone().unwrap_or_default();
            self.switch_session(id).await?;
        }
        self.editor.take();
        self.suggestion = None;
        if self.title.is_empty() {
            self.title = text.chars().take(40).collect::<String>().trim().to_string();
        }
        self.scroll = 0; // back to the latest when you send
        if !self.still_working() {
            self.push(Entry::User(text.clone()));
            self.pending_prompt = Some(text.clone());
            self.begin_turn();
            self.aborting = false;
            self.reset_stream();
        }
        if let Some(sink) = &mut self.sink {
            if let Err(e) = sink.send(Message::text(prompt.to_string())).await {
                // The connection is gone: the next send reconnects.
                self.sink = None;
                self.busy = false;
                self.pending_prompt = None;
                anyhow::bail!("couldn't send to zenbot ({e}); press ↑ to get the prompt back and send it again");
            }
        }
        Ok(())
    }

    pub(super) async fn command(&mut self, input: &str) -> Result<()> {
        let (cmd, arg) = input.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((input, ""));
        let cmd = cmd.trim_start_matches('/');
        let cmd = commands().map(|c| c.name).find(|n| *n == cmd).or_else(|| {
            let m: Vec<_> = commands().map(|c| c.name).filter(|n| n.starts_with(cmd)).collect();
            if m.len() == 1 { Some(m[0]) } else { None }
        });
        match cmd {
            Some("board") => self.open_board(),
            Some("new" | "resume") if self.still_working() => {}
            Some("new") => match arg.strip_prefix('@').map(|r| r.split_whitespace().next().unwrap_or("").to_ascii_lowercase()) {
                Some(a) if a.is_empty() => self.note("usage: /new [@agent] [title]", Sty::Warn),
                Some(a) => self.fresh_session_as(Some(a)),
                None => self.fresh_session(),
            },
            Some("resume") => self.open_session_picker().await?,
            Some("model") if arg.is_empty() => self.open_model_picker(),
            Some("effort") if arg.is_empty() => self.open_effort_picker(),
            Some("done") => self.done(arg).await?,
            Some("rename") => match (&self.session, arg.is_empty()) {
                (_, true) => self.note("usage: /rename <title>", Sty::Warn),
                (None, _) => self.note("nothing to rename yet; send a message first", Sty::Warn),
                (Some(id), _) => {
                    self.c.patch(&format!("/api/sessions/{id}"), json!({ "title": arg })).await?;
                    self.title = arg.to_string();
                    self.note("renamed", Sty::Dim);
                }
            },
            Some("archive") => {
                if let Some(id) = self.session.clone() {
                    self.c.patch(&format!("/api/sessions/{id}"), json!({ "archived": true })).await?;
                    self.reset_session();
                    self.commit(vec![line(format!("archived {}", short(&id)), Sty::Dim), line("── new session ──", Sty::Dim), Vec::new()]);
                } else {
                    self.note("nothing to archive yet", Sty::Warn);
                }
            }
            Some("help") => {
                let mut out = vec![line("Commands", Sty::Bold)];
                for c in commands() {
                    out.push(vec![(format!("  /{:<10}", c.name), Sty::Accent), (format!("{}{}", c.help, if c.args.is_empty() { String::new() } else { format!(": /{} {}", c.name, c.args) }), Sty::Plain)]);
                }
                out.push(line("Keys", Sty::Bold));
                for (k, h) in [
                    ("enter", "send"),
                    ("shift+enter", "new line (also alt+enter, ctrl+j, or end a line with \\)"),
                    ("esc", "interrupt zenbot; on an empty input, back to the sessions board"),
                    ("tab", "take the suggested next prompt (grey) into the input; enter sends it"),
                    ("↑ ↓", "previous prompts; in the / menu, choose a command (tab completes)"),
                    ("ctrl+←/→", "move by word (also alt+b / alt+f); ctrl+a/e line start/end"),
                    ("ctrl+u/k", "delete to line start / end; ctrl+w deletes a word"),
                    ("ctrl+b", "open or close the side panel; shift+tab moves the keys between chat and panel"),
                    ("1 2 3", "Sessions, Files, Tasks"),
                    ("enter (Files)", "open the file in place of the tree; esc goes back to the tree"),
                    ("ctrl+o", "show or fold the steps of tool calls (full screen)"),
                    ("PgUp/PgDn", "scroll the conversation (also the mouse wheel)"),
                    ("alt+↑↓", "scroll the side panel (alt+PgUp/PgDn by page; or the wheel over it)"),
                    ("ctrl+c", "clear input; twice to exit"),
                    ("ctrl+d", "exit"),
                ] {
                    out.push(vec![(format!("  {k:<12}"), Sty::Accent), (h.to_string(), Sty::Plain)]);
                }
                out.push(Vec::new());
                self.commit(out);
            }
            Some("open") => self.open_panel(arg),
            Some("files") if arg.is_empty() => self.side_view_command(View::Files),
            // In full screen the Tasks view; inline, the list as a note (the shared command).
            Some("tasks") if !self.inline && arg.is_empty() => self.side_view_command(View::Tasks),
            Some("files") => self.show_folder(arg),
            Some("close") => {
                if self.side_open() {
                    self.close_side();
                } else {
                    self.note("no side panel is open", Sty::Dim);
                }
            }
            Some("mouse") if !self.inline => {
                self.mouse = !self.mouse;
                self.flush((if self.mouse { MOUSE_ON } else { MOUSE_OFF }).to_string());
                let state = if self.mouse { "on: the wheel scrolls (to select text: /mouse off, or shift+drag in most terminals)" } else { "off: drag selects text; PgUp/PgDn scroll" };
                self.note(format!("mouse {state}"), Sty::Dim);
            }
            Some("mouse") => self.note("inline mode leaves the mouse to the terminal", Sty::Dim),
            Some("mcp") if arg.starts_with("login") => self.mcp_command(arg).await,
            Some("upgrade") if arg.is_empty() => self.start_upgrade(),
            Some("restart") => {
                self.restart = true;
                self.quit = true;
            }
            Some("exit") => self.quit = true,
            // The rest are the shared commands (zen_client::commands): read here and sent to the
            // kernel with the owner's token, never to the agent.
            Some(c) => self.shared_command(&format!("{c} {arg}")).await,
            _ => self.note(format!("unknown command {input}; try /help"), Sty::Warn),
        }
        Ok(())
    }

    /// `/mcp login <server>` starts a sign-in whose answer the next typed line carries.
    async fn mcp_command(&mut self, arg: &str) {
        let mut words = arg.split_whitespace();
        let result = match (words.next(), words.next()) {
            (None, _) => self.c.get("/api/mcp").await.map(|r| zen_client::commands::mcp_status(&r)),
            (Some("login"), Some(server)) => match self.c.post("/api/mcp/login", json!({ "server": server })).await {
                Ok(r) => {
                    self.mcp_login = Some((server.to_string(), r["flow"].as_str().unwrap_or("").to_string()));
                    Ok(format!("{}\n(or type a /command to cancel)", zen_client::commands::login_instructions(&r)))
                }
                Err(e) => Err(e),
            },
            _ => return self.note("usage: /mcp [login <server>]", Sty::Warn),
        };
        match result {
            Ok(text) => {
                let mut lines: Vec<Line> = text.lines().map(|l| line(clean(l), Sty::Plain)).collect();
                lines.push(Vec::new());
                self.commit(lines);
            }
            Err(e) => self.note(format!("{e:#}"), Sty::Err),
        }
    }

    async fn finish_mcp_login(&mut self, flow: &str, pasted: &str) {
        match self.c.post(&format!("/api/mcp/login/{}", zen_client::enc(flow)), json!({ "callback": pasted })).await {
            Ok(done) => self.note(zen_client::commands::login_done(&done), Sty::Plain),
            Err(e) => self.note(format!("{e:#}"), Sty::Err),
        }
    }

    /// A shared command (`zen_client::commands`) on the open session: the kernel's answer, or why
    /// not, in the transcript. A failed call is shown like any other, as an error note.
    async fn shared_command(&mut self, text: &str) {
        let cmd = match zen_client::commands::parse(text) {
            Some(Ok(cmd)) => cmd,
            Some(Err(usage)) => return self.note(format!("/{usage}").replacen("/usage: ", "usage: /", 1), Sty::Warn),
            None => return self.note(format!("unknown command /{}; try /help", text.trim()), Sty::Warn),
        };
        if cmd.needs_session() && self.session.is_none() {
            return self.note("send a message first: this works on the session", Sty::Warn);
        }
        let out = match zen_client::commands::run(&self.c, &cmd, self.session.as_deref()).await {
            Ok(r) => {
                // Changes to the open session show at once.
                match &cmd {
                    zen_client::commands::Cmd::Model(Some(_)) | zen_client::commands::Cmd::Effort(Some(_)) => self.on_session_settings(&r.json),
                    _ => {}
                }
                r.text
            }
            Err(e) => return self.note(format!("{e:#}"), Sty::Err),
        };
        let mut lines: Vec<Line> = out.lines().map(|l| line(clean(l), Sty::Plain)).collect();
        lines.push(Vec::new());
        self.commit(lines);
    }
}

/// The `prompt` message for the kernel, with what became of the suggested next prompt, if one was shown.
pub(super) fn prompt_message(text: &str, suggestion: Option<&Suggestion>) -> Value {
    let mut prompt = json!({ "type": "prompt", "text": text });
    if let Some(s) = suggestion {
        prompt["suggestion"] = s.report();
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    fn suggest(a: &mut App, text: &str) {
        a.on_event(json!({ "type": "suggestion", "id": 7, "text": text }));
    }

    #[tokio::test]
    async fn a_suggestion_is_grey_until_tab_takes_it_and_enter_would_send_it_as_taken() {
        let mut a = app(60, 20);
        suggest(&mut a, "Run the tests again");
        let (lines, ..) = a.compose();
        let row = lines.iter().find(|l| l.iter().any(|(t, _)| t.contains("Run the tests again"))).expect("the suggestion is drawn");
        assert!(row.iter().any(|(t, s)| t.contains("Run the tests") && *s == Sty::Dim), "grey, not typed text: {row:?}");
        assert!(texts(&lines).iter().any(|l| l.contains("tab: use suggestion")));
        assert!(a.editor.is_empty());
        key(&mut a, KeyCode::Tab).await;
        assert_eq!(a.editor.buf, "Run the tests again");
        assert_eq!(prompt_message(&a.editor.buf, a.suggestion.as_ref())["suggestion"], json!({ "id": 7, "taken": true }));
        // Edited after tab: still taken; the kernel compares the text and records `edited`.
        typed(&mut a, " twice").await;
        assert_eq!(prompt_message(&a.editor.buf, a.suggestion.as_ref())["suggestion"]["taken"], true);
    }

    #[tokio::test]
    async fn typing_your_own_prompt_declines_the_suggestion() {
        let mut a = app(60, 20);
        suggest(&mut a, "Run the tests again");
        typed(&mut a, "something else").await;
        let t = texts(&a.compose().0);
        assert!(!t.iter().any(|l| l.contains("Run the tests again")), "the grey text gives way to typing");
        assert_eq!(prompt_message("something else", a.suggestion.as_ref())["suggestion"], json!({ "id": 7, "taken": false }));
        // Tab with text in the input doesn't replace it.
        key(&mut a, KeyCode::Tab).await;
        assert_eq!(a.editor.buf, "something else");
        assert_eq!(prompt_message("x", None).get("suggestion"), None);
    }

    #[tokio::test]
    async fn a_new_turn_clears_the_suggestion_and_a_late_one_is_ignored() {
        let mut a = app(60, 20);
        suggest(&mut a, "Run the tests again");
        a.on_event(json!({ "type": "busy", "busy": true }));
        assert!(a.suggestion.is_none());
        suggest(&mut a, "too late");
        assert!(a.suggestion.is_none(), "a suggestion arriving mid-turn isn't shown");
    }

    #[tokio::test]
    async fn the_kernel_can_rename_the_session() {
        let mut a = app(60, 20);
        a.on_event(json!({ "type": "title", "title": "Session names and suggestions" }));
        assert_eq!(a.title, "Session names and suggestions");
    }

    #[tokio::test]
    async fn tab_takes_the_suggestion_and_shift_tab_moves_to_the_panel() {
        let mut a = app(120, 24);
        a.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)).await.unwrap();
        key(&mut a, KeyCode::Esc).await; // back to the chat
        assert!(a.side_open() && !a.side_focus);
        suggest(&mut a, "Ship it");
        key(&mut a, KeyCode::Tab).await;
        assert!(!a.side_focus && a.editor.buf == "Ship it", "tab in the chat is for the suggestion");
        a.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)).await.unwrap();
        assert!(a.side_focus, "shift+tab moves the keys to the panel");
        a.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)).await.unwrap();
        assert!(!a.side_focus, "and back");
    }

    #[tokio::test]
    async fn slash_menu_arrows_tab_and_argument_commands() {
        let mut a = app(60, 20);
        typed(&mut a, "/").await;
        let t = texts(&a.compose().0);
        assert!(t.iter().any(|l| l.starts_with("› /new")), "{t:#?}");
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Tab).await;
        assert_eq!(a.editor.buf, "/resume");
        a.editor.clear();
        typed(&mut a, "/ren").await;
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.editor.buf, "/rename ", "a command that takes an argument waits for it");
        a.editor.clear();
        typed(&mut a, "/ste").await;
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.editor.buf, "/steer ", "steering waits for the owner's correction");
    }

    #[tokio::test]
    async fn arrows_step_through_history_past_a_recalled_command() {
        let mut a = app(60, 20);
        a.editor.set_history(&["first", "/help"]);
        key(&mut a, KeyCode::Up).await;
        assert_eq!(a.editor.buf, "/help");
        key(&mut a, KeyCode::Up).await;
        assert_eq!(a.editor.buf, "first", "the menu must not swallow ↑ on a recalled command");
    }

    #[tokio::test]
    async fn a_failed_request_becomes_a_note_and_zen_keeps_running() {
        let mut a = app(80, 20); // its kernel address refuses connections
        a.session = Some("s1".into());
        for ch in "/rename new title".chars() {
            a.handle_terminal(Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))).await;
        }
        a.handle_terminal(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))).await;
        assert!(!a.quit);
        assert!(a.notice.as_ref().is_some_and(|(t, s)| t.contains("cannot reach zenbot") && *s == Sty::Err), "{:?}", a.notice);
        assert!(a.screen.rows().join("\n").contains("cannot reach zenbot"), "the note is drawn");
    }

    #[tokio::test]
    async fn a_pasted_mcp_sign_in_goes_to_the_kernel_never_to_the_agent() {
        let mut a = app(80, 20); // its kernel address refuses connections
        a.mcp_login = Some(("linear".into(), "f1".into()));
        typed(&mut a, "http://127.0.0.1:33418/callback?code=secret&state=s").await;
        a.submit().await.unwrap();
        assert!(a.notice.as_ref().is_some_and(|(t, s)| t.contains("cannot reach zenbot") && *s == Sty::Err), "{:?}", a.notice);
        assert!(a.mcp_login.is_none() && a.entries.is_empty() && !a.busy && a.pending_prompt.is_none(), "nothing was sent as a prompt");
        a.mcp_login = Some(("linear".into(), "f2".into()));
        typed(&mut a, "/mcp nope").await;
        a.submit().await.unwrap();
        assert!(a.mcp_login.is_none(), "a /command cancels the sign-in");
        assert!(a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("usage: /mcp")), "and runs: {:?}", a.notice);
        let status = zen_client::commands::mcp_status(&json!({ "servers": { "linear": 12, "files": 3 }, "login": { "linear": "signed in" }, "problems": ["server `x`: down"] }));
        assert_eq!(status, "files: 3 tools\nlinear: 12 tools · signed in\nproblem: server `x`: down");
    }

    #[tokio::test]
    async fn shared_commands_go_to_the_kernel_never_to_the_agent() {
        let mut a = app(80, 20); // its kernel address refuses connections
        a.command("/jobs run").await.unwrap();
        assert!(a.notice.as_ref().is_some_and(|(t, s)| t.starts_with("usage: /jobs") && *s == Sty::Warn), "{:?}", a.notice);
        a.command("/jobs resume daily-digest").await.unwrap();
        assert!(a.notice.as_ref().is_some_and(|(t, s)| t.contains("cannot reach zenbot") && *s == Sty::Err), "{:?}", a.notice);
        a.command("/model codex/gpt-6-sol").await.unwrap();
        assert!(a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("send a message first")), "{:?}", a.notice);
        assert!(a.entries.is_empty() && !a.busy && a.pending_prompt.is_none(), "nothing was sent as a prompt");
        for c in ["/approve 3", "/trust", "/queue"] {
            a.notice = None;
            a.command(c).await.unwrap();
            assert!(a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("unknown command")), "{c} is gone: {:?}", a.notice);
        }
        assert!(!a.busy && a.pending_prompt.is_none(), "they aren't prompts either");
    }

    /// Every command of the shared table is the terminal app's too: none is unknown here.
    #[tokio::test]
    async fn every_command_is_known() {
        for c in commands() {
            let mut a = app(80, 20); // its kernel address refuses connections
            a.command(&format!("/{}", c.name)).await.ok();
            assert!(!a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("unknown command")), "/{}: {:?}", c.name, a.notice);
        }
    }

    #[tokio::test]
    async fn a_prompt_that_cant_reconnect_stays_in_the_input() {
        let mut a = app(80, 20); // its kernel address refuses connections
        a.session = Some("s1".into()); // and the connection was lost (no sink)
        typed(&mut a, "hello").await;
        a.handle_terminal(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))).await;
        assert_eq!(a.editor.buf, "hello");
        assert!(!a.busy && a.entries.is_empty(), "nothing was sent");
        assert!(a.notice.as_ref().is_some_and(|(_, s)| *s == Sty::Err));
    }
}
