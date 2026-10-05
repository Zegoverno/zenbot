//! Built-in tools. The kernel is the only place side effects happen.
//! v0: commands run directly on the host inside the workspace; sandboxes come in M2.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const MAX_OUTPUT: usize = 50 * 1024;
const DEFAULT_READ_LINES: usize = 2000;
/// Most bash output kept in memory; the rest is counted but dropped.
const MAX_CAPTURE: usize = 16 * 1024 * 1024;

/// The kernel's tools, in a fixed order (they are part of the cached prefix). `history` is executed
/// by the kernel itself (compact.rs), since it reads the session's tape.
pub fn specs() -> Value {
    let mut all = builtin_specs();
    if let Some(list) = all.as_array_mut() {
        list.push(crate::compact::tool_spec());
    }
    all
}

fn builtin_specs() -> Value {
    json!([
        {
            "name": "bash",
            "description": "Run a shell command with bash in the workspace directory. Returns combined stdout/stderr and the exit code. \
Use for listing files, searching (rg, grep, find), git, builds, tests and running programs. \
Output over 50KB is cut to its head and tail and the full output is saved to a file you can read. \
For servers and other long-running processes, start them in the background with output redirected, e.g. `cmd > /tmp/cmd.log 2>&1 &`.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to run" },
                    "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120, max 600)" }
                },
                "required": ["command"]
            }
        },
        {
            "name": "read",
            "description": "Read a text file. Returns numbered lines (up to 2000 lines or 50KB per call; the footer says which offset to continue from). Paths are relative to the workspace unless absolute.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "offset": { "type": "integer", "description": "1-based line to start from" },
                    "limit": { "type": "integer", "description": "Maximum number of lines (default 2000)" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "write",
            "description": "Create or overwrite a file with the given content. Creates parent directories. Use edit for small changes to existing files.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "content": { "type": "string", "description": "Full file content" }
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "edit",
            "description": "Replace text in a file. old_text must match the file (copy it from read, without the line numbers) and be unique unless replace_all is true. \
Read the file first. Line endings and trailing whitespace are handled for you.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "old_text": { "type": "string", "description": "Exact text to replace" },
                    "new_text": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence" }
                },
                "required": ["path", "old_text", "new_text"]
            }
        },
        {
            "name": "move",
            "description": "Move or rename a file or directory. Creates parent directories of the destination.",
            "parameters": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Source path" },
                    "to": { "type": "string", "description": "Destination path" }
                },
                "required": ["from", "to"]
            }
        }
    ])
}

pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

fn ok(content: impl Into<String>) -> ToolOutput {
    ToolOutput { content: content.into(), is_error: false }
}

fn err(content: impl Into<String>) -> ToolOutput {
    ToolOutput { content: content.into(), is_error: true }
}

/// Resolve a tool path: `~` is the home directory, a leading `@` (as in `@file` mentions) is dropped,
/// and relative paths are relative to the workspace.
pub fn resolve(workspace: &Path, p: &str) -> PathBuf {
    let p = p.strip_prefix('@').unwrap_or(p);
    let home = || std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"));
    if p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolOutput> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| err(format!("missing string argument `{key}`")))
}

// ---------- per-file locks ----------

/// Tool calls run concurrently; edits to the same file must not interleave (read-modify-write).
static FILE_LOCKS: LazyLock<std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(Default::default);

