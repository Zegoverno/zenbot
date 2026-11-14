//! Built-in tools. The kernel is the only place side effects happen.
//! Commands run with bash on the host, as the owner, in the session's workspace; the kernel's
//! secret variables are removed from their environment and their output is masked. Every agent but
//! zenbot runs them, and its file paths are checked, in its sandbox (sandbox.rs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::sandbox::Sandbox;

const MAX_OUTPUT: usize = crate::output::PREVIEW_BYTES;
const DEFAULT_READ_LINES: usize = 2000;
const READ_BYTES: usize = crate::output::READ_BYTES;
/// Most bash output kept in memory; the rest is counted but dropped.
const MAX_CAPTURE: usize = crate::output::CAPTURE_BYTES;

/// The file and shell tools, in a fixed order (they are part of the cached prefix). The agent's
/// other tools are added after them (agent.rs). Each description says what the tool does, when to
/// use it and when not, and what it returns.
pub fn specs() -> Value {
    json!([
        {
            "name": "bash",
            "description": "Run bash in the workspace for searches, git, builds, tests and programs. Use read/write/edit for files. Runs as the owner on the host. Returns combined output and exit status, with a bounded head/tail preview and saved capture when large. Background processes are not a durable service mechanism.",
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
            "description": "Read text as numbered lines (default 2000, bounded preview); continue at the returned offset. Also inspect PNG/JPEG/GIF/WebP images and selected PDF pages (pages:[1,3], default first page; requires poppler-utils). Paths are workspace-relative or absolute; ~ means home. Read before editing.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "offset": { "type": "integer", "description": "1-based line to start from" },
                    "limit": { "type": "integer", "description": "Maximum text lines (default 2000)" },
                    "pages": { "type": "array", "minItems": 1, "maxItems": 4, "items": { "type": "integer", "minimum": 1 }, "description": "PDF pages, 1-based; default [1]" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "write",
            "description": "Create or replace a complete UTF-8 file, creating parent directories. Use edit for partial changes. Returns path and byte count. Use bash to move/delete files.",
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
            "description": "Replace text in one file. Read first; old_text must match uniquely unless replace_all:true. Give old_text/new_text, or edits (up to 100) applied in order, all matched before one atomic write. Preserves line endings and permissions. Returns replacement count and a bounded diff.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "old_text": { "type": "string", "description": "Exact text to replace" },
                    "new_text": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence" },
                    "edits": { "type": "array", "minItems": 1, "maxItems": 100, "description": "Ordered replacements in this file; all validate before one write",
                        "items": { "type": "object", "properties": {
                            "old_text": { "type": "string" }, "new_text": { "type": "string" }, "replace_all": { "type": "boolean" }
                        }, "required": ["old_text", "new_text"] } }
                },
                "required": ["path"]
            }
        }
    ])
}

/// A tool's result: text, optional media blocks and the files its full output was saved to.
pub struct ToolOutput {
    pub content_blocks: Vec<Value>,
    pub saved_outputs: Vec<PathBuf>,
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    /// Text only, an error or not.
    pub fn text(content: impl Into<String>, is_error: bool) -> ToolOutput {
        ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), content: content.into(), is_error }
    }

    pub fn ok(content: impl Into<String>) -> ToolOutput {
        ToolOutput::text(content, false)
    }

    pub fn error(content: impl Into<String>) -> ToolOutput {
        ToolOutput::text(content, true)
    }

    /// A tool's answer, or its error after `failed` (e.g. "search failed").
    pub fn from_result(result: anyhow::Result<String>, failed: &str) -> ToolOutput {
        match result {
            Ok(content) => ToolOutput::ok(content),
            Err(e) => ToolOutput::error(format!("{failed}: {e:#}")),
        }
    }
}

// Short names for the many results built below.
fn ok(content: impl Into<String>) -> ToolOutput {
    ToolOutput::ok(content)
}

fn err(content: impl Into<String>) -> ToolOutput {
    ToolOutput::error(content)
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
    crate::output::preview(s)
}

