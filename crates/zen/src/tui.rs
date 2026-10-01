//! Interactive terminal app, inline like Claude Code, Codex and Pi.
//! The conversation is printed into normal terminal scrollback; a live region at the bottom
//! holds streaming text, status, the input box and the footer, redrawn in place.

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

use crate::client::{short, tool_summary, Client, Ws};
use crate::editor::Editor;
use crate::md::{self, line, Line, Md, Sty};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PLACEHOLDER: &str = "Ask zenbot to do something…";

const COMMANDS: &[(&str, &str)] = &[
    ("/new", "start a new session"),
    ("/resume", "switch to another session"),
    ("/model", "choose the model"),
    ("/rename", "rename this session: /rename <title>"),
    ("/archive", "archive this session and start a new one"),
    ("/upgrade", "update zenbot to the latest version and restart it"),
    ("/help", "keys and commands"),
    ("/exit", "quit zen"),
];

pub enum Start {
    New,
    Continue,
    Resume(Option<String>),
}

enum PickKind {
    Session,
    Model,
}

struct Picker {
    title: String,
    items: Vec<(String, String)>,
    selected: usize,
    kind: PickKind,
}

/// The live region at the bottom of the terminal.
#[derive(Default)]
struct Region {
    height: usize,
    caret_row: usize,
}

struct App {
    c: Client,
    tx: mpsc::UnboundedSender<(String, Value)>,
    sink: Option<SplitSink<Ws, Message>>,
    reader: Option<tokio::task::JoinHandle<()>>,
    session: Option<String>,
    title: String,
    model: String,
    default_model: String,
    models: Vec<String>,
    editor: Editor,
    picker: Option<Picker>,
    /// Highlighted row in the `/` command menu.
    menu_sel: usize,
    region: Region,
    busy: bool,
    status: String,
    spin: usize,
    turn_started: Instant,
    stream: String,
    committed: usize,
    md: Md,
    turn_tokens: i64,
    turn_model: String,
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

pub async fn run(c: Client, start: Start, model: Option<String>) -> Result<()> {
    let models = c.get("/api/models").await?;
    let default_model = models["default"].as_str().unwrap_or("").to_string();
    let model_ids: Vec<String> = models["models"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(String::from)).collect();
    let signed_in = models["authenticated"].as_object().is_some_and(|a| a.values().any(|v| v == true));

    let history = std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".zenbot/history"));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App {
        c,
        tx,
        sink: None,
        reader: None,
        session: None,
        title: String::new(),
        model: model.unwrap_or_else(|| default_model.clone()),
        default_model,
        models: model_ids,
        editor: Editor::new(history),
        picker: None,
        menu_sel: 0,
        region: Region::default(),
        busy: false,
        status: String::new(),
        spin: 0,
        turn_started: Instant::now(),
        stream: String::new(),
        committed: 0,
        md: Md::default(),
        turn_tokens: 0,
        turn_model: String::new(),
        session_tokens: 0,
        pending_prompt: None,
        aborting: false,
        notice: None,
        ctrl_c_at: None,
        upgrading: false,
        quit: false,
    };

    terminal::enable_raw_mode()?;
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    let mut out = std::io::stdout();
    execute!(out, EnableBracketedPaste)?;
    if enhanced {
        execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
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

fn term_width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(80).saturating_sub(1).max(20)
}

