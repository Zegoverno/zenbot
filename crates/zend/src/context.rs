//! Instruction files the owner keeps for agents (AGENTS.md, or CLAUDE.md).
//!
//! Two kinds reach the model:
//! - Always: one per directory from `/` down to the workspace. (`~/.zenbot/AGENTS.md` is not one of
//!   these: it describes the agent's environment and has its own place in the instructions, compile.rs.)
//! - On demand: a project below the workspace (e.g. ~/zenbot) has its own AGENTS.md. The first
//!   time a tool touches a path there, the kernel attaches that file to the tool's result and
//!   records it in the session (tape kind `context`), so it isn't attached again (later turns see
//!   it in the history).

use std::path::{Path, PathBuf};

use serde_json::Value;

const MAX_FILE: usize = 32 * 1024;
const NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Text cut at MAX_FILE: what is shown of an instruction file.
pub fn capped(mut text: String) -> String {
    if text.len() > MAX_FILE {
        let mut end = MAX_FILE;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[... truncated; read the file for the rest ...]");
    }
    text
}

/// The instruction file in `dir`, if any (AGENTS.md wins over CLAUDE.md).
fn in_dir(dir: &Path) -> Option<PathBuf> {
    NAMES.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// Files that always go into the system prompt: from `/` down to the workspace, with their whole
/// text (cut with `capped` when shown).
/// For a sandboxed agent (`agent`'s sandbox), only the files it could read itself.
pub fn always(workspace: &Path, sandbox: Option<&crate::sandbox::Sandbox>) -> Vec<(PathBuf, String)> {
    let mut dirs: Vec<&Path> = workspace.ancestors().collect();
    dirs.reverse();
    let paths: Vec<PathBuf> = dirs.into_iter().filter_map(in_dir).collect();
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for p in paths {
        if !out.iter().any(|(q, _)| q == &p) {
            if let Some(text) = read_for(&p, sandbox) {
                out.push((p, text));
            }
        }
    }
    out
}

/// An instruction file's text, as the agent may read it: the kernel reads it, so for a sandboxed
/// agent it's judged on the file actually opened (a link it planted, `AGENTS.md -> ~/.ssh/id`,
/// leads nowhere), like its `read`.
pub fn read_for(path: &Path, sandbox: Option<&crate::sandbox::Sandbox>) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).ok()?;
    if let Some(s) = sandbox {
        let real = std::fs::read_link(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&file))).ok()?;
        s.check(&real, false).ok()?;
    }
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    Some(text)
}

/// Instruction files that govern `paths` but aren't covered by `always` (they live below the
/// workspace), ordered from the outermost directory in.
pub fn governing(workspace: &Path, paths: &[PathBuf]) -> Vec<PathBuf> {
    governing_except(workspace, paths, &crate::zen_home())
}

fn governing_except(workspace: &Path, paths: &[PathBuf], zen_home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let real_zen_home = zen_home.canonicalize().unwrap_or_else(|_| zen_home.to_path_buf());
    for path in paths {
        // Lexical `..` or a symlink can point into the zen home; avoid attaching its
        // environment AGENTS.md as project instructions even through such aliases.
        let real_path = path.canonicalize().or_else(|_| {
            path.parent().unwrap_or(path).canonicalize().map(|parent| parent.join(path.file_name().unwrap_or_default()))
        });
        if path.starts_with(zen_home) || real_path.is_ok_and(|p| p.starts_with(&real_zen_home)) {
            continue;
        }
        let start = if path.is_dir() { path.as_path() } else { path.parent().unwrap_or(path) };
        let mut here = Vec::new();
        for dir in start.ancestors() {
            if workspace.starts_with(dir) {
                break; // the workspace and its parents are already in the system prompt
            }
            if let Some(f) = in_dir(dir) {
                if !f.canonicalize().is_ok_and(|real| real.starts_with(&real_zen_home)) {
                    here.push(f);
                }
            }
        }
        for f in here.into_iter().rev() {
            if !found.contains(&f) {
                found.push(f);
            }
        }
    }
    found
}