/// Save the full text of an oversized output so the model can page through it with `read`. Kept in
/// `<zen home>/outputs` (crate::outputs_dir), readable only by the owner (not /tmp, which every user can read and a reboot
/// clears while the history still points at it). Secrets are masked first.
pub(crate) fn save_full_output(text: &str) -> Option<PathBuf> {
    save_full_output_in(&crate::outputs_dir()?, text)
}

fn save_full_output_in(dir: &Path, text: &str) -> Option<PathBuf> {
    crate::output::save_text(dir, text)
}

/// Run a tool. `env` is added to the environment of commands (bash). With a sandbox (every agent
/// but zenbot), bash runs inside it and the file tools refuse paths outside it.
pub async fn execute(workspace: &Path, name: &str, args: &Value, env: &[(&str, &str)], sandbox: Option<&Sandbox>) -> ToolOutput {
    let checked = |write: bool| -> Result<(), ToolOutput> {
        match (sandbox, args.get("path").and_then(Value::as_str)) {
            (Some(s), Some(p)) => s.check(&resolve(workspace, p), write).map_err(err),
            _ => Ok(()),
        }
    };
    let result = match name {
        "bash" => bash(workspace, args, env, None, sandbox).await,
        "read" => match checked(false) { Ok(()) => read(workspace, args, sandbox).await, Err(e) => Err(e) },
        "write" => match checked(true) { Ok(()) => write(workspace, args, sandbox).await, Err(e) => Err(e) },
        "edit" => match checked(true) { Ok(()) => edit(workspace, args, sandbox).await, Err(e) => Err(e) },
        _ => Err(err(format!("unknown tool `{name}`"))),
    };
    // Secrets are masked once for every tool, where the dispatcher hands the result back.
    result.unwrap_or_else(|e| e)
}

// ---------- bash ----------

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
const SIGKILL: i32 = 9;

/// Kills a command's whole process group (bash and everything it started) unless disarmed.
/// Dropping the tool future (on abort) therefore cleans up grandchildren too.
pub(crate) struct GroupKill(pub(crate) Option<i32>);

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

/// What a command printed (stdout and stderr as they arrived; past MAX_CAPTURE only counted) and how
/// it ended: `None` when it timed out and its process group was killed.
pub struct Shell {
    pub text: String,
    pub status: Option<std::io::Result<std::process::ExitStatus>>,
}

/// Run bash in its own process group, killed on timeout or cancellation, inside `sandbox` when
/// there is one (never outside it: a sandbox that can't start is an error). The kernel's secret
/// variables are removed from its environment.
pub async fn run_shell(dir: &Path, command: &str, env: &[(&str, &str)], timeout: Duration, sandbox: Option<&Sandbox>) -> Result<Shell, String> {
    let mut cmd = match sandbox {
        Some(s) => s.command(dir, command)?,
        None => {
            let mut cmd = Command::new("bash");
            cmd.arg("-c").arg(command).current_dir(dir);
            cmd
        }
    };
    // The kernel's secrets (API keys, its token, the database URL) stay out of the agent's shell.
    for (k, _) in std::env::vars().filter(|(k, _)| crate::secrets::is_secret_var(k)) {
        cmd.env_remove(k);
    }
    let mut child = cmd
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to start bash: {e}"))?;
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
        _ = tokio::time::sleep(timeout) => None,
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
    if matches!(status, Some(Ok(_))) {
        group.0 = None; // finished normally: leave background processes it started running
    }

    let (bytes, dropped) = std::mem::take(&mut *buf.lock().unwrap());
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if dropped > 0 {
        text.push_str(&format!("\n[... {dropped} more bytes of output were discarded ...]"));
    }
    Ok(Shell { text, status })
}