fn lock_key(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    match (path.parent().and_then(|d| d.canonicalize().ok()), path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

async fn lock_files(paths: &[&Path]) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
    let mut keys: Vec<PathBuf> = paths.iter().map(|p| lock_key(p)).collect();
    keys.sort();
    keys.dedup();
    let locks: Vec<_> = {
        let mut map = FILE_LOCKS.lock().unwrap();
        map.retain(|_, l| Arc::strong_count(l) > 1); // forget idle locks
        keys.into_iter().map(|k| map.entry(k).or_default().clone()).collect()
    };
    let mut guards = Vec::new();
    for l in locks {
        guards.push(l.lock_owned().await);
    }
    guards
}

// ---------- output limits ----------

/// Keep the head and tail of oversized output (cut at line boundaries) so errors at the end stay visible.
fn truncate(s: &str) -> Option<String> {
    if s.len() <= MAX_OUTPUT {
        return None;
    }
    let half = MAX_OUTPUT / 2;
    let mut head_end = half;
    while !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    if let Some(nl) = s[..head_end].rfind('\n') {
        head_end = nl + 1;
    }
    let mut tail_start = s.len() - half;
    while !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if let Some(nl) = s[tail_start..].find('\n') {
        if tail_start + nl + 1 < s.len() {
            tail_start += nl + 1;
        }
    }
    let skipped_lines = s[head_end..tail_start].matches('\n').count();
    Some(format!(
        "{}\n[... {} lines ({} bytes) omitted ...]\n\n{}",
        &s[..head_end],
        skipped_lines,
        tail_start - head_end,
        &s[tail_start..]
    ))
}

/// Save the full text of an oversized output so the model can page through it with `read`. Kept in
/// ~/.zenbot/outputs, readable only by the owner (not /tmp, which every user can read and a reboot
/// clears while the history still points at it). Secrets are masked first.
fn save_full_output(text: &str) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home).join(".zenbot/outputs");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let path = dir.join(format!("{nanos}.log"));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).ok()?;
    std::io::Write::write_all(&mut f, crate::secrets::mask(text).as_bytes()).ok()?;
    Some(path)
}

/// Run a tool. `env` is added to the environment of commands (bash). With `read_only`, bash runs in
/// a sandbox where the filesystem is mounted read-only (bubblewrap) and only `read` may run besides.
pub async fn execute(workspace: &Path, name: &str, args: &Value, env: &[(&str, &str)], read_only: bool) -> ToolOutput {
    if read_only && !matches!(name, "bash" | "read") {
        return err(format!("`{name}` can't run in a read-only phase"));
    }
    let result = match name {
        "bash" => bash(workspace, args, env, read_only).await,
        "read" => read(workspace, args).await,
        "write" => write(workspace, args).await,
        "edit" => edit(workspace, args).await,
        "move" => move_path(workspace, args).await,
        _ => Err(err(format!("unknown tool `{name}`"))),
    };
    let mut out = result.unwrap_or_else(|e| e);
    out.content = crate::secrets::mask(&out.content);
    out
}

// ---------- bash ----------

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
const SIGKILL: i32 = 9;

/// Kills a command's whole process group (bash and everything it started) unless disarmed.
/// Dropping the tool future (on abort) therefore cleans up grandchildren too.
struct GroupKill(Option<i32>);

