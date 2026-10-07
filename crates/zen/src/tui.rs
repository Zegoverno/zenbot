//! Interactive terminal app. A live region at the bottom holds status, the input box and the
//! footer.
//!
//! Full screen (default): zen owns the terminal's alternate screen. The conversation is kept as
//! entries and re-rendered at the current width, in a viewport above the live region that
//! scrolls with PgUp/PgDn or the mouse wheel. `/open <file>` shows a file in a side panel next to
//! the chat, reloaded when it changes. Each frame is drawn whole and only changed rows are
//! written (`screen.rs`), so nothing flickers.
//! Inline (`zen --inline` or ZEN_INLINE=1): the conversation is printed into normal terminal
//! scrollback and the live region (with the streaming text) follows it, like Claude Code, Codex
//! and Pi.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use zen_proto::text_of;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::terminal;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use unicode_width::UnicodeWidthStr;

use crate::client::{short, tool_summary, Client, NewSession, Ws};
use crate::editor::Editor;
use crate::files::Files;
use crate::md::{self, line, Line, Md, Sty};
use crate::screen::{self, Screen};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PLACEHOLDER: &str = "Ask zenbot to do something…";

const ALT_SCREEN_ON: &str = "\x1b[?1049h\x1b[H\x1b[2J";
const ALT_SCREEN_OFF: &str = "\x1b[?1049l";
/// Report mouse buttons and the wheel (SGR encoding), but not motion.
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1000l";
const PASTE_ON: &str = "\x1b[?2004h";
const PASTE_OFF: &str = "\x1b[?2004l";
/// Kitty keyboard protocol: report modified keys distinctly (DISAMBIGUATE_ESCAPE_CODES), and undo it.
const KEYS_PUSH: &str = "\x1b[>1u";
const KEYS_POP: &str = "\x1b[<1u";

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
    cmd("/files", "show or hide the folder tree (also ctrl+b); /files <dir> shows that folder instead of ~/.zenbot", false),
    cmd("/open", "show a file next to the chat: /open <path> (no path: the last file zenbot touched)", true),
    cmd("/close", "close the side panel", false),
    cmd("/mouse", "mouse wheel scrolling on/off (off lets the terminal select text)", false),
    cmd("/archive", "archive this session and start a new one", false),
    cmd("/upgrade", "update zenbot to the latest version and restart it", false),
    cmd("/restart", "restart zen on the installed version, back in this session", false),
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
    ("reshape", "the approach was wrong: rethink it"),
    ("drop", "stop: not worth continuing"),
];

struct Picker {
    title: String,
    items: Vec<(String, String)>,
    selected: usize,
    kind: PickKind,
}

/// One piece of the conversation, kept as its source so it can be re-rendered at any width
/// (a resize, or the side panel opening or closing).
enum Entry {
    User(String),
    /// Assistant text, as markdown.
    Md(String),
    ToolCall(String, Value),
    /// A tool's result: its first lines, how many more there are, and whether it failed.
    ToolResult { head: Vec<String>, more: usize, error: bool },
    /// Already styled lines (notes, help, end-of-turn lines); re-wrapped when too wide.
    Raw(Vec<Line>),
    /// Full screen: a run of tool calls between pieces of text, shown as one line until expanded.
    Work(Vec<Step>),
}

/// One tool call in a `Work` run, with its result once it arrives.
struct Step {
    name: String,
    args: Value,
    /// First lines, how many more, and whether it failed.
    result: Option<(Vec<String>, usize, bool)>,
}

/// Fold a tool call or result into a trailing `Work` run (full screen); anything else is added as is.
fn absorb(list: &mut Vec<Entry>, e: Entry) {
    match e {
        Entry::ToolCall(name, args) => {
            let step = Step { name, args, result: None };
            match list.last_mut() {
                Some(Entry::Work(steps)) => steps.push(step),
                _ => list.push(Entry::Work(vec![step])),
            }
        }
        Entry::ToolResult { head, more, error } => {
            if let Some(Entry::Work(steps)) = list.last_mut() {
                if let Some(step) = steps.iter_mut().find(|s| s.result.is_none()) {
                    step.result = Some((head, more, error));
                    return;
                }
            }
            list.push(Entry::ToolResult { head, more, error });
        }
        e => list.push(e),
    }
}

/// Which tab of the side panel is showing.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Files,
    Viewer,
}

/// Narrowest terminal (columns) that fits the chat and the side panel next to each other.
const SPLIT_MIN: usize = 60;

/// Largest file the side panel loads.
const PANEL_MAX_BYTES: u64 = 2 << 20;

/// A file shown next to the chat.
struct Panel {
    path: PathBuf,
    text: String,
    modified: Option<SystemTime>,
    /// First line shown.
    scroll: usize,
    /// Rendered lines, and the width they were rendered at.
    lines: Vec<Line>,
    lines_w: usize,
}

impl Panel {
    fn open(path: PathBuf) -> std::result::Result<Panel, String> {
        let mut p = Panel { path, text: String::new(), modified: None, scroll: 0, lines: Vec::new(), lines_w: 0 };
        p.load()?;
        Ok(p)
    }

    fn load(&mut self) -> std::result::Result<(), String> {
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
    fn reload_if_changed(&mut self) -> bool {
        let now = std::fs::metadata(&self.path).ok().and_then(|m| m.modified().ok());
        if now.is_some() && now != self.modified {
            return self.load().is_ok();
        }
        false
    }

    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.display().to_string())
    }

    /// The file's lines at `width` columns: markdown files rendered, others wrapped as they are.
    fn lines(&mut self, width: usize) -> &[Line] {
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
    /// Quit, then start the installed zen again on this session (`/restart`, or after `/upgrade`).
    restart: bool,
    /// The installed zen binary and its modification time when this one started, to notice a newer
    /// install (zenbot can upgrade itself from inside a session).
    installed: Option<(PathBuf, std::time::SystemTime)>,
    /// A newer install has already been announced.
    newer_noted: bool,
    /// Full screen: the conversation, and its lines rendered at `view_w` columns.
    entries: Vec<Entry>,
    view: Vec<Line>,
    view_w: usize,
    /// Full screen: lines scrolled up from the bottom of the conversation (0 follows it).
    scroll: usize,
    /// Conversation lines at the last frame, to keep a scrolled-up view still as lines arrive.
    last_total: usize,
    screen: Screen,
    panel: Option<Panel>,
    /// The last file a tool read or changed: what `/open` with no path shows.
    last_file: Option<String>,
    /// Mouse wheel reporting is on (the terminal then needs shift+drag to select text).
    mouse: bool,
    /// When the running tool started, for its elapsed time in the status line.
    tool_since: Option<Instant>,
    /// The side panel is open (it is also open whenever a file is).
    side: bool,
    /// The folder tree of the Files tab, created when first shown.
    files: Option<Files>,
    /// Where the folder tree starts: zenbot's home (~/.zenbot) unless `/files <dir>` changes it.
    files_root: PathBuf,
    tab: Tab,
    /// Keys go to the side panel, not the input (tab switches).
    side_focus: bool,
    /// Show every step of each run of tool calls (ctrl+o) instead of one line per run.
    expand_work: bool,
    /// Tool calls and distinct files written or edited in the running turn, for the status line.
    turn_tools: usize,
    turn_files: std::collections::HashSet<String>,
}