async fn bash(workspace: &Path, args: &Value, env: &[(&str, &str)], output_dir: Option<PathBuf>, sandbox: Option<&Sandbox>) -> Result<ToolOutput, ToolOutput> {
    let command = str_arg(args, "command")?;
    let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(120).clamp(1, 600);
    let Shell { text, status } = run_shell(workspace, command, env, Duration::from_secs(timeout), sandbox).await.map_err(err)?;
    // Cutting and saving (with masking) up to 16 MB of output is blocking work: off the runtime's threads.
    let mut saved_outputs = Vec::new();
    let mut body = if text.len() <= MAX_OUTPUT {
        text
    } else {
        let cut = tokio::task::spawn_blocking(move || {
            // Mask before cutting, so a secret split by the cut can't show half in clear.
            let text = crate::secrets::mask(&text);
            let cut = truncate(&text).unwrap_or_default();
            let saved = match output_dir {
                Some(dir) => save_full_output_in(&dir, &text),
                None => save_full_output(&text),
            };
            match saved {
                Some(path) => (format!("{cut}\n[output truncated; captured output ({} bytes) saved to {}]", text.len(), path.display()), Some(path)),
                None => (format!("{cut}\n[output truncated; omitted output could not be saved]"), None),
            }
        });
        let (body, path) = cut.await.map_err(|e| err(format!("failed to collect the output: {e}")))?;
        saved_outputs.extend(path);
        body
    };
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    match status {
        None => {
            body.push_str(&format!("[command timed out after {timeout}s and was killed; output so far is shown above]"));
            let mut out = err(body); out.saved_outputs = saved_outputs; Err(out)
        }
        Some(Err(e)) => { let mut out = err(format!("{body}[failed waiting for bash: {e}]")); out.saved_outputs = saved_outputs; Err(out) },
        Some(Ok(status)) => {
            let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
            body.push_str(&format!("[exit code: {code}]"));
            let mut out = if status.success() { ok(body) } else { err(body) };
            out.saved_outputs = saved_outputs;
            Ok(out)
        }
    }
}

// ---------- read ----------

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0) || std::str::from_utf8(bytes).is_err()
}

async fn read(workspace: &Path, args: &Value, sandbox: Option<&Sandbox>) -> Result<ToolOutput, ToolOutput> {
    const MAX_READ_FILE: u64 = crate::media::MAX_FILE_BYTES as u64;
    let path = resolve(workspace, str_arg(args, "path")?);
    let file = tokio::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path).await
        .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    // The file actually opened, in case a link changed after the path was checked.
    if let Some(s) = sandbox {
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
        let opened = std::fs::read_link(format!("/proc/self/fd/{fd}")).map_err(|e| err(format!("cannot check {}: {e}", path.display())))?;
        s.check(&opened, false).map_err(err)?;
    }
    let meta = file.metadata().await.map_err(|e| err(format!("cannot stat {}: {e}", path.display())))?;
    if meta.is_dir() {
        return Err(err(format!("{} is a directory; list it with bash (e.g. `ls -la {}`)", path.display(), path.display())));
    }
    if !meta.is_file() { return Err(err("read accepts only regular files, not pipes or devices")); }
    let size = meta.len();
    if size > MAX_READ_FILE {
        return Err(err(format!("{} is {} bytes, over read's 20 MiB limit; use bash to inspect a range", path.display(), size)));
    }
    // The same open file may grow after stat, and procfs files often report size zero.
    let mut bytes = Vec::with_capacity(size as usize);
    let mut limited = file.take(MAX_READ_FILE);
    limited.read_to_end(&mut bytes).await.map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    if bytes.len() as u64 == MAX_READ_FILE {
        let mut file = limited.into_inner();
        let mut extra = [0_u8; 1];
        if file.read(&mut extra).await.map_err(|e| err(format!("cannot read {}: {e}", path.display())))? > 0 {
            return Err(err(format!("{} exceeds read's 20 MiB limit; use bash to inspect a range", path.display())));
        }
    }
    // Media conversion consumes exactly the authorized snapshot, never reopens a mutable path.
    let media = crate::media::recognized(&bytes) || is_binary(&bytes);
    if media {
        if let Some((content, content_blocks)) = crate::media::read(&bytes, args).await
            .map_err(|e| err(format!("cannot inspect media: {e:#}")))? {
            return Ok(ToolOutput { content, content_blocks, saved_outputs: Vec::new(), is_error: false });
        }
    }
    if is_binary(&bytes) {
        return Err(err(format!(
            "{} is a binary file ({} bytes), not text; inspect it with bash (e.g. `file`, `xxd | head`)",
            path.display(),
            bytes.len()
        )));
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = crate::secrets::mask(&text);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).map(|l| (l as usize).max(1)).unwrap_or(DEFAULT_READ_LINES);
    let total = text.lines().count();
    if total == 0 {
        return Ok(ToolOutput::ok("[empty file]\n"));
    }
    if offset > total {
        return Err(err(format!("offset {offset} is past the end of the file ({total} lines)")));
    }
    let mut out = String::new();
    let mut last = offset - 1;
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let numbered = format!("{:>6}\t{}\n", i + 1, line);
        if out.len() + numbered.len() > READ_BYTES {
            if out.is_empty() {
                // A single line longer than the whole budget: show its start.
                out.push_str(&numbered[..numbered.floor_char_boundary(READ_BYTES)]);
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
    Ok(ToolOutput::ok(out))
}

// ---------- write / edit ----------

/// Replace a file's content all at once: a temporary file next to it, then a rename, so a crash or a
/// concurrent reader never sees it half written. A symlink is followed (its target is replaced, not
/// the link), and an existing file keeps its permissions.
async fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let target = tokio::fs::canonicalize(path).await.unwrap_or_else(|_| path.to_path_buf());
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.zen-{}", uuid::Uuid::new_v4().simple()));
    let perms = tokio::fs::metadata(&target).await.ok().map(|m| m.permissions());
    let result = async {
        let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, content).await?;
        if let Some(p) = perms {
            file.set_permissions(p).await?;
        }
        tokio::fs::rename(&tmp, &target).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
}