impl GroupKill {
    fn kill_now(&mut self) {
        if let Some(pgid) = self.0.take() {
            // SAFETY: plain syscall; a negative pid addresses the process group we created.
            unsafe { kill(-pgid, SIGKILL) };
        }
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// bubblewrap's arguments for a read-only view of the machine: everything mounted read-only, a
/// private /tmp, the network left as it is (looking things up is allowed).
const READ_ONLY_SANDBOX: [&str; 10] = ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--die-with-parent"];

async fn bash(workspace: &Path, args: &Value, env: &[(&str, &str)], read_only: bool) -> Result<ToolOutput, ToolOutput> {
    let command = str_arg(args, "command")?;
    let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(120).clamp(1, 600);
    let mut cmd = if read_only {
        let mut c = Command::new("bwrap");
        // The workspace is bound again after the private /tmp, in case it lives under /tmp.
        c.args(READ_ONLY_SANDBOX).arg("--ro-bind").arg(workspace).arg(workspace).arg("--chdir").arg(workspace).args(["bash", "-lc"]);
        c
    } else {
        let mut c = Command::new("bash");
        c.arg("-lc");
        c
    };
    let mut child = cmd
        .arg(command)
        .current_dir(workspace)
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| err(if read_only { format!("the read-only shell (bubblewrap) couldn't start: {e}; use read instead") } else { format!("failed to start bash: {e}") }))?;
    let mut group = GroupKill(child.id().map(|p| p as i32));

    // Stream both pipes into one buffer as data arrives, so a timeout still returns what was printed.
    let buf: Arc<std::sync::Mutex<(Vec<u8>, usize)>> = Arc::default();
    let mut readers = Vec::new();
    let pipes: [Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>; 2] =
        [child.stdout.take().map(|p| Box::new(p) as _), child.stderr.take().map(|p| Box::new(p) as _)];
    for mut pipe in pipes.into_iter().flatten() {
        let buf = buf.clone();
        readers.push(tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                let mut b = buf.lock().unwrap();
                let room = MAX_CAPTURE.saturating_sub(b.0.len()).min(n);
                b.0.extend_from_slice(&chunk[..room]);
                b.1 += n - room;
            }
        }));
    }

    let status = tokio::select! {
        s = child.wait() => Some(s),
        _ = tokio::time::sleep(Duration::from_secs(timeout)) => None,
    };
    if status.is_none() {
        group.kill_now();
        let _ = child.wait().await;
    }
    // bash has exited. A background process it started may still hold the pipes open:
    // collect what's already there, then stop reading instead of waiting for it.
    let drain = async {
        for r in readers.iter_mut() {
            let _ = r.await;
        }
    };
    let _ = tokio::time::timeout(Duration::from_millis(250), drain).await;
    for r in &readers {
        r.abort();
    }

    let (bytes, dropped) = std::mem::take(&mut *buf.lock().unwrap());
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if dropped > 0 {
        text.push_str(&format!("\n[... {dropped} more bytes of output were discarded ...]"));
    }
    let mut body = match truncate(&text) {
        Some(cut) => match save_full_output(&text) {
            Some(path) => format!("{cut}\n[output truncated; full output ({} bytes) saved to {}]", text.len(), path.display()),
            None => cut,
        },
        None => text,
    };
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    match status {
        None => {
            body.push_str(&format!("[command timed out after {timeout}s and was killed; output so far is shown above]"));
            Err(err(body))
        }
        Some(Err(e)) => Err(err(format!("{body}[failed waiting for bash: {e}]"))),
        Some(Ok(status)) => {
            group.0 = None; // finished normally: leave background processes it started running
            let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
            body.push_str(&format!("[exit code: {code}]"));
            Ok(if status.success() { ok(body) } else { err(body) })
        }
    }
}

// ---------- read ----------

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0) || std::str::from_utf8(bytes).is_err()
}

async fn read(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let bytes = tokio::fs::read(&path).await.map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    if is_binary(&bytes) {
        return Err(err(format!(
            "{} is a binary file ({} bytes), not text; inspect it with bash (e.g. `file`, `xxd | head`)",
            path.display(),
            bytes.len()
        )));
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).map(|l| (l as usize).max(1)).unwrap_or(DEFAULT_READ_LINES);
    let total = text.lines().count();
    if total == 0 {
        return Ok(ok("[empty file]\n"));
    }
    if offset > total {
        return Err(err(format!("offset {offset} is past the end of the file ({total} lines)")));
    }
    let mut out = String::new();
    let mut last = offset - 1;
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let numbered = format!("{:>6}\t{}\n", i + 1, line);
        if out.len() + numbered.len() > MAX_OUTPUT {
            if out.is_empty() {
                // A single line longer than the whole budget: show its start.
                let mut end = MAX_OUTPUT;
                while !numbered.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&numbered[..end]);
                out.push_str(&format!(
                    "\n[line {} is {} bytes long and was cut; use bash (e.g. `sed -n '{}p' FILE | cut -c1-2000`) to see parts of it]\n",
                    i + 1,
                    line.len(),
                    i + 1
                ));
                last = i + 1;
            }
            break;
        }
        out.push_str(&numbered);
        last = i + 1;
    }
    if last < total {
        out.push_str(&format!("[showing lines {offset}-{last} of {total}; continue with offset={}]\n", last + 1));
    }
    Ok(ok(out))
}

// ---------- write / edit / move ----------