/// Paths a tool call touches: its path arguments, and for bash, absolute or `~` paths that exist
/// (e.g. `cd ~/zenbot && cargo build`).
pub fn paths_in_call(workspace: &Path, name: &str, args: &Value) -> Vec<PathBuf> {
    let arg = |k: &str| args.get(k).and_then(Value::as_str).map(|p| crate::tools::resolve(workspace, p));
    match name {
        "read" | "write" | "edit" => arg("path").into_iter().collect(),
        "bash" => args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .split(|c: char| c.is_whitespace() || ";&|()<>\"'=".contains(c))
            .filter(|t| t.starts_with('/') || t.starts_with("~/"))
            .take(20)
            .map(|t| crate::tools::resolve(workspace, t))
            .filter(|p| p.exists())
            .collect(),
        _ => Vec::new(),
    }
}

/// How an instruction file is shown to the model when it's attached to a tool result.
pub fn attachment(path: &Path, text: &str) -> String {
    let dir = path.parent().unwrap_or(path);
    format!(
        "\n\n<project_context path=\"{}\">\nInstructions for work under {}. Follow them.\n{}\n</project_context>",
        path.display(),
        dir.display(),
        text.trim_end()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A sandboxed agent's instruction files are read only where its sandbox lets it read: a link
    /// it planted to the owner's files never reaches its prompt.
    #[test]
    fn a_sandboxed_agents_instruction_files_stay_in_its_sandbox() {
        use crate::sandbox::{Access, Mount, Sandbox};
        let t = crate::test_util::TestDir::new("context-sandbox");
        let (work, secret) = (t.join("work"), t.join("secret"));
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(&secret, "the owner's key").unwrap();
        std::os::unix::fs::symlink(&secret, work.join("AGENTS.md")).unwrap();
        std::fs::write(work.join("sub/AGENTS.md"), "its own rules").unwrap();
        let s = Sandbox::new("garry", vec![Mount::new(&work, Access::Write)]);
        let texts = |s: Option<&Sandbox>| always(&work.join("sub"), s).into_iter().map(|(_, t)| t).collect::<Vec<_>>();
        assert_eq!(texts(Some(&s)), ["its own rules"]);
        assert!(texts(None).contains(&"the owner's key".to_string()), "zenbot reads as the owner");
        assert_eq!(read_for(&work.join("AGENTS.md"), Some(&s)), None);
    }

    fn tree() -> crate::test_util::TestDir {
        let root = crate::test_util::TestDir::new("context");
        std::fs::create_dir_all(root.join("home/proj/src/deep")).unwrap();
        std::fs::write(root.join("home/AGENTS.md"), "home rules").unwrap();
        std::fs::write(root.join("home/proj/AGENTS.md"), "project rules").unwrap();
        std::fs::write(root.join("home/proj/src/CLAUDE.md"), "src rules").unwrap();
        std::fs::write(root.join("home/proj/src/deep/a.rs"), "").unwrap();
        root
    }

    #[test]
    fn finds_nested_files_below_the_workspace_only() {
        let root = tree();
        let ws = root.join("home");
        let found = governing(&ws, &[ws.join("proj/src/deep/a.rs")]);
        assert_eq!(found, vec![ws.join("proj/AGENTS.md"), ws.join("proj/src/CLAUDE.md")]);
        // The workspace's own file is in the system prompt already; outside paths add nothing.
        assert!(governing(&ws, &[ws.join("x.txt")]).is_empty());
        assert!(governing(&ws, &[PathBuf::from("/etc/hostname")]).is_empty());
        let zen_home = ws.join(".zenbot");
        std::fs::create_dir_all(&zen_home).unwrap();
        std::fs::write(zen_home.join("AGENTS.md"), "environment rules").unwrap();
        assert!(governing_except(&ws, &[zen_home.join("token")], &zen_home).is_empty());
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        let alias = ws.join("sub/../.zenbot/AGENTS.md");
        assert!(governing_except(&ws, &[alias], &zen_home).is_empty());
        std::os::unix::fs::symlink(&zen_home, ws.join("link")).unwrap();
        assert!(governing_except(&ws, &[ws.join("link/missing/deep/file")], &zen_home).is_empty());
    }


    #[test]
    fn extracts_paths_from_tool_calls() {
        let root = tree();
        let ws = root.join("home");
        let p = paths_in_call(&ws, "read", &json!({ "path": "proj/src/deep/a.rs" }));
        assert_eq!(p, vec![ws.join("proj/src/deep/a.rs")]);
        let cmd = format!("cd {} && cargo build; ls /definitely/not/here", ws.join("proj").display());
        assert_eq!(paths_in_call(&ws, "bash", &json!({ "command": cmd })), vec![ws.join("proj")]);
    }

}
