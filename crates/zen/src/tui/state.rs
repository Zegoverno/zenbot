//! The app's state (`App`), and connecting to sessions: connect, switch, reset, history.

use super::*;

/// An event for the app: (session id, connection number, event). Events not tied to a session
/// (update checks, upgrade progress) have an empty id.
pub(super) type Incoming = (String, u64, Value);

pub(super) struct App {
    pub(super) c: Client,
    pub(super) tx: mpsc::UnboundedSender<Incoming>,
    pub(super) sink: Option<SplitSink<Ws, Message>>,
    pub(super) reader: Option<tokio::task::JoinHandle<()>>,
    /// Counts connections to the kernel; events read on an older connection are dropped (they
    /// may still be queued after a reconnect or a switch back to the same session).
    pub(super) conn: u64,
    pub(super) session: Option<String>,
    pub(super) title: String,
    pub(super) model: String,
    /// The session's thinking level; None means the model's default.
    pub(super) effort: Option<String>,
    pub(super) default_model: String,
    /// The kernel's model list (`/api/models`): id, name, efforts, default_effort.
    pub(super) models: Vec<Value>,
    pub(super) editor: Editor,
    pub(super) picker: Option<Picker>,
    /// Highlighted row in the `/` command menu.
    pub(super) menu_sel: usize,
    pub(super) region: Region,
    /// Terminal size (columns, rows), updated on resize.
    pub(super) size: (usize, usize),
    /// Inline mode: don't clear the screen or pin the live region to the bottom.
    pub(super) inline: bool,
    /// Rows of committed conversation on screen above the live region (capped at the height).
    pub(super) filled: usize,
    /// Tests collect output here instead of writing to the terminal.
    pub(super) capture: Option<String>,
    /// The terminal reports modified keys (kitty protocol), so shift+enter is distinct from enter.
    pub(super) enhanced: bool,
    pub(super) busy: bool,
    pub(super) status: String,
    pub(super) spin: usize,
    pub(super) turn_started: Instant,
    pub(super) stream: String,
    /// Full screen: the stream's complete lines rendered at `stream_w` columns, how many bytes of
    /// it they cover (up to and including its last newline), and the markdown state after them;
    /// so a delta renders only the unfinished last line, not the whole reply again.
    pub(super) stream_lines: Vec<Line>,
    pub(super) stream_done: usize,
    pub(super) stream_md: Md,
    pub(super) stream_w: usize,
    /// Inline: bytes of the stream already printed into scrollback, and the markdown state after them.
    pub(super) committed: usize,
    pub(super) md: Md,
    pub(super) turn_tokens: i64,
    pub(super) turn_model: String,
    /// Thinking level the kernel reported for the running turn.
    pub(super) turn_effort: Option<String>,
    pub(super) session_tokens: i64,
    pub(super) pending_prompt: Option<String>,
    pub(super) aborting: bool,
    pub(super) notice: Option<(String, Sty)>,
    pub(super) ctrl_c_at: Option<Instant>,
    pub(super) upgrading: bool,
    pub(super) quit: bool,
    /// Quit, then start the installed zen again on this session (`/restart`, or after `/upgrade`).
    pub(super) restart: bool,
    /// The installed zen binary and its modification time when this one started, to notice a newer
    /// install (zenbot can upgrade itself from inside a session).
    pub(super) installed: Option<(PathBuf, std::time::SystemTime)>,
    /// A newer install has already been announced.
    pub(super) newer_noted: bool,
    /// Full screen: the conversation, and its lines rendered at `view_w` columns (0: render all
    /// again), with the first line of each entry. Entries from `view_dirty` on changed since.
    pub(super) entries: Vec<Entry>,
    pub(super) view: Vec<Line>,
    pub(super) view_w: usize,
    pub(super) view_start: Vec<usize>,
    pub(super) view_dirty: usize,
    /// Full screen: lines scrolled up from the bottom of the conversation (0 follows it).
    pub(super) scroll: usize,
    /// The owner moved the viewport; don't treat this frame as transcript reflow to anchor.
    pub(super) scroll_input: bool,
    /// Conversation lines at the last frame, and the first one shown, to keep a scrolled-up view
    /// still as lines arrive, entries fold, or the conversation re-wraps.
    pub(super) last_total: usize,
    pub(super) top_line: usize,
    /// Rows the conversation got in the last frame (0: none drawn yet).
    pub(super) last_vh: usize,
    pub(super) screen: Screen,
    pub(super) panel: Option<Panel>,
    /// The last file a tool read or changed: what `/open` with no path shows.
    pub(super) last_file: Option<String>,
    /// Mouse wheel reporting is on (the terminal then needs shift+drag to select text).
    pub(super) mouse: bool,
    /// When the running tool started, for its elapsed time in the status line.
    pub(super) tool_since: Option<Instant>,
    /// The side panel is open (it is also open whenever a file is).
    pub(super) side: bool,
    /// The folder tree of the Files tab, created when first shown.
    pub(super) files: Option<Files>,
    /// Where the folder tree starts: zenbot's home (~/.zenbot) unless `/files <dir>` changes it.
    pub(super) files_root: PathBuf,
    pub(super) tab: Tab,
    /// Keys go to the side panel, not the input (tab switches).
    pub(super) side_focus: bool,
    /// Show every step of each run of tool calls (ctrl+o) instead of one line per run.
    pub(super) expand_work: bool,
    /// Tool calls and distinct files written or edited in the running turn, for the status line.
    pub(super) turn_tools: usize,
    pub(super) turn_files: std::collections::HashSet<String>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        c: Client,
        tx: mpsc::UnboundedSender<Incoming>,
        model: String,
        default_model: String,
        models: Vec<Value>,
        history: Option<std::path::PathBuf>,
        size: (usize, usize),
        inline: bool,
    ) -> App {
        App {
            c,
            tx,
            sink: None,
            reader: None,
            conn: 0,
            session: None,
            title: String::new(),
            model,
            effort: None,
            default_model,
            models,
            editor: Editor::new(history),
            picker: None,
            menu_sel: 0,
            region: Region::default(),
            size,
            inline,
            filled: 0,
            capture: None,
            enhanced: false,
            busy: false,
            status: String::new(),
            spin: 0,
            turn_started: Instant::now(),
            stream: String::new(),
            stream_lines: Vec::new(),
            stream_done: 0,
            stream_md: Md::default(),
            stream_w: 0,
            committed: 0,
            md: Md::default(),
            turn_tokens: 0,
            turn_model: String::new(),
            turn_effort: None,
            session_tokens: 0,
            pending_prompt: None,
            aborting: false,
            notice: None,
            ctrl_c_at: None,
            upgrading: false,
            quit: false,
            restart: false,
            installed: None,
            newer_noted: false,
            entries: Vec::new(),
            view: Vec::new(),
            view_w: 0,
            view_start: Vec::new(),
            view_dirty: usize::MAX,
            scroll: 0,
            scroll_input: false,
            last_total: 0,
            top_line: 0,
            last_vh: 0,
            screen: Screen::default(),
            panel: None,
            last_file: None,
            mouse: !inline,
            tool_since: None,
            side: false,
            files: None,
            files_root: std::env::current_dir().unwrap_or_default(),
            tab: Tab::Files,
            side_focus: false,
            expand_work: false,
            turn_tools: 0,
            turn_files: Default::default(),
        }
    }

    pub(super) async fn connect(&mut self, id: &str) -> Result<()> {
        if let Some(r) = self.reader.take() {
            r.abort();
        }
        let ws = self.c.connect(id).await?;
        let (sink, mut stream) = ws.split();
        let tx = self.tx.clone();
        let sid = id.to_string();
        self.conn += 1;
        let conn = self.conn;
        self.reader = Some(tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                if let Message::Text(t) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if tx.send((sid.clone(), conn, v)).is_err() {
                            break;
                        }
                    }
                } else if let Message::Close(_) = msg {
                    break;
                }
            }
            let _ = tx.send((sid, conn, json!({ "type": "disconnected" })));
        }));
        self.sink = Some(sink);
        Ok(())
    }

    /// Leave the current session: disconnect and forget its transcript and turn state (the side
    /// panel stays open; it shows a file, not the session).
    pub(super) fn reset_session(&mut self) {
        if let Some(r) = self.reader.take() {
            r.abort();
        }
        self.sink = None;
        self.session = None;
        self.title.clear();
        self.session_tokens = 0;
        self.busy = false;
        self.aborting = false;
        self.pending_prompt = None;
        self.last_file = None;
        self.reset_stream();
        self.entries.clear();
        self.view.clear();
        self.view_start.clear();
        self.view_w = 0;
        self.view_dirty = usize::MAX;
        self.scroll = 0;
        self.last_total = 0;
        self.top_line = 0;
    }

    pub(super) async fn switch_session(&mut self, id: String) -> Result<()> {
        let s = self.c.get(&format!("/api/sessions/{id}")).await?;
        self.reset_session();
        self.session = Some(id.clone());
        self.title = clean(s["title"].as_str().unwrap_or(""));
        self.model = s["model"].as_str().unwrap_or(&self.default_model).to_string();
        self.effort = s["effort"].as_str().map(String::from);
        self.connect(&id).await?;
        self.show_history(&s);
        Ok(())
    }

    /// Show a session's latest messages and total its tokens for the footer.
    pub(super) fn show_history(&mut self, s: &Value) {
        let msgs = s["messages"].as_array().cloned().unwrap_or_default();
        let mut head: Vec<Line> = vec![line(format!("── {} ──", if self.title.is_empty() { "session" } else { &self.title }), Sty::Dim), Vec::new()];
        let skip = msgs.len().saturating_sub(40);
        if skip > 0 {
            head.push(line(format!("… {skip} earlier messages"), Sty::Dim));
            head.push(Vec::new());
        }
        let mut entries = vec![Entry::Raw(head)];
        for m in msgs.iter().skip(skip) {
            entries.extend(Self::message_entries(m));
        }
        // The footer's total covers the whole session, not only the messages shown.
        for m in msgs.iter().filter(|m| m["role"] == "assistant") {
            self.session_tokens += usage_total(&m["usage"]);
        }
        self.push_all(entries);
        if s["busy"] == true {
            self.begin_turn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    #[tokio::test]
    async fn a_new_session_starts_a_fresh_transcript_and_resume_waits_for_the_turn() {
        let mut a = app(80, 20);
        a.session = Some("s1".into());
        a.push(Entry::User("old question".into()));
        a.busy = true;
        a.on_event(json!({ "type": "delta", "delta": "half an answ" }));
        a.on_event(json!({ "type": "tool_start", "name": "read", "args": { "path": "/tmp/x" } }));
        a.command("/resume").await.unwrap();
        assert!(a.picker.is_none() && a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("zenbot is still working")));
        a.command("/new").await.unwrap();
        assert_eq!(a.session.as_deref(), Some("s1"), "/new waits for the turn too");
        a.on_event(json!({ "type": "idle" }));
        a.command("/new").await.unwrap();
        assert!(a.session.is_none() && a.stream.is_empty() && a.last_file.is_none());
        assert_eq!(a.entries.len(), 1, "only the new-session line");
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("new session") && !rows.contains("old question") && !rows.contains("half an answ"), "{rows}");
    }

    #[test]
    fn resumed_session_counts_the_tokens_of_messages_not_shown() {
        let mut a = app(80, 20);
        let msg = json!({ "role": "assistant", "content": [{ "type": "text", "text": "ok" }], "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0 } });
        let s = json!({ "title": "t", "messages": vec![msg; 50] });
        a.show_history(&s);
        let shown = texts(&a.view).join("\n");
        assert!(shown.contains("… 10 earlier messages"), "{shown}");
        assert_eq!(a.session_tokens, 50 * 15, "every message counts, shown or not");
    }
}