async fn write(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let content = str_arg(args, "content")?;
    let _guard = lock_files(&[&path]).await;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| err(format!("cannot create {}: {e}", parent.display())))?;
    }
    tokio::fs::write(&path, content).await.map_err(|e| err(format!("cannot write {}: {e}", path.display())))?;
    Ok(ok(format!("wrote {} bytes to {}", content.len(), path.display())))
}

/// Text normalized for loose matching, with the byte span in the original text that each
/// normalized byte came from. Trailing whitespace on each line is dropped, and typographic
/// quotes, dashes and special spaces become their ASCII forms (models often "fix" these).
struct Normalized {
    text: String,
    spans: Vec<(usize, usize)>,
}

fn normalize(s: &str) -> Normalized {
    let mut n = Normalized { text: String::with_capacity(s.len()), spans: Vec::with_capacity(s.len()) };
    let mut line: Vec<(char, usize, usize)> = Vec::new();
    let flush = |line: &mut Vec<(char, usize, usize)>, n: &mut Normalized| {
        while line.last().is_some_and(|(c, _, _)| *c == ' ' || *c == '\t') {
            line.pop();
        }
        for (c, a, b) in line.drain(..) {
            n.text.push(c);
            for _ in 0..c.len_utf8() {
                n.spans.push((a, b));
            }
        }
    };
    for (i, c) in s.char_indices() {
        let end = i + c.len_utf8();
        if c == '\n' {
            flush(&mut line, &mut n);
            n.text.push('\n');
            n.spans.push((i, end));
            continue;
        }
        let c = match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            c => c,
        };
        line.push((c, i, end));
    }
    flush(&mut line, &mut n);
    n
}

/// Non-overlapping byte ranges of `needle` in `hay`: exact matches if any, otherwise loose matches
/// mapped back to the original text. The bool says whether the loose fallback was used.
fn find_matches(hay: &str, needle: &str) -> (Vec<(usize, usize)>, bool) {
    let exact: Vec<(usize, usize)> = hay.match_indices(needle).map(|(i, m)| (i, i + m.len())).collect();
    if !exact.is_empty() {
        return (exact, false);
    }
    let h = normalize(hay);
    let n = normalize(needle);
    let needle = n.text.trim_end_matches('\n');
    if needle.trim().is_empty() {
        return (Vec::new(), false);
    }
    let found = h
        .text
        .match_indices(needle)
        .map(|(i, m)| (h.spans[i].0, h.spans[i + m.len() - 1].1))
        .collect();
    (found, true)
}

async fn edit(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let old = str_arg(args, "old_text")?.replace("\r\n", "\n");
    let new = str_arg(args, "new_text")?.replace("\r\n", "\n");
    let all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    if old.is_empty() {
        return Err(err("old_text must not be empty"));
    }
    if old == new {
        return Err(err("old_text and new_text are identical; nothing to change"));
    }
    let _guard = lock_files(&[&path]).await;
    let bytes = tokio::fs::read(&path).await.map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    let original = String::from_utf8(bytes).map_err(|_| err(format!("{} is not a UTF-8 text file", path.display())))?;

    // Match on LF text without a BOM; restore both when writing back.
    let (bom, body) = match original.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", original.as_str()),
    };
    let crlf = body.find('\n').is_some_and(|i| i > 0 && body.as_bytes()[i - 1] == b'\r');
    let text = body.replace("\r\n", "\n");

    let (matches, loose) = find_matches(&text, &old);
    if matches.is_empty() {
        return Err(err(format!(
            "old_text not found in {}; read the file again and copy the exact text (without line numbers)",
            path.display()
        )));
    }
    if matches.len() > 1 && !all {
        return Err(err(format!("old_text matches {} times; include more surrounding lines to make it unique, or set replace_all", matches.len())));
    }
    let mut updated = String::with_capacity(text.len() + new.len());
    let mut pos = 0;
    for &(a, b) in if all { &matches[..] } else { &matches[..1] } {
        updated.push_str(&text[pos..a]);
        updated.push_str(&new);
        pos = b;
    }
    updated.push_str(&text[pos..]);
    if updated == text {
        return Err(err("the replacement produced identical content; nothing changed"));
    }
    let updated = if crlf { updated.replace('\n', "\r\n") } else { updated };
    tokio::fs::write(&path, format!("{bom}{updated}"))
        .await
        .map_err(|e| err(format!("cannot write {}: {e}", path.display())))?;
    let n = if all { matches.len() } else { 1 };
    let note = if loose { " (matched after normalizing whitespace and quotes)" } else { "" };
    Ok(ok(format!("replaced {n} occurrence(s) in {}{note}", path.display())))
}