/// For a sandboxed agent, the file at `path` opened for reading and writing, judged by where the
/// opened file really is (`/proc/self/fd`), so a symlink swapped in after the path was checked
/// can't redirect the kernel's write. A symlink as the last part is refused (O_NOFOLLOW), nothing
/// is truncated before the check, a FIFO can't hold the kernel (O_NONBLOCK), and only a regular
/// file with a single link is written. With `create`, a missing file is created (and removed
/// again if it turns out to be outside the sandbox).
async fn open_sandboxed(path: &Path, s: &Sandbox, create: bool) -> Result<tokio::fs::File, ToolOutput> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.read(true).write(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let (opened, created) = match create {
        true => match opts.clone().create_new(true).open(path).await {
            Ok(f) => (Ok(f), true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (opts.open(path).await, false),
            Err(e) => (Err(e), false),
        },
        false => (opts.open(path).await, false),
    };
    let file = opened.map_err(|e| match e.raw_os_error() {
        Some(libc::ELOOP) => err(format!("{} is a symlink; {} writes only regular files (write to the file it points to)", path.display(), s.agent)),
        _ => err(format!("cannot open {}: {e}", path.display())),
    })?;
    let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
    let real = std::fs::read_link(format!("/proc/self/fd/{fd}")).map_err(|e| err(format!("cannot check {}: {e}", path.display())))?;
    if let Err(e) = s.check(&real, true) {
        if created {
            let _ = tokio::fs::remove_file(&real).await;
        }
        return Err(err(e));
    }
    let meta = file.metadata().await.map_err(|e| err(format!("cannot stat {}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(err(format!("{} isn't a regular file", path.display())));
    }
    if std::os::unix::fs::MetadataExt::nlink(&meta) > 1 {
        return Err(err(format!("{} is hardlinked; {} writes only files with a single link", path.display(), s.agent)));
    }
    Ok(file)
}

/// Replace the content of a file opened by `open_sandboxed`, through the same descriptor.
async fn overwrite(mut file: tokio::fs::File, content: &[u8]) -> std::io::Result<()> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};
    file.set_len(0).await?;
    file.seek(std::io::SeekFrom::Start(0)).await?;
    file.write_all(content).await?;
    file.flush().await
}

async fn write(workspace: &Path, args: &Value, sandbox: Option<&Sandbox>) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let content = str_arg(args, "content")?;
    let _guard = lock_files(&[&path]).await;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| err(format!("cannot create {}: {e}", parent.display())))?;
    }
    let written = match sandbox {
        Some(s) => overwrite(open_sandboxed(&path, s, true).await?, content.as_bytes()).await,
        None => write_atomic(&path, content.as_bytes()).await,
    };
    written.map_err(|e| err(format!("cannot write {}: {e}", path.display())))?;
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
    let needle = n.text.as_str();
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

