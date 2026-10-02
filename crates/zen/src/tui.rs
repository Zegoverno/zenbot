//! Interactive terminal app. The conversation is printed into normal terminal scrollback; a
//! live region at the bottom holds streaming text, status, the input box and the footer,
//! redrawn in place.
//!
//! Full screen (default): on start the screen is cleared (old contents go to scrollback) and
//! the live region is padded so it stays pinned to the bottom of the terminal.
//! Inline (`zen --inline` or ZEN_INLINE=1): no clearing or padding; the live region follows
//! the conversation, like Claude Code, Codex and Pi.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use unicode_width::UnicodeWidthStr;

use crate::client::{short, tool_summary, Client, NewSession, Ws};
use crate::editor::Editor;
use crate::md::{self, line, Line, Md, Sty};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PLACEHOLDER: &str = "Ask zenbot to do something…";

struct Command {
    name: &'static str,
    help: &'static str,
    /// Needs an argument: choosing it in the menu fills in the name and waits for the rest.
    takes_arg: bool,
}

const fn cmd(name: &'static str, help: &'static str, takes_arg: bool) -> Command {
    Command { name, help, takes_arg }
}

const COMMANDS: &[Command] = &[
    cmd("/new", "start a new session", false),
    cmd("/resume", "switch to another session", false),
    cmd("/model", "choose the model", false),
    cmd("/effort", "choose the thinking level", false),
    cmd("/done", "judge the work so far: accept, more, reshape or drop", false),
    cmd("/rename", "rename this session: /rename <title>", true),
    cmd("/archive", "archive this session and start a new one", false),
    cmd("/upgrade", "update zenbot to the latest version and restart it", false),
    cmd("/help", "keys and commands", false),
    cmd("/exit", "quit zen", false),
];

pub enum Start {
    New,
    Continue,
    Resume(Option<String>),
}

enum PickKind {
    Session,
    Model,
    Effort,
    Decision,
}

/// Your decisions on a session's work (`/done`), with what each one means.
const DECISIONS: [(&str, &str); 4] = [
    ("accept", "done, and good as it is"),
    ("more", "same goal, keep working"),
    ("reshape", "the framing was wrong: rethink the approach"),
    ("drop", "stop: not worth continuing"),
];

struct Picker {
    title: String,
    items: Vec<(String, String)>,
    selected: usize,
    kind: PickKind,
}

/// The live region at the bottom of the terminal, as last painted.
#[derive(Default)]
struct Region {
    height: usize,
    caret_row: usize,
    caret_col: usize,
    /// Display width of each painted line, to work out where the caret ended up after the
    /// terminal reflows them on resize.
    widths: Vec<usize>,
}

impl Region {
    /// The terminal was resized to `cols` columns and has re-wrapped the painted lines:
    /// recompute how many rows sit above the caret, so `erase` clears exactly the region.
    fn reflow(&mut self, cols: usize) {
        let cols = cols.max(1);
        let rows = |w: usize| w.div_ceil(cols).max(1);
        let above: usize = self.widths.iter().take(self.caret_row).map(|&w| rows(w)).sum();
        self.caret_row = above + self.caret_col / cols;
        self.height = self.widths.iter().map(|&w| rows(w)).sum();
    }
}

struct App {
    c: Client,
    tx: mpsc::UnboundedSender<(String, Value)>,
    sink: Option<SplitSink<Ws, Message>>,
    reader: Option<tokio::task::JoinHandle<()>>,
    session: Option<String>,
    title: String,
    model: String,
    /// The session's thinking level; None means the model's default.
    effort: Option<String>,
    default_model: String,
    /// The kernel's model list (`/api/models`): id, name, efforts, default_effort.
    models: Vec<Value>,
    editor: Editor,
    picker: Option<Picker>,
    /// Highlighted row in the `/` command menu.
    menu_sel: usize,
    region: Region,
    /// Terminal size (columns, rows), updated on resize.
    size: (usize, usize),
    /// Inline mode: don't clear the screen or pin the live region to the bottom.
    inline: bool,
    /// Rows of committed conversation on screen above the live region (capped at the height).
    filled: usize,
    /// Tests collect output here instead of writing to the terminal.
    capture: Option<String>,
    /// The terminal reports modified keys (kitty protocol), so shift+enter is distinct from enter.
    enhanced: bool,
    busy: bool,
    status: String,
    spin: usize,
    turn_started: Instant,
    stream: String,
    committed: usize,
    md: Md,
    turn_tokens: i64,
    turn_model: String,
    /// Thinking level the kernel reported for the running turn.
    turn_effort: Option<String>,
    session_tokens: i64,
    pending_prompt: Option<String>,
    aborting: bool,
    notice: Option<(String, Sty)>,
    ctrl_c_at: Option<Instant>,
    upgrading: bool,
    quit: bool,
}

fn banner(version_path: Option<&std::path::Path>) -> Line {
    let mut suffix = String::from(" · zenbot");
    if let Some(version) = version_path.and_then(|path| std::fs::read_to_string(path).ok()) {
        let version = version.trim();
        if !version.is_empty() {
            suffix.push_str(&format!(" · {version}"));
        }
    }
    vec![("zen".into(), Sty::Bold), (suffix, Sty::Dim)]
}

impl App {
    #[allow(clippy::too_many_arguments)]
    fn new(
        c: Client,
        tx: mpsc::UnboundedSender<(String, Value)>,
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
        }
    }
}

fn terminal_size() -> (usize, usize) {
    terminal::size().map(|(w, h)| (w as usize, h as usize)).unwrap_or((80, 24))
}

