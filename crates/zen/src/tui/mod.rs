//! Interactive terminal app. A live region at the bottom holds status, the input box and the
//! footer.
//!
//! Full screen (default): zen owns the terminal's alternate screen. The conversation is kept as
//! entries and re-rendered at the current width, in a viewport above the live region that
//! scrolls with PgUp/PgDn or the mouse wheel. `/open <file>` shows a file in a side panel next to
//! the chat, reloaded when it changes. Each frame is drawn whole and only changed rows are
//! written (`screen.rs`), so nothing flickers.
//! The sessions board (`board.rs`) is the home screen: every session, running or idle, with its
//! subagents; enter dives into one, `/board` (or esc on an empty input) comes back.
//! Inline (`zen --inline` or ZEN_INLINE=1): the conversation is printed into normal terminal
//! scrollback and the live region (with the streaming text) follows it, like Claude Code, Codex
//! and Pi.

mod board;
mod events;
mod files;
mod inline;
mod input;
mod panel;
mod pickers;
mod render;
mod state;
mod transcript;
#[cfg(test)]
pub(crate) mod test_util;

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

use crate::client::{assistant_text, record_total, short, tool_summary, usage_total, zen_home, Client, NewSession, Ws};
use crate::editor::Editor;
use crate::md::{self, line, Line, Md, Sty};
use crate::screen::{self, Screen};

use self::files::Files;
use self::{board::*, inline::*, panel::*, pickers::*, render::*, state::*, transcript::*};

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

pub enum Start {
    /// The sessions board (full screen; inline starts a new session instead).
    Board,
    New,
    Continue,
    Resume(Option<String>),
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

fn terminal_size() -> (usize, usize) {
    terminal::size().map(|(w, h)| (w as usize, h as usize)).unwrap_or((80, 24))
}

pub async fn run(c: Client, start: Start, new: NewSession, inline: bool) -> Result<()> {
    let models = c.get("/api/models").await?;
    let default_model = models["default"].as_str().unwrap_or("").to_string();
    let catalog: Vec<Value> = models["models"].as_array().cloned().unwrap_or_default();
    let signed_in = crate::engine_signed_in(&models);

    let history = Some(zen_home().join("history"));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let model = new.model.unwrap_or_else(|| default_model.clone());
    let mut app = App::new(c, tx, model, default_model, catalog, history, terminal_size(), inline);
    app.effort = new.effort;
    app.installed = installed_zen().and_then(|p| Some((p.clone(), std::fs::metadata(&p).ok()?.modified().ok()?)));
    if zen_home().is_dir() {
        app.files_root = zen_home();
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
            banner(Some(&zen_home().join("version"))),
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
                    let _ = tx.send((String::new(), 0, json!({ "type": "update_available", "text": text })));
                }
            }
        });
        match start {
            Start::Board if !inline => app.open_board().await,
            Start::Board | Start::New => {}
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
        let mut board_poll = tokio::time::interval(BOARD_REFRESH);
        while !app.quit {
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(ev)) => app.handle_terminal(ev).await,
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some(msg) = rx.recv() => app.on_incoming(msg).await,
                _ = tick.tick(), if app.busy || app.board_running() => {
                    if app.board.is_some() {
                        app.rebuild_board();
                    }
                    app.spin = app.spin.wrapping_add(1);
                    app.draw();
                }
                // The board follows what runs in every session.
                _ = board_poll.tick(), if app.board.is_some() => {
                    app.refresh_board().await;
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

/// The zen to restart into: the installed one (`<zen home>/bin/zen`, where upgrades put it), else
/// this binary's own path (on Linux, a replaced binary's path ends in " (deleted)").
fn installed_zen() -> Option<PathBuf> {
    let installed = zen_home().join("bin/zen");
    if installed.is_file() {
        return Some(installed);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

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
}