/// A batch applies in order against the preceding edit's result, with one final atomic write.
fn edit_requests(args: &Value) -> Result<Vec<(String, String, bool)>, ToolOutput> {
    let requests = if let Some(batch) = args.get("edits") {
        if args.get("old_text").is_some() || args.get("new_text").is_some() || args.get("replace_all").is_some() {
            return Err(err("use either edits or old_text/new_text, not both"));
        }
        let items = batch.as_array().filter(|v| !v.is_empty() && v.len() <= 100)
            .ok_or_else(|| err("edits must contain 1 to 100 replacements"))?;
        items.iter().collect::<Vec<_>>()
    } else { vec![args] };
    requests.into_iter().map(|a| {
        let old = str_arg(a, "old_text")?.replace("\r\n", "\n");
        let new = str_arg(a, "new_text")?.replace("\r\n", "\n");
        if old.is_empty() { return Err(err("old_text must not be empty")); }
        if old == new { return Err(err("old_text and new_text are identical; nothing to change")); }
        Ok((old, new, a.get("replace_all").and_then(Value::as_bool).unwrap_or(false)))
    }).collect()
}

/// One valid unified hunk spanning the changed lines, with three unchanged context lines.
fn edit_diff(path: &Path, before: &str, after: &str) -> String {
    let old: Vec<&str> = before.split_inclusive('\n').collect();
    let new: Vec<&str> = after.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a,b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a,b)| a == b).count();
    let start = prefix.saturating_sub(3);
    let old_end = (old.len() - suffix + 3).min(old.len());
    let new_end = (new.len() - suffix + 3).min(new.len());
    let mut diff = format!("--- {}\n+++ {}\n@@ -{},{} +{},{} @@\n", path.display(), path.display(),
        if old.is_empty() {0} else {start + 1}, old_end-start,
        if new.is_empty() {0} else {start + 1}, new_end-start);
    let mut line = |mark: char, text: &str| {
        diff.push(mark); diff.push_str(text);
        if !text.ends_with('\n') { diff.push_str("\n\\ No newline at end of file\n"); }
    };
    for text in &old[start..prefix] { line(' ', text); }
    for text in &old[prefix..old.len()-suffix] { line('-', text); }
    for text in &new[prefix..new.len()-suffix] { line('+', text); }
    for text in &old[old.len()-suffix..old_end] { line(' ', text); }
    let diff = crate::secrets::mask(&diff);
    crate::output::preview(&diff).unwrap_or(diff)
}