async fn move_path(workspace: &Path, args: &Value) -> Result<ToolOutput, ToolOutput> {
    let from = resolve(workspace, str_arg(args, "from")?);
    let to = resolve(workspace, str_arg(args, "to")?);
    let _guard = lock_files(&[&from, &to]).await;
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| err(format!("cannot create {}: {e}", parent.display())))?;
    }
    tokio::fs::rename(&from, &to)
        .await
        .map_err(|e| err(format!("cannot move {} to {}: {e}", from.display(), to.display())))?;
    Ok(ok(format!("moved {} to {}", from.display(), to.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("zend-test-{name}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn run(ws: &Path, name: &str, args: Value) -> ToolOutput {
        execute(ws, name, &args, &[], false).await
    }

    #[tokio::test]
    async fn edit_exact_unique_and_replace_all() {
        let ws = scratch("edit");
        std::fs::write(ws.join("a.txt"), "one\ntwo\ntwo\n").unwrap();
        let r = run(&ws, "edit", json!({ "path": "a.txt", "old_text": "one", "new_text": "1" })).await;
        assert!(!r.is_error, "{}", r.content);
        let r = run(&ws, "edit", json!({ "path": "a.txt", "old_text": "two", "new_text": "2" })).await;
        assert!(r.is_error && r.content.contains("2 times"));
        let r = run(&ws, "edit", json!({ "path": "a.txt", "old_text": "two", "new_text": "2", "replace_all": true })).await;
        assert!(!r.is_error);
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "1\n2\n2\n");
        let r = run(&ws, "edit", json!({ "path": "a.txt", "old_text": "2", "new_text": "2" })).await;
        assert!(r.is_error && r.content.contains("identical"));
    }

    #[tokio::test]
    async fn edit_keeps_crlf_and_bom() {
        let ws = scratch("crlf");
        std::fs::write(ws.join("w.txt"), "\u{feff}fn a() {\r\n    x\r\n}\r\n").unwrap();
        let r = run(&ws, "edit", json!({ "path": "w.txt", "old_text": "fn a() {\n    x\n}", "new_text": "fn a() {\n    y\n}" })).await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(ws.join("w.txt")).unwrap(), "\u{feff}fn a() {\r\n    y\r\n}\r\n");
    }

    #[tokio::test]
    async fn edit_loose_match_trailing_space_and_quotes() {
        let ws = scratch("loose");
        std::fs::write(ws.join("q.md"), "say \u{201C}hi\u{201D} — now   \nnext line\nkeep \u{2019}this\u{2019}\n").unwrap();
        let r = run(&ws, "edit", json!({ "path": "q.md", "old_text": "say \"hi\" - now\nnext line", "new_text": "replaced" })).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("normalizing"));
        // Untouched lines keep their original bytes.
        assert_eq!(std::fs::read_to_string(ws.join("q.md")).unwrap(), "replaced\nkeep \u{2019}this\u{2019}\n");
    }

    #[tokio::test]
    async fn concurrent_edits_to_one_file_all_apply() {
        let ws = scratch("lock");
        let body: String = (0..20).map(|i| format!("line{i}\n")).collect();
        std::fs::write(ws.join("c.txt"), &body).unwrap();
        let ws = Arc::new(ws);
        let mut tasks = Vec::new();
        for i in 0..20 {
            let ws = ws.clone();
            tasks.push(tokio::spawn(async move {
                run(&ws, "edit", json!({ "path": "c.txt", "old_text": format!("line{i}\n"), "new_text": format!("LINE{i}\n") })).await
            }));
        }
        for t in tasks {
            assert!(!t.await.unwrap().is_error);
        }
        let out = std::fs::read_to_string(ws.join("c.txt")).unwrap();
        assert!(!out.contains("line"), "{out}");
    }

    #[tokio::test]
    async fn read_pages_by_whole_lines() {
        let ws = scratch("read");
        let body: String = (1..=5000).map(|i| format!("{i:0>40}\n")).collect();
        std::fs::write(ws.join("big.txt"), &body).unwrap();
        let r = run(&ws, "read", json!({ "path": "big.txt" })).await;
        assert!(!r.is_error);
        assert!(r.content.len() <= MAX_OUTPUT + 200);
        let footer = r.content.lines().last().unwrap();
        assert!(footer.contains("continue with offset="), "{footer}");
        let next: usize = footer.rsplit('=').next().unwrap().trim_end_matches(']').parse().unwrap();
        let last_shown = r.content.lines().rev().nth(1).unwrap().trim().split('\t').next().unwrap().parse::<usize>().unwrap();
        assert_eq!(next, last_shown + 1);
        let r = run(&ws, "read", json!({ "path": "big.txt", "offset": 9999 })).await;
        assert!(r.is_error && r.content.contains("past the end"));
        std::fs::write(ws.join("bin"), [0u8, 1, 2, 3]).unwrap();
        let r = run(&ws, "read", json!({ "path": "bin" })).await;
        assert!(r.is_error && r.content.contains("binary"));
    }

    #[tokio::test]
    async fn paths_expand_home_and_strip_at() {
        let ws = Path::new("/ws");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(resolve(ws, "~/x"), PathBuf::from(&home).join("x"));
        assert_eq!(resolve(ws, "@src/a.rs"), PathBuf::from("/ws/src/a.rs"));
        assert_eq!(resolve(ws, "/abs"), PathBuf::from("/abs"));
    }

    #[tokio::test]
    async fn bash_timeout_keeps_output_and_kills_group() {
        let ws = scratch("bash");
        let started = Instant::now();
        // 3s leaves room for a slow login shell (bash -l) to start, e.g. on CI runners.
        // A marker unique to this run, so leftovers from other runs can't be mistaken for ours.
        let marker = ws.file_name().unwrap().to_string_lossy().to_string();
        let cmd = format!("echo before; (sleep 30; echo {marker} > leaked.txt) & sleep 30");
        let r = run(&ws, "bash", json!({ "command": cmd, "timeout_secs": 3 })).await;
        assert!(r.is_error);
        assert!(r.content.contains("before"), "{}", r.content);
        assert!(r.content.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let out = std::process::Command::new("pgrep").args(["-f", &format!("echo {marker} ")]).output().unwrap();
        assert!(out.stdout.is_empty(), "background child survived the timeout");
    }

    #[tokio::test]
    async fn bash_background_process_does_not_block() {
        let ws = scratch("bg");
        let started = Instant::now();
        let r = run(&ws, "bash", json!({ "command": "sleep 5 & echo started" })).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("started"));
        assert!(started.elapsed() < Duration::from_secs(3), "waited for the background process");
    }

    #[tokio::test]
    async fn bash_large_output_is_saved() {
        let ws = scratch("big");
        let r = run(&ws, "bash", json!({ "command": "seq 1 100000" })).await;
        assert!(!r.is_error);
        assert!(r.content.starts_with("1\n2\n"));
        assert!(r.content.contains("100000\n"));
        let path = r.content.split("saved to ").nth(1).unwrap().split(']').next().unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 100000);
    }

    #[tokio::test]
    async fn dropping_bash_future_kills_it() {
        let ws = scratch("drop");
        let marker = ws.join("done.txt");
        let args = json!({ "command": "sleep 2; touch done.txt" });
        let fut = execute(&ws, "bash", &args, &[], false);
        let _ = tokio::time::timeout(Duration::from_millis(300), fut).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "command kept running after its future was dropped");
    }
}