pub async fn run(c: Client, start: Start, new: NewSession, inline: bool) -> Result<()> {
    let models = c.get("/api/models").await?;
    let default_model = models["default"].as_str().unwrap_or("").to_string();
    let catalog: Vec<Value> = models["models"].as_array().cloned().unwrap_or_default();
    let signed_in = models["authenticated"].as_object().is_some_and(|a| a.values().any(|v| v == true));

    let history = std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".zenbot/history"));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let model = new.model.unwrap_or_else(|| default_model.clone());
    let mut app = App::new(c, tx, model, default_model, catalog, history, terminal_size(), inline);
    app.effort = new.effort;

    terminal::enable_raw_mode()?;
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    let mut out = std::io::stdout();
    execute!(out, EnableBracketedPaste)?;
    if enhanced {
        execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }
    app.enhanced = enhanced;
    if !inline {
        // Full screen: push what's on screen into scrollback and start from the top.
        let _ = out.write_all(format!("\x1b[999B{}\x1b[H", "\n".repeat(app.height())).as_bytes());
        let _ = out.flush();
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        default_hook(info);
    }));

    let result = async {
        app.commit(vec![
            banner(std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".zenbot/version")).as_deref()),
            line("type a task · /help for commands · esc interrupts · ctrl-d exits", Sty::Dim),
            Vec::new(),
        ]);
        if !signed_in {
            app.commit(vec![line("No model engine is signed in yet; run `zen login` first.", Sty::Warn), Vec::new()]);
        }
        // Mention an available update (from the kernel's last background check).
        let (c, tx) = (app.c.clone(), app.tx.clone());
        tokio::spawn(async move {
            if let Ok(v) = c.get("/api/version").await {
                if v["available"] == true {
                    let text = format!("{} · /upgrade to install", crate::client::describe_update(&v));
                    let _ = tx.send((String::new(), json!({ "type": "update_available", "text": text })));
                }
            }
        });
        match start {
            Start::New => {}
            Start::Continue => {
                let list = app.c.get("/api/sessions?archived=false").await?;
                match list.as_array().and_then(|a| a.first()).and_then(|s| s["id"].as_str()) {
                    Some(id) => app.switch_session(id.to_string()).await?,
                    None => app.note("No sessions yet; starting a new one.", Sty::Dim),
                }
            }
            Start::Resume(Some(id)) => {
                let id = app.c.resolve(&id).await?;
                app.switch_session(id).await?;
            }
            Start::Resume(None) => app.open_session_picker().await?,
        }
        app.draw();

        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(90));
        while !app.quit {
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(ev)) => app.on_terminal(ev).await?,
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some((sid, ev)) = rx.recv() => {
                    if sid.is_empty() {
                        app.on_app_event(ev).await;
                    } else if app.session.as_deref() == Some(sid.as_str()) {
                        app.on_event(ev);
                    }
                },
                _ = tick.tick(), if app.busy => {
                    app.spin = app.spin.wrapping_add(1);
                    app.draw();
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    // Restore the terminal.
    let mut s = String::from("\x1b[?2026h");
    app.erase(&mut s);
    if let Some(id) = &app.session {
        s.push_str(&md::to_ansi(&line(format!("session {} · resume with: zen -r {}", short(id), short(id)), Sty::Dim)));
        s.push_str("\r\n");
    }
    s.push_str("\x1b[?25h\x1b[?2026l");
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableBracketedPaste);
    let _ = terminal::disable_raw_mode();
    result
}