async fn edit(workspace: &Path, args: &Value, sandbox: Option<&Sandbox>) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let edits = edit_requests(args)?;
    let _guard = lock_files(&[&path]).await;
    // A sandboxed agent's file is read and written through the one descriptor that was checked.
    let (bytes, opened) = match sandbox {
        Some(s) => {
            let mut file = open_sandboxed(&path, s, false).await?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).await.map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
            (bytes, Some(file))
        }
        None => (tokio::fs::read(&path).await.map_err(|e| err(format!("cannot read {}: {e}", path.display())))?, None),
    };
    let original = String::from_utf8(bytes).map_err(|_| err(format!("{} is not a UTF-8 text file", path.display())))?;

    // Match on LF text without a BOM; restore both when writing back.
    let (bom, body) = match original.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", original.as_str()),
    };
    // CRLF only when every line ends so; a file with mixed endings is edited as it is, so lines the
    // edit doesn't touch keep their own endings.
    let newlines = body.matches('\n').count();
    let crlf_lines = body.matches("\r\n").count();
    let crlf = newlines > 0 && crlf_lines == newlines;
    let text = if crlf { body.replace("\r\n", "\n") } else { body.to_string() };

    let mut updated = text.clone();
    let mut n = 0;
    let mut loose = false;
    for (i, (old, new, all)) in edits.iter().enumerate() {
        let (matches, normalized) = find_matches(&updated, old);
        if matches.is_empty() {
            return Err(err(format!("edit {}: old_text not found in {}; no edits written; read the file again", i + 1, path.display())));
        }
        if matches.len() > 1 && !all {
            return Err(err(format!("edit {}: old_text matches {} times; include more surrounding lines or set replace_all; no edits written", i + 1, matches.len())));
        }
        let mut next = String::with_capacity(updated.len() + new.len());
        let mut pos = 0;
        for &(a, b) in if *all { &matches[..] } else { &matches[..1] } {
            next.push_str(&updated[pos..a]);
            next.push_str(new);
            pos = b;
        }
        next.push_str(&updated[pos..]);
        n += if *all { matches.len() } else { 1 };
        loose |= normalized;
        updated = next;
    }
    if updated == text {
        return Err(err("the replacement produced identical content; nothing changed"));
    }
    let diff = edit_diff(&path, &text, &updated);
    let updated = if crlf { updated.replace('\n', "\r\n") } else { updated };
    let updated = format!("{bom}{updated}");
    let written = match opened {
        Some(file) => overwrite(file, updated.as_bytes()).await,
        None => write_atomic(&path, updated.as_bytes()).await,
    };
    written.map_err(|e| err(format!("cannot write {}: {e}", path.display())))?;
    let note = if loose { " (matched after normalizing whitespace and quotes)" } else { "" };
    Ok(ok(format!("replaced {n} occurrence(s) in {}{note}\n{diff}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn scratch(name: &str) -> crate::test_util::TestDir {
        crate::test_util::TestDir::new(name)
    }

    async fn run(ws: &Path, name: &str, args: Value) -> ToolOutput {
        if name == "bash" {
            bash(ws, &args, &[], Some(ws.join("outputs")), None).await.unwrap_or_else(|e| e)
        } else {
            execute(ws, name, &args, &[], None).await
        }
    }

    /// A sandboxed write or edit is judged by the file it actually opened: a link planted between
    /// the path check and the write (simulated by skipping `execute`'s check) can't redirect it.
    #[tokio::test]
    async fn a_sandboxed_write_or_edit_refuses_a_planted_link() {
        use crate::sandbox::{Access, Mount, Sandbox};
        let dir = scratch("sandboxed-write");
        let (ws, out) = (dir.join("ws"), dir.join("outside"));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("token"), "secret").unwrap();
        let s = Sandbox::new("garry", vec![Mount::new(&ws, Access::Write)]);
        let write = |p: &str, c: &str| { let (ws, s, a) = (ws.clone(), s.clone(), json!({"path": p, "content": c})); async move { super::write(&ws, &a, Some(&s)).await.unwrap_or_else(|e| e) } };
        let edit = |p: &str| { let (ws, s, a) = (ws.clone(), s.clone(), json!({"path": p, "old_text": "secret", "new_text": "pwned"})); async move { super::edit(&ws, &a, Some(&s)).await.unwrap_or_else(|e| e) } };
        // A normal write and edit work, and a shorter write leaves nothing of the old content.
        assert!(!write("a.md", "hello secret world").await.is_error);
        assert!(!edit("a.md").await.is_error);
        assert_eq!(std::fs::read_to_string(ws.join("a.md")).unwrap(), "hello pwned world");
        assert!(!write("a.md", "short").await.is_error);
        assert_eq!(std::fs::read_to_string(ws.join("a.md")).unwrap(), "short");
        assert!(!write("new/deep.md", "x").await.is_error);
        // A symlink as the file itself: refused, even one pointing inside.
        std::os::unix::fs::symlink(out.join("token"), ws.join("link")).unwrap();
        std::os::unix::fs::symlink(ws.join("a.md"), ws.join("inner")).unwrap();
        for p in ["link", "inner"] {
            let o = write(p, "pwned").await;
            assert!(o.is_error && o.content.contains("symlink"), "{}", o.content);
            let o = edit(p).await;
            assert!(o.is_error && o.content.contains("symlink"), "{}", o.content);
        }
        // A folder link out: judged by where the opened file is; a file it created there is removed.
        std::os::unix::fs::symlink(&out, ws.join("up")).unwrap();
        for o in [write("up/token", "pwned").await, edit("up/token").await, write("up/new.md", "x").await] {
            assert!(o.is_error && o.content.contains("outside garry's sandbox"), "{}", o.content);
        }
        assert_eq!(std::fs::read_to_string(out.join("token")).unwrap(), "secret");
        assert!(!out.join("new.md").exists());
        // A hardlink is refused too.
        std::fs::hard_link(ws.join("a.md"), ws.join("hard.md")).unwrap();
        assert!(write("hard.md", "x").await.content.contains("hardlinked"));
        assert_eq!(std::fs::read_to_string(ws.join("a.md")).unwrap(), "short");
    }

    #[tokio::test]
    async fn batch_edit_validates_every_replacement_before_writing() {
        use std::os::unix::fs::PermissionsExt;
        let ws = scratch("batch-edit");
        let file = ws.join("a.txt");
        std::fs::write(&file, "alpha\nbeta\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let failed = run(&ws, "edit", json!({"path":"a.txt","edits":[
            {"old_text":"alpha","new_text":"changed"}, {"old_text":"missing","new_text":"x"}]})).await;
        assert!(failed.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha\nbeta\n");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
        let good = run(&ws, "edit", json!({"path":"a.txt","edits":[
            {"old_text":"alpha","new_text":"changed"}, {"old_text":"changed","new_text":"final"}]})).await;
        assert!(!good.is_error, "{}", good.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "final\nbeta\n");
        assert!(good.content.contains("-alpha") && good.content.contains("+final"));
        let mixed = run(&ws, "edit", json!({"path":"a.txt","old_text":"final","new_text":"other","edits":[]})).await;
        assert!(mixed.is_error);
    }


    #[test]
    fn edit_diff_handles_missing_newlines_and_masks_before_cutting() {
        let diff = edit_diff(Path::new("a.txt"), "before", "after");
        assert!(diff.contains("-before\n\\ No newline at end of file"));
        assert!(diff.contains("+after\n\\ No newline at end of file"));
        let large = edit_diff(Path::new("a.txt"), &"x".repeat(30_000), &"y".repeat(30_000));
        assert!(large.len() < crate::output::PREVIEW_BYTES + 200);
    }

    #[tokio::test]
    async fn reading_a_fifo_is_refused_without_blocking() {
        let root = scratch("fifo");
        assert!(std::process::Command::new("mkfifo").arg(root.join("pipe")).status().unwrap().success());
        let result = tokio::time::timeout(Duration::from_secs(2),
            execute(&root, "read", &json!({"path":"pipe"}), &[], None)).await.unwrap();
        assert!(result.is_error && result.content.contains("regular files"));
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
    async fn read_refuses_a_huge_file_before_loading_it() {
        let ws = scratch("huge-read");
        let file = std::fs::File::create(ws.join("huge.txt")).unwrap();
        file.set_len(20 * 1024 * 1024 + 1).unwrap();
        let r = run(&ws, "read", json!({ "path": "huge.txt" })).await;
        assert!(r.is_error);
        assert!(r.content.contains("20 MiB limit"), "{}", r.content);
    }

    #[tokio::test]
    async fn loose_edit_preserves_the_needles_trailing_newline() {
        let ws = scratch("loose-newline");
        std::fs::write(ws.join("a.txt"), "foo  \nbar\n").unwrap();
        let r = run(&ws, "edit", json!({ "path": "a.txt", "old_text": "foo \n", "new_text": "baz\n" })).await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "baz\nbar\n");
    }

    #[tokio::test]
    async fn edit_keeps_mixed_line_endings_and_writes_through_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let ws = scratch("mixed");
        std::fs::write(ws.join("m.txt"), "a\r\nb\nc\r\n").unwrap();
        std::fs::set_permissions(ws.join("m.txt"), std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(ws.join("m.txt"), ws.join("link.txt")).unwrap();
        let r = run(&ws, "edit", json!({ "path": "link.txt", "old_text": "b", "new_text": "B" })).await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(ws.join("m.txt")).unwrap(), "a\r\nB\nc\r\n");
        assert!(std::fs::symlink_metadata(ws.join("link.txt")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::metadata(ws.join("m.txt")).unwrap().permissions().mode() & 0o777, 0o640);
        let leftovers = std::fs::read_dir(&*ws).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".zen-")).count();
        assert_eq!(leftovers, 0);
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
        assert!(r.content.len() <= READ_BYTES + 200);
        let footer = r.content.lines().last().unwrap();
        assert!(footer.contains("continue with offset="), "{footer}");
        let next: usize = footer.rsplit('=').next().unwrap().trim_end_matches(']').parse().unwrap();
        let last_shown = r.content.lines().rev().nth(1).unwrap().trim().split('\t').next().unwrap().parse::<usize>().unwrap();
        assert_eq!(next, last_shown + 1);
        let r = run(&ws, "read", json!({ "path": "big.txt", "offset": 9999 })).await;
        assert!(r.is_error && r.content.contains("past the end"));
        let r = run(&ws, "read", json!({ "path": "." })).await;
        assert!(r.is_error && r.content.contains("is a directory"), "{}", r.content);
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
        // 8s leaves room for a slow login shell (bash -l) to start on a loaded CI runner: at 3s
        // it sometimes hadn't printed "before" yet. A marker unique to this run, so leftovers from other runs can't be mistaken for ours.
        let marker = ws.file_name().unwrap().to_string_lossy().to_string();
        let cmd = format!("echo before; (sleep 60; echo {marker} > leaked.txt) & sleep 60");
        let r = run(&ws, "bash", json!({ "command": cmd, "timeout_secs": 8 })).await;
        assert!(r.is_error);
        assert!(r.content.contains("before"), "{}", r.content);
        assert!(r.content.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let out = std::process::Command::new("pgrep").args(["-f", &format!("echo {marker} ")]).output().unwrap();
        assert!(out.stdout.is_empty(), "background child survived the timeout");
    }

    #[tokio::test]
    async fn bash_background_process_does_not_block() {
        let ws = scratch("bg");
        let started = Instant::now();
        let r = run(&ws, "bash", json!({ "command": "sleep 60 & echo started" })).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("started"));
        // Far below the background sleep, with room for a slow login shell on a loaded runner.
        assert!(started.elapsed() < Duration::from_secs(30), "waited for the background process");
    }

    #[tokio::test]
    async fn bash_large_output_is_saved() {
        let ws = scratch("big");
        let r = run(&ws, "bash", json!({ "command": "seq 1 100000" })).await;
        assert!(!r.is_error);
        assert!(r.content.starts_with("1\n2\n"));
        assert!(r.content.contains("100000\n"));
        let path = r.content.split("saved to ").nth(1).unwrap().split(']').next().unwrap();
        assert!(Path::new(path).starts_with(ws.join("outputs")), "test output must stay in its scratch home");
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 100000);
    }

    #[tokio::test]
    async fn dropping_bash_future_kills_it() {
        let ws = scratch("drop");
        let marker = ws.join("done.txt");
        let args = json!({ "command": "sleep 2; touch done.txt" });
        let fut = execute(&ws, "bash", &args, &[], None);
        let _ = tokio::time::timeout(Duration::from_millis(300), fut).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "command kept running after its future was dropped");
    }
}
