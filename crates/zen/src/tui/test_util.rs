//! Helpers shared by the tests: a temporary folder, and an `App` on a fake terminal.

use std::path::{Path, PathBuf};

use super::*;

/// A fresh temporary folder, removed when dropped, so a failing test doesn't leave it behind.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> TempDir {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("zen-test-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    /// Write `text` to `rel` inside the folder (creating its parents) and return its path.
    pub fn file(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An app on a fake terminal of `cols` x `rows`, capturing output, with no kernel behind it.
pub(super) fn app(cols: usize, rows: usize) -> App {
    let c = Client::new("http://127.0.0.1:9".into(), Some("test".into())).unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut a = App::new(c, tx, "claude/claude-opus-5-5".into(), "claude/claude-opus-5-5".into(), vec![], None, (cols, rows), false);
    a.capture = Some(String::new());
    a
}

/// An app with a model list: one model with thinking levels (default high), one without.
pub(super) fn app_with_models(cols: usize, rows: usize) -> App {
    let mut a = app(cols, rows);
    a.models = vec![
        json!({ "id": "claude/claude-opus-5-5", "efforts": ["low", "medium", "high", "xhigh", "max"], "default_effort": "high" }),
        json!({ "id": "faux/smoke" }),
    ];
    a
}

pub(super) fn footer(a: &App) -> String {
    texts(&a.compose().0).last().cloned().unwrap_or_default()
}

pub(super) fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(|l| l.iter().map(|(t, _)| t.as_str()).collect()).collect()
}

pub(super) fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub(super) async fn key(a: &mut App, code: KeyCode) {
    a.on_key(KeyEvent::new(code, KeyModifiers::NONE)).await.unwrap();
}

pub(super) async fn typed(a: &mut App, text: &str) {
    for ch in text.chars() {
        key(a, KeyCode::Char(ch)).await;
    }
}

pub(super) fn tool_turn(a: &mut App, n: usize) {
    for i in 0..n {
        a.on_event(json!({ "type": "tool_start", "name": "bash", "args": { "command": format!("step {i}") } }));
        a.on_event(json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "toolCall", "name": "bash", "arguments": { "command": format!("step {i}") } }] } }));
        a.on_event(json!({ "type": "tool_end" }));
        a.on_event(json!({ "type": "message", "message": { "role": "toolResult", "content": [{ "type": "text", "text": format!("out {i}a\nout {i}b") }] } }));
    }
}