/// A path the owner typed, with a leading `~` meaning the home folder.
fn expand_home(raw: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    match raw.strip_prefix("~/") {
        Some(rest) => PathBuf::from(&home).join(rest),
        None if raw == "~" => PathBuf::from(&home),
        None => PathBuf::from(raw),
    }
}

/// Text from outside (the kernel, a model, a tool, a file) made safe to show: see `md::sanitize`.
fn clean(s: &str) -> String {
    md::sanitize(s).into_owned()
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
            restart: false,
            installed: None,
            newer_noted: false,
            entries: Vec::new(),
            view: Vec::new(),
            view_w: 0,
            scroll: 0,
            last_total: 0,
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
    app.installed = installed_zen().and_then(|p| Some((p.clone(), std::fs::metadata(&p).ok()?.modified().ok()?)));
    if let Some(home) = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zenbot")).filter(|p| p.is_dir()) {
        app.files_root = home;
    }

    terminal::enable_raw_mode()?;
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    app.enhanced = enhanced;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal(inline, enhanced);
        default_hook(info);
    }));
    let mut out = std::io::stdout();
    if let Err(e) = out.write_all(setup_sequence(inline, enhanced).as_bytes()).and_then(|_| out.flush()) {
        restore_terminal(inline, enhanced);
        return Err(e.into());
    }

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
        let mut watch = tokio::time::interval(Duration::from_millis(500));
        let mut install_check = tokio::time::interval(Duration::from_secs(5));
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
                // The side panel follows its file as it changes (e.g. while zenbot edits it).
                _ = watch.tick(), if app.panel.is_some() => {
                    if app.panel.as_mut().is_some_and(Panel::reload_if_changed) {
                        app.draw();
                    }
                }
                _ = install_check.tick(), if !app.newer_noted && !app.upgrading => {
                    if app.newer_installed() {
                        app.newer_noted = true;
                        app.commit(vec![line("a new zen is installed · /restart to load it (this session continues)", Sty::Warn), Vec::new()]);
                        app.draw();
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if inline {
        let mut s = String::new();
        app.erase(&mut s);
        let _ = out.write_all(s.as_bytes());
    }
    restore_terminal(inline, enhanced);
    let bye = if app.restart {
        Some("restarting zen…".to_string())
    } else {
        app.session.as_deref().map(|id| format!("session {} · resume with: zen -r {}", short(id), short(id)))
    };
    if let Some(bye) = bye {
        let _ = out.write_all(format!("{}\r\n", md::to_ansi(&line(bye, Sty::Dim))).as_bytes());
        let _ = out.flush();
    }
    if result.is_ok() && app.restart {
        let bin = installed_zen().context("can't find the zen binary to restart")?;
        let mut cmd = std::process::Command::new(&bin);
        cmd.args(restart_args(app.session.as_deref(), inline)).env("ZEN_URL", &app.c.url).env("ZEN_TOKEN", &app.c.token);
        // exec only returns on failure: on success this process becomes the new zen.
        let err = std::os::unix::process::CommandExt::exec(&mut cmd);
        return Err(anyhow::anyhow!("couldn't restart {}: {err}", bin.display()));
    }
    result
}

/// Escape codes that set the terminal up for zen: bracketed paste, modified keys when the
/// terminal reports them, and in full screen the alternate screen (which leaves the terminal's
/// own scrollback untouched) with mouse reporting.
fn setup_sequence(inline: bool, enhanced: bool) -> String {
    let mut s = String::from(PASTE_ON);
    if enhanced {
        s.push_str(KEYS_PUSH);
    }
    if !inline {
        s.push_str(ALT_SCREEN_ON);
        s.push_str(MOUSE_ON);
    }
    s
}

/// Escape codes that undo `setup_sequence`, and show the caret.
fn restore_sequence(inline: bool, enhanced: bool) -> String {
    let mut s = String::new();
    if !inline {
        s.push_str(MOUSE_OFF);
        s.push_str(ALT_SCREEN_OFF);
    }
    if enhanced {
        s.push_str(KEYS_POP);
    }
    s.push_str(PASTE_OFF);
    s.push_str("\x1b[?25h");
    s
}

/// Give the terminal back as zen found it: on a normal exit, an error, or a panic.
fn restore_terminal(inline: bool, enhanced: bool) {
    let mut out = std::io::stdout();
    let _ = out.write_all(restore_sequence(inline, enhanced).as_bytes());
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
}

/// The zen to restart into: the installed one (`~/.zenbot/bin/zen`, where upgrades put it), else
/// this binary's own path (on Linux, a replaced binary's path ends in " (deleted)").
fn installed_zen() -> Option<PathBuf> {
    let installed = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zenbot/bin/zen"));
    if let Some(p) = installed.filter(|p| p.is_file()) {
        return Some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let s = exe.to_string_lossy();
    Some(PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(&s)))
}

/// Arguments for the restarted zen: back in the same session (or a new one), same display mode.
/// The kernel URL and token go in the environment, so the token never shows in `ps`.
fn restart_args(session: Option<&str>, inline: bool) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(id) = session {
        args.extend(["--resume".to_string(), id.to_string()]);
    }
    if inline {
        args.push("--inline".into());
    }
    args
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

    /// Redraw: the whole frame in full screen, the live region in place inline.
    fn draw(&mut self) {
        if !self.inline {
            self.frame();
            return;
        }
        let mut out = String::from("\x1b[?2026h");
        self.erase(&mut out);
        self.paint_region(&mut out);
        out.push_str("\x1b[?2026l");
        self.flush(out);
    }

    fn side_open(&self) -> bool {
        self.side || self.panel.is_some()
    }

    /// Open the side panel on the Files tab with the keys, or close it (and the file in it).
    fn toggle_side(&mut self) {
        if self.inline {
            self.note("the side panel needs full screen: run zen without --inline", Sty::Warn);
        } else if self.side_open() {
            self.side = false;
            self.panel = None;
            self.side_focus = false;
        } else {
            if self.width() < SPLIT_MIN {
                self.note(format!("the terminal is too narrow for the side panel (needs {SPLIT_MIN} columns)"), Sty::Warn);
            }
            self.side = true;
            self.tab = Tab::Files;
            self.side_focus = true;
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        match Panel::open(path) {
            Ok(p) => {
                self.panel = Some(p);
                self.tab = Tab::Viewer;
            }
            Err(e) => self.note(e, Sty::Err),
        }
    }

    /// Open or fold the selected row of the tree, as Enter and a click do.
    fn activate_row(&mut self) {
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
    fn side_key(&mut self, k: KeyEvent) -> bool {
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        if k.code == KeyCode::Esc {
            self.side_focus = false;
            return true;
        }
        let page = self.viewport_rows().saturating_sub(3).max(1) as isize;
        if self.tab == Tab::Viewer && self.panel.is_none() {
            self.tab = Tab::Files;
        }
        match self.tab {
            Tab::Files => {
                if self.files.is_none() {
                    self.files = Some(Files::new(self.files_root.clone()));
                }
                match k.code {
                    KeyCode::Up => self.files.as_mut().unwrap().move_by(-1),
                    KeyCode::Down => self.files.as_mut().unwrap().move_by(1),
                    KeyCode::PageUp => self.files.as_mut().unwrap().move_by(-page),
                    KeyCode::PageDown => self.files.as_mut().unwrap().move_by(page),
                    KeyCode::Home => self.files.as_mut().unwrap().sel = 0,
                    KeyCode::End => {
                        let f = self.files.as_mut().unwrap();
                        f.sel = f.rows.len().saturating_sub(1);
                    }
                    KeyCode::Right => {
                        let f = self.files.as_mut().unwrap();
                        if !f.expand() && f.selected().is_some_and(|r| !r.dir) {
                            self.activate_row();
                        }
                    }
                    KeyCode::Enter => self.activate_row(),
                    KeyCode::Left => self.files.as_mut().unwrap().collapse_or_parent(),
                    KeyCode::Char('.') => self.files.as_mut().unwrap().toggle_hidden(),
                    KeyCode::Char('r') => self.files.as_mut().unwrap().rebuild(),
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
    fn click_side(&mut self, x: usize, row: usize) {
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
    fn tab_bar(&self) -> Line {
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
    fn side_lines(&mut self, pw: usize, vh: usize) -> Vec<Line> {
        if self.tab == Tab::Viewer && self.panel.is_none() {
            self.tab = Tab::Files;
        }
        let mut out = vec![self.tab_bar()];
        let body = vh.saturating_sub(2); // below the tab strip, above the hint row
        match self.tab {
            Tab::Files => {
                if self.files.is_none() {
                    self.files = Some(Files::new(self.files_root.clone()));
                }
                let focus = self.side_focus;
                let f = self.files.as_mut().unwrap();
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
                (false, _) => "tab: use this panel · ctrl+b: close",
                (true, Tab::Files) => "↑↓ move · →/enter open · ← fold · . hidden · esc/tab chat",
                (true, Tab::Viewer) => "↑↓ scroll · ← files · x close file · esc/tab chat",
            };
            out.push(vec![(hint.to_string(), Sty::Dim)]);
        }
        out
    }

    /// Columns for the chat and for the side panel (0 when it's closed or the screen is too narrow).
    fn columns(&self) -> (usize, usize) {
        let total = self.width();
        if self.side_open() && !self.inline && total >= SPLIT_MIN {
            let panel = (total - 3) / 2;
            (total - 3 - panel, panel)
        } else {
            (total, 0)
        }
    }

    fn render_entry(e: &Entry, w: usize, expanded: bool) -> Vec<Line> {
        match e {
            Entry::User(text) => Self::render_user(text, w),
            Entry::Md(text) => {
                let mut out = Md::default().render(text.trim_end(), w);
                out.push(Vec::new());
                out
            }
            Entry::ToolCall(name, args) => Self::render_tool_call(name, args, w),
            Entry::ToolResult { head, more, error } => Self::render_tool_result(head, *more, *error, w),
            Entry::Work(steps) => Self::render_work(steps, w, expanded),
            Entry::Raw(lines) => lines
                .iter()
                .flat_map(|l| {
                    if l.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum::<usize>() <= w {
                        vec![l.clone()]
                    } else {
                        md::wrap(l.clone(), w, (String::new(), Sty::Plain), (String::new(), Sty::Plain))
                    }
                })
                .collect(),
        }
    }

    /// Add to the conversation: into the transcript in full screen, into scrollback inline.
    fn push(&mut self, e: Entry) {
        self.push_all(vec![e]);
    }

    /// Render the transcript again when the chat width changed.
    fn sync_view(&mut self) {
        let w = self.columns().0;
        if self.view_w != w {
            let x = self.expand_work;
            self.view = self.entries.iter().flat_map(|e| Self::render_entry(e, w, x)).collect();
            self.view_w = w;
        }
    }

    /// Rows the conversation gets above the live region at the current size.
    fn viewport_rows(&self) -> usize {
        let region = self.compose().0.len().min(self.height().saturating_sub(1));
        self.height().saturating_sub(region).max(1)
    }

    fn scroll_chat(&mut self, up: bool, n: usize) {
        self.scroll = if up { self.scroll + n } else { self.scroll.saturating_sub(n) };
    }

    fn scroll_panel(&mut self, up: bool, n: usize) {
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

    /// Full screen: compose every row (conversation and side panel above, live region below)
    /// and write the rows that changed.
    fn frame(&mut self) {
        self.sync_view();
        let (cw, pw) = self.columns();
        let h = self.height();
        let (mut region, mut caret_row, caret_col, show_caret) = self.compose();
        let max = h.saturating_sub(1);
        if region.len() > max {
            let cut = region.len() - max;
            region.drain(..cut);
            caret_row = caret_row.saturating_sub(cut);
        }
        let vh = h - region.len();

        // The conversation, with the streaming reply at the end: all of it, as it arrives.
        let live = if self.busy && !self.stream.is_empty() { Md::default().render(&self.stream, cw) } else { Vec::new() };
        let total = self.view.len() + live.len();
        if self.scroll > 0 && total > self.last_total {
            self.scroll += total - self.last_total; // scrolled up: keep the view where it is
        }
        self.last_total = total;
        self.scroll = self.scroll.min(total.saturating_sub(vh));
        let end = total - self.scroll;
        let start = end.saturating_sub(vh);
        let at = |i: usize| if i < self.view.len() { &self.view[i] } else { &live[i - self.view.len()] };
        let mut chat: Vec<Line> = (start..end).map(|i| at(i).clone()).collect();
        if self.scroll > 0 && !chat.is_empty() {
            let last = chat.len() - 1;
            chat[last] = line(format!("↓ {} more lines · PgDn", self.scroll), Sty::Accent);
        }

        // The side panel: tabs on top, the folder tree or the open file below.
        let side: Vec<Line> = if pw > 0 { self.side_lines(pw, vh) } else { Vec::new() };

        let mut rows: Vec<String> = (0..vh)
            .map(|r| screen::row(chat.get(r), (pw > 0).then(|| (side.get(r), pw)), cw))
            .collect();
        rows.extend(region.iter().map(|l| md::to_ansi(&screen::fit(l, self.width(), false))));
        let caret = show_caret.then_some((vh + caret_row, caret_col));
        let out = self.screen.frame(rows, caret);
        self.flush(out);
    }

    /// Add lines to the conversation (see `push`).
    fn commit(&mut self, lines: Vec<Line>) {
        self.push(Entry::Raw(lines));
    }

    /// Inline: print lines permanently into scrollback above the live region. Lines wider than the
    /// screen are wrapped here, so every printed line is exactly one row and `filled` stays true.
    fn print(&mut self, lines: Vec<Line>) {
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
        self.notice = Some((clean(&text.into()), sty));
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

        // Streaming text that hasn't completed a line yet, rendered in the state the committed
        // lines left (e.g. inside a code fence), without changing it.
        // (Full screen shows it in the conversation instead.)
        if self.inline && self.busy && self.committed < self.stream.len() {
            let mut m = self.md;
            let partial = m.render(&self.stream[self.committed..], w);
            let skip = partial.len().saturating_sub(6);
            lines.extend(partial.into_iter().skip(skip));
        }
        if self.busy {
            let secs = self.turn_started.elapsed().as_secs();
            // How long the running tool has taken, so a long one (a subagent) visibly progresses.
            let tool = self.tool_since.map(|t| t.elapsed().as_secs()).filter(|s| *s > 0).map(|s| format!(" ({s}s)")).unwrap_or_default();
            let mut work = String::new();
            if self.turn_tools > 0 {
                work.push_str(&format!(" · {} tool{}", self.turn_tools, if self.turn_tools == 1 { "" } else { "s" }));
            }
            if !self.turn_files.is_empty() {
                work.push_str(&format!(" · {} file{} edited", self.turn_files.len(), if self.turn_files.len() == 1 { "" } else { "s" }));
            }
            lines.push(vec![
                (format!("{} ", SPINNER[self.spin % SPINNER.len()]), Sty::Accent),
                (self.status.chars().take(w.saturating_sub(60)).collect(), Sty::Plain),
                (format!("{tool}  ·  turn {secs}s{work} · esc to interrupt"), Sty::Dim),
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
        self.title = clean(s["title"].as_str().unwrap_or(""));
        self.model = s["model"].as_str().unwrap_or(&self.default_model).to_string();
        self.effort = s["effort"].as_str().map(String::from);
        self.connect(&id).await?;
        self.show_history(&s);
        Ok(())
    }

    /// Show a session's latest messages and total its tokens for the footer.
    fn show_history(&mut self, s: &Value) {
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
            self.session_tokens += ["input", "output", "cacheRead", "cacheWrite"].iter().map(|k| m["usage"][*k].as_i64().unwrap_or(0)).sum::<i64>();
        }
        self.push_all(entries);
        if s["busy"] == true {
            self.busy = true;
            self.status = "Working".into();
            self.turn_started = Instant::now();
        }
    }

    /// Add several entries with one redraw.
    fn push_all(&mut self, entries: Vec<Entry>) {
        if self.inline {
            let w = self.width();
            let lines = entries.iter().flat_map(|e| Self::render_entry(e, w, true)).collect();
            self.print(lines);
            return;
        }
        // Tool calls fold into the run before them, which changes lines already rendered.
        for e in entries {
            absorb(&mut self.entries, e);
        }
        self.view_w = 0;
        self.draw();
    }

    fn render_user(text: &str, w: usize) -> Vec<Line> {
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

    /// A run of tool calls: one line (the latest step and a count) or, expanded, every step with its result.
    fn render_work(steps: &[Step], w: usize, expanded: bool) -> Vec<Line> {
        let failed = steps.iter().filter(|s| s.result.as_ref().is_some_and(|r| r.2)).count();
        let n = steps.len();
        let count = format!("{n} step{}", if n == 1 { "" } else { "s" });
        if expanded {
            let mut out = vec![vec![("▾ ".to_string(), Sty::Accent), (format!("{count} · ctrl+o to fold"), Sty::Dim)]];
            for s in steps {
                out.extend(Self::render_tool_call(&s.name, &s.args, w));
                if let Some((head, more, error)) = &s.result {
                    out.extend(Self::render_tool_result(head, *more, *error, w));
                }
            }
            return out;
        }
        let last = &steps[n - 1];
        let summary = tool_summary(&last.name, &last.args);
        let mut tail = format!("  · {count}");
        if failed > 0 {
            tail.push_str(&format!(" · {failed} failed"));
        }
        tail.push_str(" · ctrl+o");
        let room = w.saturating_sub(UnicodeWidthStr::width(tail.as_str()) + 3).max(8);
        let mut text: String = summary.chars().take(room).collect();
        while UnicodeWidthStr::width(text.as_str()) > room {
            text.pop();
        }
        vec![vec![("▸ ".to_string(), Sty::Accent), (text, Sty::Plain), (tail, if failed > 0 { Sty::Err } else { Sty::Dim })], Vec::new()]
    }

    /// A tool result as shown: its first three non-empty lines and how many more there are.
    fn tool_result_entry(m: &Value) -> Entry {
        let text = clean(&text_of(&m["content"]));
        let body: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let head = body.iter().take(3).map(|l| l.chars().take(400).collect()).collect();
        Entry::ToolResult { head, more: body.len().saturating_sub(3), error: m["isError"] == true }
    }

    fn render_tool_result(head: &[String], more: usize, error: bool, w: usize) -> Vec<Line> {
        let sty = if error { Sty::Err } else { Sty::Dim };
        let mut out = Vec::new();
        if head.is_empty() {
            out.push(line("  └ (no output)", Sty::Dim));
        }
        for (i, l) in head.iter().enumerate() {
            let max = w.saturating_sub(5);
            let mut s: String = l.chars().take(max).collect();
            if UnicodeWidthStr::width(s.as_str()) > max {
                s = s.chars().take(max.saturating_sub(1)).collect();
            }
            out.push(vec![(if i == 0 { "  └ " } else { "    " }.to_string(), Sty::Dim), (s, sty)]);
        }
        if more > 0 {
            out.push(line(format!("    … {more} more lines"), Sty::Dim));
        }
        out.push(Vec::new());
        out
    }

    /// A stored message as conversation entries.
    fn message_entries(m: &Value) -> Vec<Entry> {
        match m["role"].as_str() {
            Some("user") => vec![Entry::User(clean(&text_of(&m["content"])))],
            Some("assistant") => {
                let mut out = Vec::new();
                for c in m["content"].as_array().into_iter().flatten() {
                    match c["type"].as_str() {
                        Some("text") if !c["text"].as_str().unwrap_or("").trim().is_empty() => {
                            out.push(Entry::Md(clean(c["text"].as_str().unwrap_or(""))));
                        }
                        Some("toolCall") => out.push(Entry::ToolCall(c["name"].as_str().unwrap_or("").to_string(), c["arguments"].clone())),
                        _ => {}
                    }
                }
                if m["stopReason"] == "error" {
                    out.push(Entry::Raw(vec![line(clean(m["errorMessage"].as_str().unwrap_or("error")), Sty::Err), Vec::new()]));
                }
                out
            }
            Some("toolResult") => vec![Self::tool_result_entry(m)],
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
                let title = clean(s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(untitled)"));
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
                self.screen.invalidate();
            }
            Event::Mouse(m) => {
                // The wheel scrolls whichever side is under the pointer.
                let (cw, pw) = self.columns();
                if m.kind == MouseEventKind::Down(MouseButton::Left) {
                    if pw > 0 && m.column as usize >= cw + 3 && (m.row as usize) < self.viewport_rows() {
                        self.click_side(m.column as usize - (cw + 3), m.row as usize);
                    }
                    self.draw();
                    return Ok(());
                }
                let up = match m.kind {
                    MouseEventKind::ScrollUp => true,
                    MouseEventKind::ScrollDown => false,
                    _ => return Ok(()),
                };
                if pw > 0 && m.column as usize > cw + 1 && (m.row as usize) < self.viewport_rows() {
                    self.scroll_panel(up, 3);
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
            if k.code == KeyCode::Tab && plain && self.side_open() && (self.side_focus || (self.editor.is_empty() && self.menu().is_empty())) {
                self.side_focus = !self.side_focus;
                return Ok(());
            }
            if self.side_focus && self.side_open() && self.side_key(k) {
                return Ok(());
            }
        }

        // Scrolling (full screen): PgUp/PgDn move the chat, alt+↑↓ and alt+PgUp/PgDn the side panel.
        if !self.inline {
            let page = self.viewport_rows().saturating_sub(2).max(1);
            let panel = self.columns().1 > 0;
            match k.code {
                KeyCode::Up | KeyCode::Down if alt && panel => {
                    self.scroll_panel(k.code == KeyCode::Up, 1);
                    return Ok(());
                }
                KeyCode::PageUp | KeyCode::PageDown if alt && panel => {
                    self.scroll_panel(k.code == KeyCode::PageUp, page);
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
        self.scroll = 0; // back to the latest when you send
        self.push(Entry::User(text.clone()));
        self.pending_prompt = Some(text.clone());
        self.busy = true;
        self.status = "Working".into();
        self.turn_started = Instant::now();
        self.turn_tokens = 0;
        self.turn_tools = 0;
        self.turn_files.clear();
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
                    ("ctrl+b", "open or close the side panel; tab moves the keys between chat and panel"),
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
            Some("/open") => self.open_panel(arg),
            Some("/files") if arg.is_empty() => self.toggle_side(),
            Some("/files") => self.show_folder(arg),
            Some("/close") => {
                if self.side_open() {
                    self.side = false;
                    self.panel = None;
                    self.side_focus = false;
                } else {
                    self.note("no side panel is open", Sty::Dim);
                }
            }
            Some("/mouse") if !self.inline => {
                self.mouse = !self.mouse;
                self.flush((if self.mouse { MOUSE_ON } else { MOUSE_OFF }).to_string());
                let state = if self.mouse { "on: the wheel scrolls (to select text: /mouse off, or shift+drag in most terminals)" } else { "off: drag selects text; PgUp/PgDn scroll" };
                self.note(format!("mouse {state}"), Sty::Dim);
            }
            Some("/mouse") => self.note("inline mode leaves the mouse to the terminal", Sty::Dim),
            Some("/upgrade") => self.start_upgrade(),
            Some("/restart") => {
                self.restart = true;
                self.quit = true;
            }
            Some("/exit") => self.quit = true,
            _ => self.note(format!("unknown command {input}; try /help"), Sty::Warn),
        }
        Ok(())
    }

    // ---------- side panel ----------

    /// `/open [path]`: show a file next to the chat; with no path, the last file a tool touched.
    fn open_panel(&mut self, arg: &str) {
        if self.inline {
            self.note("the side panel needs full screen: run zen without --inline", Sty::Warn);
            return;
        }
        let Some(raw) = (if arg.is_empty() { self.last_file.clone() } else { Some(arg.to_string()) }) else {
            self.note("usage: /open <path> (no file touched yet in this session)", Sty::Warn);
            return;
        };
        match Panel::open(expand_home(&raw)) {
            Ok(p) => {
                if self.width() < SPLIT_MIN {
                    self.note(format!("the terminal is too narrow for the side panel (needs {SPLIT_MIN} columns)"), Sty::Warn);
                }
                self.panel = Some(p);
                self.tab = Tab::Viewer;
            }
            Err(e) => self.note(e, Sty::Err),
        }
    }

    /// `/files <dir>`: root the folder tree at `dir` and show it.
    fn show_folder(&mut self, arg: &str) {
        if self.inline {
            self.note("the side panel needs full screen: run zen without --inline", Sty::Warn);
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

    // ---------- upgrade ----------

    /// The installed zen changed since this one started.
    fn newer_installed(&self) -> bool {
        let Some((path, started)) = &self.installed else { return false };
        std::fs::metadata(path).and_then(|m| m.modified()).is_ok_and(|now| now != *started)
    }

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
        let text = clean(ev["text"].as_str().or(ev["line"].as_str()).unwrap_or(""));
        match ev["type"].as_str().unwrap_or("") {
            "update_available" => self.commit(vec![line(text, Sty::Warn), Vec::new()]),
            "upgrade_log" => self.commit(vec![line(text, Sty::Dim)]),
            "upgrade_done" => {
                self.upgrading = false;
                let ok = ev["ok"] == true;
                self.commit(vec![line(text, if ok { Sty::Accent } else { Sty::Err }), Vec::new()]);
                // The upgrade installed a new zen too: load it, back in this session.
                if ok && self.newer_installed() {
                    self.restart = true;
                    self.quit = true;
                    return;
                }
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
                    Some("user") if m["kernel"] == true => {
                        // The kernel talking to the model (a verifier's instructions), not the owner.
                        let text = clean(&text_of(&m["content"]));
                        let mut out: Vec<Line> = text.lines().map(|l| line(format!("  zen › {l}"), Sty::Dim)).collect();
                        out.push(Vec::new());
                        self.commit(out);
                    }
                    Some("user") => {
                        let text = clean(&text_of(&m["content"]));
                        if self.pending_prompt.as_deref() == Some(text.as_str()) {
                            self.pending_prompt = None;
                        } else {
                            self.push(Entry::User(text));
                        }
                    }
                    Some("assistant") => {
                        let mut entries = Vec::new();
                        let full = clean(&m["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect::<String>());
                        if self.inline {
                            // Inline, the streamed lines are already in scrollback: print the rest.
                            let mut out = Vec::new();
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
                            entries.push(Entry::Raw(out));
                        } else {
                            let text = if full.trim().is_empty() { self.stream.clone() } else { full };
                            if !text.trim().is_empty() {
                                entries.push(Entry::Md(text));
                            }
                        }
                        self.stream.clear();
                        self.committed = 0;
                        self.md = Md::default();
                        for c in m["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "toolCall") {
                            entries.push(Entry::ToolCall(c["name"].as_str().unwrap_or("").to_string(), c["arguments"].clone()));
                        }
                        if m["stopReason"] == "error" && !self.aborting {
                            entries.push(Entry::Raw(vec![line(clean(m["errorMessage"].as_str().unwrap_or("error")), Sty::Err)]));
                        }
                        let u = &m["usage"];
                        self.turn_tokens += ["input", "output", "cacheRead", "cacheWrite"].iter().map(|k| u[*k].as_i64().unwrap_or(0)).sum::<i64>();
                        self.turn_model = m["model"].as_str().unwrap_or("").to_string();
                        self.push_all(entries);
                    }
                    Some("toolResult") => self.push(Self::tool_result_entry(m)),
                    _ => {}
                }
            }
            "delta" => {
                self.stream.push_str(&md::sanitize(ev["delta"].as_str().unwrap_or("")));
                self.status = "Writing".into();
                self.tool_since = None;
                if !self.inline {
                    self.draw(); // the whole reply so far shows in the conversation
                    return;
                }
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
            "busy" => {
                // A turn the kernel started also counts.
                if !self.busy {
                    self.busy = true;
                    self.turn_started = std::time::Instant::now();
                    self.turn_tokens = 0;
                    self.turn_tools = 0;
                    self.turn_files.clear();
                }
                self.turn_effort = ev["effort"].as_str().map(String::from);
            }
            "questions" => {
                let mut out = vec![line("── Questions ──", Sty::Dim)];
                for (i, q) in ev["questions"].as_array().into_iter().flatten().enumerate() {
                    out.push(line(format!("{}. {}", i + 1, clean(q["question"].as_str().unwrap_or(""))), Sty::Plain));
                    for (j, o) in q["options"].as_array().into_iter().flatten().enumerate() {
                        let rec = if j == 0 { "  (recommended)" } else { "" };
                        out.push(line(format!("   {}) {}{rec}", (b'a' + j as u8) as char, clean(o.as_str().unwrap_or(""))), Sty::Plain));
                    }
                }
                out.push(Vec::new());
                self.commit(out);
            }
            "status" => {
                self.status = clean(ev["text"].as_str().unwrap_or("Working"));
                self.draw();
            }
            "child_end" => {
                let r = &ev["turn"];
                self.session_tokens += ["input_tokens", "output_tokens", "cache_read", "cache_write"].iter().map(|k| r[*k].as_i64().unwrap_or(0)).sum::<i64>();
            }
            "idle" => {
                self.busy = false;
                self.draw();
            }
            "thinking" => {
                self.status = "Thinking".into();
            }
            "tool_start" => {
                let name = ev["name"].as_str().unwrap_or("");
                self.status = format!("Running {}", tool_summary(name, &ev["args"]));
                self.tool_since = Some(Instant::now());
                self.turn_tools += 1;
                if matches!(name.rsplit("__").next(), Some("write" | "edit")) {
                    if let Some(path) = ev["args"]["path"].as_str() {
                        self.turn_files.insert(path.to_string());
                    }
                }
                // Remember the file, for `/open` with no path.
                if matches!(name.rsplit("__").next(), Some("read" | "write" | "edit")) {
                    if let Some(path) = ev["args"]["path"].as_str() {
                        self.last_file = Some(path.to_string());
                    }
                }
                self.draw();
            }
            "tool_end" => {
                self.status = "Working".into();
                self.tool_since = None;
                // A tool may have changed the file in the side panel: show it now, not on the next poll.
                if self.panel.as_mut().is_some_and(Panel::reload_if_changed) {
                    self.draw();
                }
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
                self.tool_since = None;
                if self.aborting {
                    if !self.inline {
                        if !self.stream.trim().is_empty() {
                            out.extend(Md::default().render(self.stream.trim_end(), w));
                        }
                    } else if self.committed < self.stream.len() {
                        let rest = self.stream[self.committed..].to_string();
                        out.extend(self.md.render(rest.trim_end(), w));
                    }
                    out.push(line("interrupted", Sty::Warn));
                } else if let Some(e) = ev["error"].as_str() {
                    out.push(line(clean(e), Sty::Err));
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
                // The workflow may continue on its own (verify, or the work after approval).
                self.busy = ev["next"] == true;
                if self.busy {
                    self.status = "Continuing".into();
                }
                self.commit(out);
            }
            "error" => {
                self.busy = false;
                self.commit(vec![line(clean(ev["error"].as_str().unwrap_or("error")), Sty::Err), Vec::new()]);
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
    use crate::test_util::TempDir;

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
        a.inline = true;
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
        let rows = a.screen.rows();
        assert_eq!(rows.len(), 20, "every row of the screen is drawn");
        assert!(rows[rows.len() - 2].contains('╰'), "the input box sits at the bottom, above the footer: {rows:#?}");
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
        let dir = TempDir::new("banner");
        let path = dir.join("version");
        assert_eq!(banner_text(None), "zen · zenbot");
        assert_eq!(banner_text(Some(&path.join("missing"))), "zen · zenbot");
        std::fs::write(&path, "775d408\n").unwrap();
        assert_eq!(banner_text(Some(&path)), "zen · zenbot · 775d408");
        std::fs::write(&path, " \n").unwrap();
        assert_eq!(banner_text(Some(&path)), "zen · zenbot");
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

    #[test]
    fn questions_render_with_the_recommended_option_first() {
        let mut a = app_with_models(80, 30);
        a.busy = true;
        a.on_event(json!({ "type": "questions", "questions": [{ "question": "Which flag name?", "options": ["--json", "--format json"] }] }));
        a.on_event(json!({ "type": "status", "text": "verifying: running 1 check(s)" }));
        assert_eq!(a.status, "verifying: running 1 check(s)");
        a.on_event(json!({ "type": "end", "error": null, "turn": {} }));
        assert!(!a.busy);
        let out = a.capture.take().unwrap();
        for want in ["── Questions ──", "1. Which flag name?", "a) --json  (recommended)", "b) --format json"] {
            assert!(out.contains(want), "missing {want:?} in {out}");
        }
    }

    #[test]
    fn a_kernel_started_turn_shows_as_busy() {
        let mut a = app_with_models(80, 20);
        assert!(!a.busy);
        a.on_event(json!({ "type": "busy", "busy": true, "model": "claude/claude-opus-5-5", "effort": "high" }));
        assert!(a.busy, "a turn started elsewhere shows here too");
        a.on_event(json!({ "type": "idle" }));
        assert!(!a.busy);
    }

    #[test]
    fn a_streaming_partial_line_inside_a_code_fence_renders_as_code() {
        let mut a = app(80, 20);
        a.inline = true;
        a.busy = true;
        a.on_event(json!({ "type": "delta", "delta": "```\nlet x = 1;" }));
        let (lines, _, _, _) = a.compose();
        let code = lines.iter().find(|l| l.iter().any(|(t, _)| t.contains("let x = 1;"))).expect("partial line shown");
        assert!(code.iter().any(|(t, s)| t.contains("let x") && *s == Sty::Code), "{code:?}");
        // Rendering the partial line leaves the committed state alone.
        a.on_event(json!({ "type": "delta", "delta": "\n```\nafter\n" }));
        let out = a.capture.take().unwrap();
        assert!(out.contains("\x1b[36mlet x = 1;") && out.contains("\r\nafter"), "{out:?}");
    }

    #[test]
    fn full_screen_streams_the_whole_reply_into_the_conversation() {
        let mut a = app(80, 30);
        a.busy = true;
        let reply: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        a.on_event(json!({ "type": "delta", "delta": reply }));
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("line 1") && rows.contains("line 12"), "not only the last few lines: {rows}");
        assert!(a.compose().0.iter().all(|l| !l.iter().any(|(t, _)| t.contains("line 12"))), "and not in the live region");
        // The finished message replaces the stream, rendered once.
        a.on_event(json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "text", "text": reply }] } }));
        let rows = a.screen.rows().join("\n");
        assert_eq!(rows.matches("line 12").count(), 1, "{rows}");
    }

    fn tool_turn(a: &mut App, n: usize) {
        for i in 0..n {
            a.on_event(json!({ "type": "tool_start", "name": "bash", "args": { "command": format!("step {i}") } }));
            a.on_event(json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "toolCall", "name": "bash", "arguments": { "command": format!("step {i}") } }] } }));
            a.on_event(json!({ "type": "tool_end" }));
            a.on_event(json!({ "type": "message", "message": { "role": "toolResult", "content": [{ "type": "text", "text": format!("out {i}a\nout {i}b") }] } }));
        }
    }

    #[tokio::test]
    async fn a_run_of_tool_calls_folds_into_one_line_and_ctrl_o_unfolds_it() {
        let mut a = app(80, 30);
        a.busy = true;
        tool_turn(&mut a, 4);
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("step 3") && rows.contains("4 steps"), "{rows}");
        assert!(!rows.contains("step 1") && !rows.contains("out 3a"), "earlier steps and results are hidden: {rows}");
        assert!(rows.contains("4 tools"), "the status line counts tools: {rows}");
        a.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)).await.unwrap();
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("step 1") && rows.contains("out 3a"), "{rows}");
    }

    #[test]
    fn edits_count_distinct_files_in_the_status_line() {
        let mut a = app(100, 30);
        a.busy = true;
        for p in ["a.rs", "b.rs", "a.rs"] {
            a.on_event(json!({ "type": "tool_start", "name": "edit", "args": { "path": p } }));
        }
        let rows = a.screen.rows().join("\n");
        assert!(rows.contains("3 tools") && rows.contains("2 files edited"), "{rows}");
    }

    #[test]
    fn a_delta_rewrites_only_the_rows_that_changed() {
        let mut a = app(80, 30);
        a.busy = true;
        a.commit(vec![line("earlier conversation", Sty::Plain)]);
        a.on_event(json!({ "type": "delta", "delta": "Hello" }));
        a.capture = Some(String::new());
        a.on_event(json!({ "type": "delta", "delta": " world" }));
        let out = a.capture.take().unwrap();
        assert!(out.contains("Hello world"), "{out:?}");
        assert!(!out.contains("earlier conversation"), "unchanged rows are not rewritten: {out:?}");
        assert!(!out.contains("\x1b[2J") && !out.contains("\x1b[J"), "no clearing, so no flicker: {out:?}");
    }

    #[tokio::test]
    async fn escape_sequences_in_replies_tool_output_and_files_are_dropped() {
        let evil = "\x1b]0;pwned\x07\x1b[5Agot you\x1b]52;c;aGk=\x07";
        let mut a = app(100, 30);
        a.busy = true;
        a.on_event(json!({ "type": "delta", "delta": evil }));
        a.on_event(json!({ "type": "message", "message": { "role": "toolResult", "content": [{ "type": "text", "text": evil }] } }));
        a.on_event(json!({ "type": "status", "text": evil }));
        let dir = TempDir::new("escapes");
        let path = dir.file("evil.txt", evil);
        a.command(&format!("/open {}", path.display())).await.unwrap();
        a.draw();
        let out = a.capture.take().unwrap();
        assert!(out.contains("got you"), "{out:?}");
        for bad in ["\x1b]", "\x07", "\x1b[5A"] {
            assert!(!out.contains(bad), "{bad:?} reached the terminal: {out:?}");
        }
    }

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

    #[test]
    fn restoring_the_terminal_undoes_every_setup_step() {
        for (inline, enhanced) in [(false, false), (false, true), (true, false), (true, true)] {
            let (on, off) = (setup_sequence(inline, enhanced), restore_sequence(inline, enhanced));
            for (set, unset) in [(PASTE_ON, PASTE_OFF), (KEYS_PUSH, KEYS_POP), (ALT_SCREEN_ON, ALT_SCREEN_OFF), (MOUSE_ON, MOUSE_OFF)] {
                assert_eq!(on.contains(set), off.contains(unset), "inline={inline} enhanced={enhanced}: {set:?} vs {unset:?}");
            }
            assert!(off.ends_with("\x1b[?25h"), "the caret is shown again");
        }
        assert!(setup_sequence(false, true).contains(KEYS_PUSH) && !setup_sequence(true, false).contains(ALT_SCREEN_ON));
    }

    #[tokio::test]
    async fn restart_quits_to_start_the_installed_zen_on_this_session() {
        let mut a = app(80, 24);
        a.command("/restart").await.unwrap();
        assert!(a.quit && a.restart);
        assert_eq!(restart_args(Some("abc"), false), ["--resume", "abc"]);
        assert_eq!(restart_args(None, true), ["--inline"]);
        assert!(restart_args(None, false).is_empty());
    }

    #[tokio::test]
    async fn a_newer_install_is_noticed_and_an_upgrade_that_brings_one_restarts() {
        let dir = TempDir::new("install");
        let bin = dir.file("zen", "old");
        let mut a = app(80, 24);
        let started = std::fs::metadata(&bin).unwrap().modified().unwrap();
        a.installed = Some((bin.clone(), started));
        assert!(!a.newer_installed());
        // An upgrade that didn't change zen itself doesn't restart it.
        a.on_app_event(json!({ "type": "upgrade_done", "ok": true, "text": "zenbot upgraded" })).await;
        assert!(!a.restart && !a.quit);
        let f = std::fs::File::options().write(true).open(&bin).unwrap();
        f.set_modified(started + Duration::from_secs(60)).unwrap();
        assert!(a.newer_installed());
        // A failed upgrade doesn't restart; a successful one that installed a new zen does.
        a.on_app_event(json!({ "type": "upgrade_done", "ok": false, "text": "rolled back" })).await;
        assert!(!a.restart);
        a.on_app_event(json!({ "type": "upgrade_done", "ok": true, "text": "zenbot upgraded" })).await;
        assert!(a.restart && a.quit);
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

    #[tokio::test]
    async fn page_up_scrolls_and_new_lines_keep_the_view_still() {
        let mut a = app(80, 20);
        for i in 0..100 {
            a.commit(vec![line(format!("row {i}"), Sty::Plain)]);
        }
        let bottom = a.screen.rows().join("\n");
        assert!(bottom.contains("row 99"));
        key(&mut a, KeyCode::PageUp).await;
        a.draw();
        let up = a.screen.rows().join("\n");
        assert!(!up.contains("row 99") && up.contains("more lines · PgDn"), "{up}");
        a.commit(vec![line("row 100", Sty::Plain)]);
        assert_eq!(a.screen.rows().join("\n").replace("more lines", ""), up.replace("more lines", "").replace(&format!("↓ {} ", a.scroll - 1), &format!("↓ {} ", a.scroll)), "new output doesn't move a scrolled-up view");
        a.command("/new").await.unwrap(); // any send returns to the bottom; here PgDn does
        for _ in 0..20 {
            key(&mut a, KeyCode::PageDown).await;
        }
        a.draw();
        assert_eq!(a.scroll, 0);
        assert!(a.screen.rows().join("\n").contains("new session"));
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

    /// CPU time this thread has used, in seconds (Linux: /proc/thread-self/schedstat, in ns).
    fn thread_cpu() -> f64 {
        let s = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
        s.split_whitespace().next().and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0) / 1e9
    }

    /// A long markdown reply: paragraphs, a list and a code block, repeated to `bytes` bytes.
    fn long_reply(bytes: usize) -> String {
        let block = "## A heading\n\nSome **bold** text and `code` in a paragraph that is long enough to wrap \
                     at the chat width, as model replies usually are.\n\n- a list item\n- another item with `code`\n\n\
                     ```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n";
        let mut s = String::new();
        while s.len() < bytes {
            s.push_str(block);
        }
        s.truncate(bytes);
        s
    }

    /// Streaming cost in full screen: a 43 KB reply in 20-byte deltas, each drawn as it arrives.
    /// Run with `cargo test --release -p zen -- --ignored --nocapture streaming_cost`.
    #[test]
    #[ignore]
    fn streaming_cost() {
        let reply = long_reply(43_000);
        let mut a = app(120, 40);
        a.busy = true;
        let t0 = thread_cpu();
        let chars: Vec<char> = reply.chars().collect();
        for chunk in chars.chunks(20) {
            a.on_event(json!({ "type": "delta", "delta": chunk.iter().collect::<String>() }));
            a.capture = Some(String::new());
        }
        let secs = thread_cpu() - t0;
        println!("streaming {} bytes in 20-byte deltas: {secs:.3} s CPU", reply.len());
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