fn term_height() -> usize {
    terminal::size().map(|(_, h)| h as usize).unwrap_or(24).max(6)
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
        let max = term_height().saturating_sub(1);
        if lines.len() > max {
            let cut = lines.len() - max;
            lines.drain(..cut);
            caret_row = caret_row.saturating_sub(cut);
        }
        let n = lines.len();
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
        self.region = Region { height: n, caret_row };
    }

    fn flush(out: String) {
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
        Self::flush(out);
    }

    /// Print lines permanently into scrollback above the live region.
    fn commit(&mut self, lines: Vec<Line>) {
        if lines.is_empty() {
            return;
        }
        let mut out = String::from("\x1b[?2026h");
        self.erase(&mut out);
        for l in &lines {
            out.push_str(&md::to_ansi(l));
            out.push_str("\x1b[K\r\n");
        }
        self.paint_region(&mut out);
        out.push_str("\x1b[?2026l");
        Self::flush(out);
    }

    fn note(&mut self, text: impl Into<String>, sty: Sty) {
        self.notice = Some((text.into(), sty));
    }

    /// Build the live region: (lines, caret row, caret col, show caret).
    /// Commands matching what's typed, while the input is a bare `/word`.
    fn menu(&self) -> Vec<(&'static str, &'static str)> {
        let buf = &self.editor.buf;
        if !buf.starts_with('/') || buf.contains(' ') || buf.contains('\n') {
            return Vec::new();
        }
        COMMANDS.iter().filter(|(n, _)| n.starts_with(buf.as_str())).copied().collect()
    }

    fn compose(&self) -> (Vec<Line>, usize, usize, bool) {
        let w = term_width();
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

        let rule = line("─".repeat(w), Sty::Dim);
        lines.push(rule.clone());
        let (input, crow, ccol) = self.editor.render(w, PLACEHOLDER);
        let caret_row = lines.len() + crow;
        lines.extend(input);
        lines.push(rule);

        let menu = self.menu();
        if !menu.is_empty() {
            let sel = self.menu_sel.min(menu.len() - 1);
            for (i, (name, help)) in menu.iter().enumerate() {
                let mark = if i == sel { "› " } else { "  " };
                let help_sty = if i == sel { Sty::Plain } else { Sty::Dim };
                lines.push(vec![(format!("{mark}{name:<10}"), Sty::Accent), (help.to_string(), help_sty)]);
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
            let footer = format!("  {model} · {session} · {} tokens", fmt_tokens(self.session_tokens));
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
        self.connect(&id).await?;

        let w = term_width();
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
        let items: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (format!("{}{}", m, if *m == self.model { "  (current)" } else { "" }), m.clone()))
            .collect();
        let selected = self.models.iter().position(|m| *m == self.model).unwrap_or(0);
        self.picker = Some(Picker { title: "Choose a model".into(), items, selected, kind: PickKind::Model });
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
                self.note(format!("model: {value}"), Sty::Dim);
            }
        }
        Ok(())
    }

    // ---------- input ----------

    async fn on_terminal(&mut self, ev: Event) -> Result<()> {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k).await?,
            Event::Paste(s) if self.picker.is_none() => self.editor.insert(&s),
            Event::Resize(..) => {}
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
            match k.code {
                KeyCode::Up => {
                    self.menu_sel = sel.checked_sub(1).unwrap_or(menu.len() - 1);
                    return Ok(());
                }
                KeyCode::Down => {
                    self.menu_sel = (sel + 1) % menu.len();
                    return Ok(());
                }
                KeyCode::Tab => {
                    self.editor.set(menu[sel].0);
                    self.menu_sel = 0;
                    return Ok(());
                }
                KeyCode::Enter => {
                    let name = menu[sel].0;
                    self.menu_sel = 0;
                    if name == "/rename" {
                        // Needs an argument: complete it and let the user type the title.
                        self.editor.set("/rename ");
                    } else {
                        self.editor.set(name);
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
            KeyCode::Enter if alt || shift => self.editor.insert("\n"),
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
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.home(),
            KeyCode::End => self.editor.end(),
            KeyCode::Char('a') if ctrl => self.editor.home(),
            KeyCode::Char('e') if ctrl => self.editor.end(),
            KeyCode::Char('u') if ctrl => self.editor.kill_to_start(),
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
            let id = self.c.new_session(Some(self.model.clone())).await?;
            self.session = Some(id.clone());
            self.connect(&id).await?;
        } else if self.sink.is_none() {
            let id = self.session.clone().unwrap_or_default();
            self.connect(&id).await?;
        }
        if self.title.is_empty() {
            self.title = text.chars().take(40).collect::<String>().trim().to_string();
        }
        let w = term_width();
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
        let cmd = COMMANDS.iter().map(|(n, _)| *n).find(|n| *n == cmd).or_else(|| {
            let m: Vec<_> = COMMANDS.iter().map(|(n, _)| *n).filter(|n| n.starts_with(cmd)).collect();
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
                self.commit(vec![line("── new session ──", Sty::Dim), Vec::new()]);
            }
            Some("/resume") => self.open_session_picker().await?,
            Some("/model") => self.open_model_picker(),
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
                for (n, h) in COMMANDS {
                    out.push(vec![(format!("  {n:<10}"), Sty::Accent), (h.to_string(), Sty::Plain)]);
                }
                out.push(line("Keys", Sty::Bold));
                for (k, h) in [
                    ("enter", "send"),
                    ("alt+enter", "new line (also shift+enter, ctrl+j, or end a line with \\)"),
                    ("esc", "interrupt zenbot"),
                    ("↑ ↓", "previous prompts; in the / menu, choose a command (tab completes)"),
                    ("ctrl+c", "clear input; twice to exit"),
                    ("ctrl+d", "exit"),
                ] {
                    out.push(vec![(format!("  {k:<10}"), Sty::Accent), (h.to_string(), Sty::Plain)]);
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
        let w = term_width();
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
                let secs = self.turn_started.elapsed().as_secs_f32();
                out.push(line(format!("  {} · {} tokens · {:.1}s", self.turn_model, fmt_tokens(self.turn_tokens), secs), Sty::Dim));
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
}