fn fmt_tokens(n: i64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

impl App {
    // ---------- drawing ----------

    /// Columns to draw in (one less than the terminal, so lines never trigger an auto-wrap).
    fn width(&self) -> usize {
        self.size.0.saturating_sub(1).max(20)
    }

    fn height(&self) -> usize {
        self.size.1.max(6)
    }

    fn erase(&mut self, out: &mut String) {
        if self.region.height > 0 {
            if self.region.caret_row > 0 {
                out.push_str(&format!("\x1b[{}A", self.region.caret_row));
            }
            out.push_str("\r\x1b[J");
        } else {
            out.push('\r');
        }
        self.region = Region::default();
    }

    fn paint_region(&mut self, out: &mut String) {
        let (mut lines, mut caret_row, caret_col, show_caret) = self.compose();
        let h = self.height();
        let max = h.saturating_sub(1);
        if lines.len() > max {
            let cut = lines.len() - max;
            lines.drain(..cut);
            caret_row = caret_row.saturating_sub(cut);
        }
        // Full screen: pad above so the region sits at the bottom of the screen.
        let pad = if self.inline { 0 } else { h.saturating_sub(self.filled.min(h) + lines.len()) };
        if pad > 0 {
            lines.splice(0..0, std::iter::repeat_with(Vec::new).take(pad));
            caret_row += pad;
        }
        let n = lines.len();
        self.filled = self.filled.min(h - n);
        out.push_str(&lines.iter().map(md::to_ansi).collect::<Vec<_>>().join("\r\n"));
        let up = n.saturating_sub(1).saturating_sub(caret_row);
        if up > 0 {
            out.push_str(&format!("\x1b[{up}A"));
        }
        out.push('\r');
        if caret_col > 0 {
            out.push_str(&format!("\x1b[{caret_col}C"));
        }
        out.push_str(if show_caret { "\x1b[?25h" } else { "\x1b[?25l" });
        let widths = lines.iter().map(|l| l.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum()).collect();
        self.region = Region { height: n, caret_row, caret_col, widths };
    }

    fn flush(&mut self, out: String) {
        if let Some(c) = &mut self.capture {
            c.push_str(&out);
            return;
        }
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    /// Redraw the live region in place.
    fn draw(&mut self) {
        let mut out = String::from("\x1b[?2026h");
        self.erase(&mut out);
        self.paint_region(&mut out);
        out.push_str("\x1b[?2026l");
        self.flush(out);
    }

    /// Print lines permanently into scrollback above the live region. Lines wider than the
    /// screen are wrapped here, so every printed line is exactly one row and `filled` stays true.
    fn commit(&mut self, lines: Vec<Line>) {
        if lines.is_empty() {
            return;
        }
        let w = self.width();
        let lines: Vec<Line> = lines
            .into_iter()
            .flat_map(|l| {
                if l.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum::<usize>() <= w {
                    vec![l]
                } else {
                    md::wrap(l, w, (String::new(), Sty::Plain), (String::new(), Sty::Plain))
                }
            })
            .collect();
        let mut out = String::from("\x1b[?2026h");
        self.erase(&mut out);
        for l in &lines {
            out.push_str(&md::to_ansi(l));
            out.push_str("\x1b[K\r\n");
        }
        self.filled = (self.filled + lines.len()).min(self.height());
        self.paint_region(&mut out);
        out.push_str("\x1b[?2026l");
        self.flush(out);
    }

    fn note(&mut self, text: impl Into<String>, sty: Sty) {
        self.notice = Some((text.into(), sty));
    }

    /// Commands matching what's typed, while the input is a bare `/word`.
    fn menu(&self) -> Vec<&'static Command> {
        let buf = &self.editor.buf;
        if !buf.starts_with('/') || buf.contains(' ') || buf.contains('\n') {
            return Vec::new();
        }
        COMMANDS.iter().filter(|c| c.name.starts_with(buf.as_str())).collect()
    }

    /// Build the live region: (lines, caret row, caret col, show caret).
    fn compose(&self) -> (Vec<Line>, usize, usize, bool) {
        let w = self.width();
        let mut lines: Vec<Line> = Vec::new();

        if let Some(p) = &self.picker {
            lines.push(line(p.title.clone(), Sty::Bold));
            let window = 10.min(p.items.len().max(1));
            let start = p.selected.saturating_sub(window - 1).min(p.items.len().saturating_sub(window));
            for (i, (label, _)) in p.items.iter().enumerate().skip(start).take(window) {
                let label: String = label.chars().take(w.saturating_sub(4)).collect();
                if i == p.selected {
                    lines.push(vec![("› ".into(), Sty::Accent), (label, Sty::Accent)]);
                } else {
                    lines.push(vec![("  ".into(), Sty::Plain), (label, Sty::Plain)]);
                }
            }
            if p.items.is_empty() {
                lines.push(line("  (nothing here)", Sty::Dim));
            }
            lines.push(line("↑↓ choose · enter select · esc cancel", Sty::Dim));
            let row = 1 + p.selected - start;
            return (lines, row, 0, false);
        }

        // Streaming text that hasn't completed a line yet.
        if self.busy && self.committed < self.stream.len() {
            let mut m = Md::default();
            let partial = m.render(&self.stream[self.committed..], w);
            let skip = partial.len().saturating_sub(6);
            lines.extend(partial.into_iter().skip(skip));
        }
        if self.busy {
            let secs = self.turn_started.elapsed().as_secs();
            lines.push(vec![
                (format!("{} ", SPINNER[self.spin % SPINNER.len()]), Sty::Accent),
                (self.status.chars().take(w.saturating_sub(30)).collect(), Sty::Plain),
                (format!("  {secs}s · esc to interrupt"), Sty::Dim),
            ]);
        } else if let Some((n, s)) = &self.notice {
            lines.push(line(n.chars().take(w).collect::<String>(), *s));
        }

        let hint = if self.editor.is_empty() {
            ""
        } else if self.enhanced {
            "enter send · shift+enter new line"
        } else {
            "enter send · alt+enter new line"
        };
        let (input, crow, ccol) = self.editor.render(w, PLACEHOLDER, hint, (self.height() / 2).max(3));
        let caret_row = lines.len() + crow;
        lines.extend(input);

        let menu = self.menu();
        if !menu.is_empty() {
            let sel = self.menu_sel.min(menu.len() - 1);
            for (i, c) in menu.iter().enumerate() {
                let mark = if i == sel { "› " } else { "  " };
                let help_sty = if i == sel { Sty::Plain } else { Sty::Dim };
                let help: String = c.help.chars().take(w.saturating_sub(12)).collect();
                lines.push(vec![(format!("{mark}{:<10}", c.name), Sty::Accent), (help, help_sty)]);
            }
        } else {
            let model = self.model.split('/').next_back().unwrap_or(&self.model);
            let session = match &self.session {
                Some(id) if !self.title.is_empty() => {
                    let t: String = self.title.chars().take(32).collect();
                    let ell = if self.title.chars().count() > 32 { "…" } else { "" };
                    format!("{t}{ell} ({})", short(id))
                }
                Some(id) => short(id).to_string(),
                None => "new session".into(),
            };
            let effort = self.shown_effort().map(|e| format!(" · {e}")).unwrap_or_default();
            let footer = format!("  {model}{effort} · {session} · {} tokens", fmt_tokens(self.session_tokens));
            lines.push(line(footer.chars().take(w).collect::<String>(), Sty::Dim));
        }
        (lines, caret_row, ccol, true)
    }

    // ---------- sessions ----------

    async fn connect(&mut self, id: &str) -> Result<()> {
        if let Some(r) = self.reader.take() {
            r.abort();
        }
        let ws = self.c.connect(id).await?;
        let (sink, mut stream) = ws.split();
        let tx = self.tx.clone();
        let sid = id.to_string();
        self.reader = Some(tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                if let Message::Text(t) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if tx.send((sid.clone(), v)).is_err() {
                            break;
                        }
                    }
                } else if let Message::Close(_) = msg {
                    break;
                }
            }
            let _ = tx.send((sid, json!({ "type": "disconnected" })));
        }));
        self.sink = Some(sink);
        Ok(())
    }

    fn reset_session(&mut self) {
        if let Some(r) = self.reader.take() {
            r.abort();
        }
        self.sink = None;
        self.session = None;
        self.title.clear();
        self.session_tokens = 0;
        self.busy = false;
    }

    async fn switch_session(&mut self, id: String) -> Result<()> {
        let s = self.c.get(&format!("/api/sessions/{id}")).await?;
        self.reset_session();
        self.session = Some(id.clone());
        self.title = s["title"].as_str().unwrap_or("").to_string();
        self.model = s["model"].as_str().unwrap_or(&self.default_model).to_string();
        self.effort = s["effort"].as_str().map(String::from);
        self.connect(&id).await?;

        let w = self.width();
        let msgs = s["messages"].as_array().cloned().unwrap_or_default();
        let mut out: Vec<Line> = vec![line(format!("── {} ──", if self.title.is_empty() { "session" } else { &self.title }), Sty::Dim), Vec::new()];
        let skip = msgs.len().saturating_sub(40);
        if skip > 0 {
            out.push(line(format!("… {skip} earlier messages"), Sty::Dim));
            out.push(Vec::new());
        }
        for m in msgs.iter().skip(skip) {
            out.extend(self.render_message(m, w));
            if m["role"] == "assistant" {
                self.session_tokens += ["input", "output", "cacheRead", "cacheWrite"].iter().map(|k| m["usage"][*k].as_i64().unwrap_or(0)).sum::<i64>();
            }
        }
        self.commit(out);
        if s["busy"] == true {
            self.busy = true;
            self.status = "Working".into();
            self.turn_started = Instant::now();
        }
        Ok(())
    }

    fn render_user(&self, text: &str, w: usize) -> Vec<Line> {
        let mut out = Vec::new();
        for (i, l) in text.split('\n').enumerate() {
            let prefix = if i == 0 { ("› ".to_string(), Sty::Accent) } else { ("  ".to_string(), Sty::Plain) };
            out.extend(md::wrap(vec![(l.to_string(), Sty::Bold)], w, prefix, ("  ".into(), Sty::Plain)));
        }
        out.push(Vec::new());
        out
    }

    fn render_tool_call(name: &str, args: &Value, w: usize) -> Vec<Line> {
        let summary = tool_summary(name, args);
        let detail = summary.strip_prefix(name).unwrap_or(&summary).trim().to_string();
        md::wrap(vec![(name.to_string(), Sty::Bold), (format!(" {detail}"), Sty::Dim)], w, ("• ".into(), Sty::Accent), ("  ".into(), Sty::Plain))
    }

    fn render_tool_result(m: &Value, w: usize) -> Vec<Line> {
        let text = text_of(&m["content"]);
        let sty = if m["isError"] == true { Sty::Err } else { Sty::Dim };
        let body: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut out = Vec::new();
        if body.is_empty() {
            out.push(line("  └ (no output)", Sty::Dim));
        }
        for (i, l) in body.iter().take(3).enumerate() {
            let max = w.saturating_sub(5);
            let mut s: String = l.chars().take(max).collect();
            if UnicodeWidthStr::width(s.as_str()) > max {
                s = s.chars().take(max.saturating_sub(1)).collect();
            }
            out.push(vec![(if i == 0 { "  └ " } else { "    " }.to_string(), Sty::Dim), (s, sty)]);
        }
        if body.len() > 3 {
            out.push(line(format!("    … {} more lines", body.len() - 3), Sty::Dim));
        }
        out.push(Vec::new());
        out
    }

    fn render_message(&self, m: &Value, w: usize) -> Vec<Line> {
        match m["role"].as_str() {
            Some("user") => self.render_user(&text_of(&m["content"]), w),
            Some("assistant") => {
                let mut out = Vec::new();
                let mut md = Md::default();
                for c in m["content"].as_array().into_iter().flatten() {
                    match c["type"].as_str() {
                        Some("text") if !c["text"].as_str().unwrap_or("").trim().is_empty() => {
                            out.extend(md.render(c["text"].as_str().unwrap_or("").trim_end(), w));
                            out.push(Vec::new());
                        }
                        Some("toolCall") => out.extend(Self::render_tool_call(c["name"].as_str().unwrap_or(""), &c["arguments"], w)),
                        _ => {}
                    }
                }
                if m["stopReason"] == "error" {
                    out.push(line(m["errorMessage"].as_str().unwrap_or("error").to_string(), Sty::Err));
                    out.push(Vec::new());
                }
                out
            }
            Some("toolResult") => Self::render_tool_result(m, w),
            _ => Vec::new(),
        }
    }

    async fn open_session_picker(&mut self) -> Result<()> {
        let list = self.c.get("/api/sessions?archived=false").await?;
        let items = list
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let title = s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(untitled)");
                let when = s["updated_at"].as_str().unwrap_or("").get(5..16).unwrap_or("").replace('T', " ");
                (format!("{when}  {title}"), s["id"].as_str().unwrap_or("").to_string())
            })
            .collect();
        self.picker = Some(Picker { title: "Resume a session".into(), items, selected: 0, kind: PickKind::Session });
        Ok(())
    }

    fn open_model_picker(&mut self) {
        let ids: Vec<String> = self.models.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect();
        let items: Vec<(String, String)> =
            ids.iter().map(|m| (format!("{}{}", m, if *m == self.model { "  (current)" } else { "" }), m.clone())).collect();
        let selected = ids.iter().position(|m| *m == self.model).unwrap_or(0);
        self.picker = Some(Picker { title: "Choose a model".into(), items, selected, kind: PickKind::Model });
    }

    // ---------- thinking level ----------

    fn model_info(&self, model: &str) -> Option<&Value> {
        self.models.iter().find(|m| m["id"] == model)
    }

    /// Thinking levels the current model takes (empty: none to choose).
    fn effort_levels(&self) -> Vec<String> {
        let info = self.model_info(&self.model);
        info.and_then(|m| m["efforts"].as_array()).into_iter().flatten().filter_map(|e| e.as_str().map(String::from)).collect()
    }

    fn default_effort(&self) -> Option<String> {
        self.model_info(&self.model).and_then(|m| m["default_effort"].as_str()).map(String::from)
    }

    /// The level turns run with: the session's choice, else the model's default.
    fn shown_effort(&self) -> Option<String> {
        self.effort.clone().or_else(|| self.default_effort())
    }

    fn open_decision_picker(&mut self) {
        let items = DECISIONS.iter().map(|(d, help)| (format!("{d:<8} {help}"), d.to_string())).collect();
        self.picker = Some(Picker { title: "How did it go?".into(), items, selected: 0, kind: PickKind::Decision });
    }

    /// `/done [decision] [note]`: record the decision, or open the picker when none is given.
    async fn done(&mut self, arg: &str) -> Result<()> {
        if self.session.is_none() {
            self.note("nothing to judge yet; send a message first", Sty::Warn);
            return Ok(());
        }
        let (decision, note) = arg.split_once(' ').map(|(d, n)| (d, n.trim())).unwrap_or((arg, ""));
        if decision.is_empty() {
            self.open_decision_picker();
        } else if DECISIONS.iter().any(|(d, _)| *d == decision) {
            self.record_decision(decision, note).await?;
        } else {
            self.note("usage: /done accept|more|reshape|drop [note]", Sty::Warn);
        }
        Ok(())
    }

    async fn record_decision(&mut self, decision: &str, note: &str) -> Result<()> {
        let Some(id) = self.session.clone() else { return Ok(()) };
        self.c.post(&format!("/api/sessions/{id}/decision"), json!({ "decision": decision, "note": note })).await?;
        self.note(format!("recorded: {decision}"), Sty::Dim);
        Ok(())
    }

    fn open_effort_picker(&mut self) {
        let levels = self.effort_levels();
        if levels.is_empty() {
            self.note(format!("{} has no thinking levels to choose from", self.model), Sty::Dim);
            return;
        }
        let current = |v: Option<&str>| if self.effort.as_deref() == v { "  (current)" } else { "" };
        let default = self.default_effort().map(|d| format!(" ({d})")).unwrap_or_default();
        let mut items = vec![(format!("default{default}{}", current(None)), "default".to_string())];
        items.extend(levels.iter().map(|l| (format!("{l}{}", current(Some(l))), l.clone())));
        let selected = self.effort.as_ref().and_then(|e| levels.iter().position(|l| l == e)).map(|i| i + 1).unwrap_or(0);
        self.picker = Some(Picker { title: "Choose a thinking level".into(), items, selected, kind: PickKind::Effort });
    }

    async fn pick(&mut self) -> Result<()> {
        let Some(p) = self.picker.take() else { return Ok(()) };
        let Some((_, value)) = p.items.get(p.selected).cloned() else { return Ok(()) };
        match p.kind {
            PickKind::Session => self.switch_session(value).await?,
            PickKind::Model => {
                self.model = value.clone();
                if let Some(id) = &self.session {
                    self.c.patch(&format!("/api/sessions/{id}"), json!({ "model": value })).await?;
                }
                // The kernel drops a level the new model doesn't take; do the same here.
                let reset = self.effort.as_ref().is_some_and(|e| !self.effort_levels().contains(e));
                if reset {
                    self.effort = None;
                }
                let effort = self.shown_effort().map(|e| format!(" · effort: {e}{}", if reset { " (default for this model)" } else { "" }));
                self.note(format!("model: {value}{}", effort.unwrap_or_default()), Sty::Dim);
            }
            PickKind::Decision => self.record_decision(&value, "").await?,
            PickKind::Effort => {
                self.effort = (value != "default").then(|| value.clone());
                if let Some(id) = &self.session {
                    self.c.patch(&format!("/api/sessions/{id}"), json!({ "effort": value })).await?;
                }
                self.note(format!("effort: {}", self.shown_effort().unwrap_or(value)), Sty::Dim);
            }
        }
        Ok(())
    }

    // ---------- input ----------

    async fn on_terminal(&mut self, ev: Event) -> Result<()> {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k).await?,
            Event::Paste(s) if self.picker.is_none() => self.editor.insert(&s),
            Event::Resize(cols, rows) => {
                // The terminal has re-wrapped what was on screen; find the region again before redrawing.
                self.region.reflow(cols as usize);
                self.size = (cols as usize, rows as usize);
                self.filled = self.filled.min(self.height());
            }
            _ => return Ok(()),
        }
        if !self.quit {
            self.draw();
        }
        Ok(())
    }

    async fn on_key(&mut self, k: KeyEvent) -> Result<()> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);

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

        if !(ctrl && k.code == KeyCode::Char('c')) {
            self.ctrl_c_at = None;
            if !self.busy {
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
                    self.editor.set(menu[sel].name);
                    self.menu_sel = 0;
                    return Ok(());
                }
                KeyCode::Enter => {
                    let c = menu[sel];
                    self.menu_sel = 0;
                    if c.takes_arg {
                        self.editor.set(&format!("{} ", c.name));
                    } else {
                        self.editor.set(c.name);
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

    async fn edit_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool, shift: bool) -> Result<()> {
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if self.busy {
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
            KeyCode::Esc => {
                if self.busy {
                    self.abort().await;
                } else if self.editor.buf.starts_with('/') {
                    self.editor.clear();
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

    async fn abort(&mut self) {
        if let Some(sink) = &mut self.sink {
            let _ = sink.send(Message::text(json!({ "type": "abort" }).to_string())).await;
        }
        self.status = "Stopping".into();
        self.aborting = true;
    }

    async fn submit(&mut self) -> Result<()> {
        let text = self.editor.buf.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        if text.starts_with('/') {
            self.editor.take();
            return self.command(&text).await;
        }
        if self.busy {
            self.note("zenbot is still working; press esc to interrupt first", Sty::Warn);
            return Ok(());
        }
        self.editor.take();
        if self.session.is_none() {
            let id = self.c.new_session(Some(self.model.clone()), self.effort.clone()).await?;
            self.session = Some(id.clone());
            self.connect(&id).await?;
        } else if self.sink.is_none() {
            let id = self.session.clone().unwrap_or_default();
            self.connect(&id).await?;
        }
        if self.title.is_empty() {
            self.title = text.chars().take(40).collect::<String>().trim().to_string();
        }
        let w = self.width();
        self.commit(self.render_user(&text, w));
        self.pending_prompt = Some(text.clone());
        self.busy = true;
        self.status = "Working".into();
        self.turn_started = Instant::now();
        self.turn_tokens = 0;
        self.aborting = false;
        self.stream.clear();
        self.committed = 0;
        self.md = Md::default();
        if let Some(sink) = &mut self.sink {
            sink.send(Message::text(json!({ "type": "prompt", "text": text }).to_string())).await?;
        }
        Ok(())
    }

    async fn command(&mut self, input: &str) -> Result<()> {
        let (cmd, arg) = input.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((input, ""));
        let cmd = COMMANDS.iter().map(|c| c.name).find(|n| *n == cmd).or_else(|| {
            let m: Vec<_> = COMMANDS.iter().map(|c| c.name).filter(|n| n.starts_with(cmd)).collect();
            if m.len() == 1 { Some(m[0]) } else { None }
        });
        match cmd {
            Some("/new") => {
                if self.busy {
                    self.note("zenbot is still working; press esc to interrupt first", Sty::Warn);
                    return Ok(());
                }
                self.reset_session();
                self.model = self.default_model.clone();
                self.effort = None;
                self.commit(vec![line("── new session ──", Sty::Dim), Vec::new()]);
            }
            Some("/resume") => self.open_session_picker().await?,
            Some("/model") => self.open_model_picker(),
            Some("/effort") => self.open_effort_picker(),
            Some("/done") => self.done(arg).await?,
            Some("/rename") => match (&self.session, arg.is_empty()) {
                (_, true) => self.note("usage: /rename <title>", Sty::Warn),
                (None, _) => self.note("nothing to rename yet; send a message first", Sty::Warn),
                (Some(id), _) => {
                    self.c.patch(&format!("/api/sessions/{id}"), json!({ "title": arg })).await?;
                    self.title = arg.to_string();
                    self.note("renamed", Sty::Dim);
                }
            },
            Some("/archive") => {
                if let Some(id) = self.session.clone() {
                    self.c.patch(&format!("/api/sessions/{id}"), json!({ "archived": true })).await?;
                    self.reset_session();
                    self.commit(vec![line(format!("archived {}", short(&id)), Sty::Dim), line("── new session ──", Sty::Dim), Vec::new()]);
                } else {
                    self.note("nothing to archive yet", Sty::Warn);
                }
            }
            Some("/help") => {
                let mut out = vec![line("Commands", Sty::Bold)];
                for Command { name: n, help: h, .. } in COMMANDS {
                    out.push(vec![(format!("  {n:<10}"), Sty::Accent), (h.to_string(), Sty::Plain)]);
                }
                out.push(line("Keys", Sty::Bold));
                for (k, h) in [
                    ("enter", "send"),
                    ("shift+enter", "new line (also alt+enter, ctrl+j, or end a line with \\)"),
                    ("esc", "interrupt zenbot"),
                    ("↑ ↓", "previous prompts; in the / menu, choose a command (tab completes)"),
                    ("ctrl+←/→", "move by word (also alt+b / alt+f); ctrl+a/e line start/end"),
                    ("ctrl+u/k", "delete to line start / end; ctrl+w deletes a word"),
                    ("ctrl+c", "clear input; twice to exit"),
                    ("ctrl+d", "exit"),
                ] {
                    out.push(vec![(format!("  {k:<12}"), Sty::Accent), (h.to_string(), Sty::Plain)]);
                }
                out.push(Vec::new());
                self.commit(out);
            }
            Some("/upgrade") => self.start_upgrade(),
            Some("/exit") => self.quit = true,
            _ => self.note(format!("unknown command {input}; try /help"), Sty::Warn),
        }
        Ok(())
    }

    // ---------- upgrade ----------

    fn start_upgrade(&mut self) {
        if self.upgrading {
            self.note("an upgrade is already running", Sty::Warn);
            return;
        }
        self.upgrading = true;
        self.commit(vec![line("checking for updates…", Sty::Dim)]);
        let (c, tx) = (self.c.clone(), self.tx.clone());
        tokio::spawn(async move {
            let log_tx = tx.clone();
            let res = c.upgrade(move |l| {
                let _ = log_tx.send((String::new(), json!({ "type": "upgrade_log", "line": l })));
            });
            let ev = match res.await {
                Ok(msg) => json!({ "type": "upgrade_done", "ok": true, "text": msg }),
                Err(e) => json!({ "type": "upgrade_done", "ok": false, "text": format!("{e:#}") }),
            };
            let _ = tx.send((String::new(), ev));
        });
    }

    /// Events that aren't tied to a session (sent with an empty session id).
    async fn on_app_event(&mut self, ev: Value) {
        let text = ev["text"].as_str().or(ev["line"].as_str()).unwrap_or("").to_string();
        match ev["type"].as_str().unwrap_or("") {
            "update_available" => self.commit(vec![line(text, Sty::Warn), Vec::new()]),
            "upgrade_log" => self.commit(vec![line(text, Sty::Dim)]),
            "upgrade_done" => {
                self.upgrading = false;
                let ok = ev["ok"] == true;
                self.commit(vec![line(text, if ok { Sty::Accent } else { Sty::Err }), Vec::new()]);
                if let Some(id) = self.session.clone() {
                    if self.sink.is_none() && self.connect(&id).await.is_err() {
                        self.note("lost connection to zenbot; send a message to reconnect", Sty::Warn);
                    }
                }
                self.draw();
            }
            _ => {}
        }
    }

    // ---------- kernel events ----------

    fn on_event(&mut self, ev: Value) {
        let w = self.width();
        match ev["type"].as_str().unwrap_or("") {
            "message" => {
                let m = &ev["message"];
                match m["role"].as_str() {
                    Some("user") => {
                        let text = text_of(&m["content"]);
                        if self.pending_prompt.as_deref() == Some(text.as_str()) {
                            self.pending_prompt = None;
                        } else {
                            self.commit(self.render_user(&text, w));
                        }
                    }
                    Some("assistant") => {
                        let mut out = Vec::new();
                        let full: String = m["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect();
                        if self.stream.is_empty() {
                            if !full.trim().is_empty() {
                                out.extend(self.md.render(full.trim_end(), w));
                            }
                        } else if self.committed < self.stream.len() {
                            let rest = self.stream[self.committed..].to_string();
                            out.extend(self.md.render(rest.trim_end(), w));
                        }
                        if !full.trim().is_empty() {
                            out.push(Vec::new());
                        }
                        self.stream.clear();
                        self.committed = 0;
                        self.md = Md::default();
                        for c in m["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "toolCall") {
                            out.extend(Self::render_tool_call(c["name"].as_str().unwrap_or(""), &c["arguments"], w));
                        }
                        if m["stopReason"] == "error" && !self.aborting {
                            out.push(line(m["errorMessage"].as_str().unwrap_or("error").to_string(), Sty::Err));
                        }
                        let u = &m["usage"];
                        self.turn_tokens += ["input", "output", "cacheRead", "cacheWrite"].iter().map(|k| u[*k].as_i64().unwrap_or(0)).sum::<i64>();
                        self.turn_model = m["model"].as_str().unwrap_or("").to_string();
                        self.commit(out);
                    }
                    Some("toolResult") => self.commit(Self::render_tool_result(m, w)),
                    _ => {}
                }
            }
            "delta" => {
                self.stream.push_str(ev["delta"].as_str().unwrap_or(""));
                self.status = "Writing".into();
                let mut out = Vec::new();
                while let Some(pos) = self.stream[self.committed..].find('\n') {
                    let l = self.stream[self.committed..self.committed + pos].to_string();
                    out.extend(self.md.render_line(&l, w));
                    self.committed += pos + 1;
                }
                if out.is_empty() {
                    self.draw();
                } else {
                    self.commit(out);
                }
            }
            "busy" => self.turn_effort = ev["effort"].as_str().map(String::from),
            "usage" => {
                self.turn_tokens += ev["input"].as_i64().unwrap_or(0) + ev["output"].as_i64().unwrap_or(0);
            }
            "thinking" => {
                self.status = "Thinking".into();
            }
            "tool_start" => {
                self.status = format!("Running {}", tool_summary(ev["name"].as_str().unwrap_or(""), &ev["args"]));
                self.draw();
            }
            "tool_end" => {
                self.status = "Working".into();
            }
            "end" if !self.busy => {} // already ended (e.g. after a resync)
            "resync" => {
                self.note(format!("missed {} updates from zenbot (the terminal fell behind)", ev["skipped"]), Sty::Warn);
                if ev["busy"] == false && self.busy {
                    self.on_event(serde_json::json!({ "type": "end", "error": null }));
                }
            }
            "end" => {
                let mut out = Vec::new();
                if self.aborting {
                    if self.committed < self.stream.len() {
                        let rest = self.stream[self.committed..].to_string();
                        out.extend(self.md.render(rest.trim_end(), w));
                    }
                    out.push(line("interrupted", Sty::Warn));
                } else if let Some(e) = ev["error"].as_str() {
                    out.push(line(e.to_string(), Sty::Err));
                }
                self.aborting = false;
                self.stream.clear();
                self.committed = 0;
                // The kernel's totals cover the whole turn, including calls the stream never showed.
                let r = &ev["turn"];
                if r.is_object() {
                    self.turn_tokens = ["input_tokens", "output_tokens", "cache_read", "cache_write"].iter().map(|k| r[*k].as_i64().unwrap_or(0)).sum();
                }
                let secs = self.turn_started.elapsed().as_secs_f32();
                let effort = self.turn_effort.take().map(|e| format!(" · {e}")).unwrap_or_default();
                out.push(line(format!("  {}{effort} · {} tokens · {:.1}s", self.turn_model, fmt_tokens(self.turn_tokens), secs), Sty::Dim));
                out.push(Vec::new());
                self.session_tokens += self.turn_tokens;
                self.busy = false;
                self.commit(out);
            }
            "error" => {
                self.busy = false;
                self.commit(vec![line(ev["error"].as_str().unwrap_or("error").to_string(), Sty::Err), Vec::new()]);
            }
            "disconnected" if self.upgrading => {
                self.busy = false;
                self.sink = None; // expected: zenbot is restarting; we reconnect when it's back
            }
            "disconnected" => {
                self.busy = false;
                self.sink = None;
                self.note("lost connection to zenbot; send a message to reconnect", Sty::Warn);
                self.draw();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app on a fake terminal of `cols` x `rows`, capturing output, with no kernel behind it.
    fn app(cols: usize, rows: usize) -> App {
        let c = Client::new("http://127.0.0.1:9".into(), Some("test".into())).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut a = App::new(c, tx, "claude/claude-opus-5-5".into(), "claude/claude-opus-5-5".into(), vec![], None, (cols, rows), false);
        a.capture = Some(String::new());
        a
    }

    /// An app with a model list: one model with thinking levels (default high), one without.
    fn app_with_models(cols: usize, rows: usize) -> App {
        let mut a = app(cols, rows);
        a.models = vec![
            json!({ "id": "claude/claude-opus-5-5", "efforts": ["low", "medium", "high", "xhigh", "max"], "default_effort": "high" }),
            json!({ "id": "faux/smoke" }),
        ];
        a
    }

    fn footer(a: &App) -> String {
        texts(&a.compose().0).last().cloned().unwrap_or_default()
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.iter().map(|(t, _)| t.as_str()).collect()).collect()
    }

    fn width(s: &str) -> usize {
        UnicodeWidthStr::width(s)
    }

    async fn key(a: &mut App, code: KeyCode) {
        a.on_key(KeyEvent::new(code, KeyModifiers::NONE)).await.unwrap();
    }

    async fn typed(a: &mut App, text: &str) {
        for ch in text.chars() {
            key(a, KeyCode::Char(ch)).await;
        }
    }

    #[test]
    fn region_fits_the_screen_and_shows_the_input_box() {
        let a = app(40, 12);
        let (lines, caret_row, _, _) = a.compose();
        let t = texts(&lines);
        assert!(t.iter().all(|l| width(l) <= a.width()), "{t:#?}");
        assert!(t.iter().any(|l| l.starts_with('╭')) && t.iter().any(|l| l.starts_with('╰')));
        assert!(t[caret_row].contains(PLACEHOLDER.chars().take(10).collect::<String>().as_str()));
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

    #[test]
    fn committed_lines_wider_than_the_screen_are_wrapped_and_counted() {
        let mut a = app(40, 30);
        a.commit(vec![line("x".repeat(100), Sty::Err)]);
        // 39 usable columns: 100 characters take 3 rows, and the pinning must count all 3.
        assert_eq!(a.filled, 3);
        let out = a.capture.take().unwrap();
        assert_eq!(out.matches("\x1b[K\r\n").count(), 3);
    }

    #[test]
    fn full_screen_pins_the_region_to_the_bottom_and_inline_does_not() {
        let mut a = app(40, 20);
        a.draw();
        assert_eq!(a.region.height, 20, "padded to fill the screen");
        let mut b = app(40, 20);
        b.inline = true;
        b.draw();
        assert!(b.region.height < 20);
    }

    #[test]
    fn resize_recomputes_where_the_caret_is() {
        let mut r = Region { height: 3, caret_row: 2, caret_col: 5, widths: vec![39, 39, 10] };
        r.reflow(20);
        // Narrowed to 20 columns, each 39-wide line now takes 2 rows.
        assert_eq!(r.caret_row, 4);
        assert_eq!(r.height, 5);
    }

    fn banner_text(path: Option<&std::path::Path>) -> String {
        banner(path).into_iter().map(|(text, _)| text).collect()
    }

    #[test]
    fn banner_shows_installed_version_when_available() {
        let path = std::env::temp_dir().join(format!("zen-banner-version-{}", std::process::id()));
        assert_eq!(banner_text(None), "zen · zenbot");
        assert_eq!(banner_text(Some(&path.join("missing"))), "zen · zenbot");
        std::fs::write(&path, "775d408\n").unwrap();
        assert_eq!(banner_text(Some(&path)), "zen · zenbot · 775d408");
        std::fs::write(&path, " \n").unwrap();
        assert_eq!(banner_text(Some(&path)), "zen · zenbot");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn footer_shows_the_thinking_level_turns_run_with() {
        let mut a = app_with_models(80, 20);
        assert!(footer(&a).contains("claude-opus-5-5 · high · new session"), "{}", footer(&a));
        a.effort = Some("max".into());
        assert!(footer(&a).contains("claude-opus-5-5 · max · "), "{}", footer(&a));
        a.model = "faux/smoke".into();
        a.effort = None;
        assert!(footer(&a).contains("smoke · new session"), "a model without levels shows none: {}", footer(&a));
    }

    #[tokio::test]
    async fn effort_picker_lists_the_models_levels_and_sets_the_choice() {
        let mut a = app_with_models(80, 20);
        typed(&mut a, "/effort").await;
        key(&mut a, KeyCode::Enter).await;
        let items: Vec<String> = a.picker.as_ref().expect("picker open").items.iter().map(|(label, _)| label.clone()).collect();
        assert_eq!(items, ["default (high)  (current)", "low", "medium", "high", "xhigh", "max"]);
        for _ in 0..5 {
            key(&mut a, KeyCode::Down).await;
        }
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.effort.as_deref(), Some("max"));
        // Choosing "default" goes back to the model's default.
        typed(&mut a, "/effort").await;
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.picker.as_ref().unwrap().selected, 5, "the current level is highlighted");
        for _ in 0..5 {
            key(&mut a, KeyCode::Up).await;
        }
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.effort, None);
    }

    #[tokio::test]
    async fn switching_to_a_model_without_the_level_drops_it() {
        let mut a = app_with_models(80, 20);
        a.effort = Some("max".into());
        typed(&mut a, "/model").await;
        key(&mut a, KeyCode::Enter).await;
        key(&mut a, KeyCode::Down).await;
        key(&mut a, KeyCode::Enter).await;
        assert_eq!(a.model, "faux/smoke");
        assert_eq!(a.effort, None);
        typed(&mut a, "/effort").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(a.picker.is_none(), "no levels to choose for this model");
    }

    #[test]
    fn end_of_turn_line_shows_the_level_the_turn_ran_with() {
        let mut a = app_with_models(80, 20);
        a.busy = true;
        a.on_event(json!({ "type": "busy", "busy": true, "model": "claude/claude-opus-5-5", "effort": "xhigh" }));
        a.turn_tokens = 50; // from the streamed messages, which miss the engine's side calls
        let record = json!({ "input_tokens": 1000, "output_tokens": 200, "cache_read": 3000, "cache_write": 0 });
        a.on_event(json!({ "type": "end", "error": null, "turn": record }));
        let out = a.capture.take().unwrap();
        assert!(out.contains(" · xhigh · "), "{out}");
        assert_eq!(a.session_tokens, 4200, "the kernel's turn totals replace the streamed estimate");
    }

    #[tokio::test]
    async fn done_opens_the_decision_picker_or_explains_itself() {
        let mut a = app(80, 20);
        typed(&mut a, "/done").await;
        key(&mut a, KeyCode::Enter).await;
        assert!(a.picker.is_none(), "no session yet: nothing to judge");
        a.session = Some("s1".into());
        typed(&mut a, "/done").await;
        key(&mut a, KeyCode::Enter).await;
        let items: Vec<String> = a.picker.as_ref().expect("picker open").items.iter().map(|(_, v)| v.clone()).collect();
        assert_eq!(items, ["accept", "more", "reshape", "drop"]);
        key(&mut a, KeyCode::Esc).await;
        a.command("/done maybe later").await.unwrap();
        assert!(a.notice.as_ref().is_some_and(|(t, _)| t.starts_with("usage: /done")), "{:?}", a.notice);
    }
}
