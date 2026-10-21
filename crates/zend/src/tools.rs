//! Built-in tools. The kernel is the only place side effects happen.
//! Commands run on the host in the workspace (a verifier's in a read-only bubblewrap sandbox, and its
//! `read` confined to the work it checks); per-project sandboxes come with M2.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const MAX_OUTPUT: usize = crate::output::PREVIEW_BYTES;
const DEFAULT_READ_LINES: usize = 2000;
const READ_BYTES: usize = crate::output::READ_BYTES;
/// Most bash output kept in memory; the rest is counted but dropped.
const MAX_CAPTURE: usize = crate::output::CAPTURE_BYTES;

/// The file and shell tools, in a fixed order (they are part of the cached prefix). The agent's
/// other tools are added after them (agent.rs). Each description says what the tool does, when to
/// use it and when not, and what it returns (DESIGN.md, "Who teaches what").
pub fn specs() -> Value {
    json!([
        {
            "name": "bash",
            "description": "Run bash in the workspace for searches, git, builds, tests and programs. Use read/write/edit for files. Runs contained: no owner credentials, sudo, Docker or local services; network uses the egress proxy. host:true requests checked access to the owner's host/credentials. Returns combined output and exit status, with a bounded head/tail preview and saved capture when large. Background processes are not a durable service mechanism.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to run" },
                    "host": { "type": "boolean", "description": "Run on the owner's machine with their credentials, outside the sandbox (checked first)" },
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

/// Text plus optional kernel provenance. Dispatch persists it before responding to a worker.
pub struct ToolOutput {
    pub content_blocks: Vec<Value>,
    pub saved_outputs: Vec<PathBuf>,
    /// Kernel-owned provenance, never taken from a worker message or page text.
    pub rating: Option<crate::sources::Rating>,
    pub content: String,
    pub is_error: bool,
}

fn ok(content: impl Into<String>) -> ToolOutput {
    ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), rating: None, content: content.into(), is_error: false }
}

fn err(content: impl Into<String>) -> ToolOutput {
    ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), rating: None, content: content.into(), is_error: true }
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

/// Where a shell runs (bubblewrap). Every session's shell is `Contained` (D-067); `None` is the
/// host as it is, for an owner-approved or gate-approved `host: true` command and the kernel's own
/// upkeep scripts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sandbox {
    None,
    /// Kernel file effects confined by directory descriptors; never follow mutable symlinks.
    GatedFile,
    /// Everything read-only, an empty home with only the work bound back, no network (a verifier).
    ReadOnly,
    /// The session's shell: the machine read-only, the workspace writable when `writable`, the zen
    /// home read-only and its secrets hidden, the session's own /tmp (kept between its commands) and an
    /// empty /run, its own processes, no sudo
    /// (bubblewrap sets no_new_privs), and network only through the session's egress proxy when
    /// `net` (proxy.rs; else none at all).
    Contained { writable: bool, net: bool },
}

/// Run a tool. `env` is added to the environment of commands (bash), which run in `sandbox`. In
/// a read-only sandbox (a verifier) only `read` may run besides, inside the workspace (`confined`).
#[cfg(test)]
pub async fn execute(workspace: &Path, name: &str, args: &Value, env: &[(&str, &str)], sandbox: Sandbox) -> ToolOutput {
    execute_origin(workspace,name,args,env,sandbox,None).await
}

#[derive(Clone, Copy)]
pub struct Origin<'a> { pub db: &'a sqlx::PgPool, pub session: uuid::Uuid, pub masks: &'a [PathBuf] }

/// File provenance is computed from the exact bytes within the file operation, never a reread.
pub async fn execute_origin(workspace: &Path, name: &str, args: &Value, env: &[(&str,&str)], sandbox: Sandbox, origin: Option<Origin<'_>>) -> ToolOutput {
    let read_only = sandbox == Sandbox::ReadOnly;
    if read_only && !matches!(name, "bash" | "read") {
        return err(format!("`{name}` can't run in a read-only session"));
    }
    let result = match name {
        "bash" => bash(workspace, args, env, sandbox, origin.map(|o| o.masks).unwrap_or(&[]), None).await,
        "read" if read_only => match confined(workspace, args) {
            Ok(()) => read(workspace, args, origin).await,
            Err(e) => Err(err(e)),
        },
        "read" => read(workspace, args, origin).await,
        "write" => write(workspace, args, sandbox == Sandbox::GatedFile, origin).await,
        "edit" => edit(workspace, args, sandbox == Sandbox::GatedFile, origin).await,
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

/// bubblewrap's arguments for a read-only view of the machine: everything mounted read-only, a
/// private /tmp, its own processes (the kernel's `/proc/<pid>/environ` and `/proc/<pid>/root` out
/// of sight), its own terminal session, and no network (the database and the kernel's API listen
/// on it). `run_shell` adds an empty home folder with only the work bound back, and hides the
/// secret files.
const READ_ONLY_SANDBOX: [&str; 13] =
    ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--unshare-pid", "--unshare-net", "--new-session", "--die-with-parent"];

/// The shell of a session that has read untrusted content: the same view of the machine, with
/// `/run` emptied too (the sockets there, Docker's and the user's agents, reach past the network
/// namespace). `run_shell` binds the workspace back read-only and hides the secret files.
const OFFLINE_SANDBOX: [&str; 15] = [
    "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--tmpfs", "/run", "--unshare-pid", "--unshare-net", "--new-session", "--die-with-parent",
];

/// What a contained shell's output ends with when its workspace is read-only, so the model knows why.
pub const OFFLINE_NOTE: &str = "[contained: this session read high-risk outside content (or is locked), so its workspace is read-only; only private /tmp is writable, \
and the network reaches only hosts the egress decision allows. For more, end the turn and ask the owner to review /queue, then retry unchanged]";

/// bubblewrap's arguments to hide the secret files: an empty folder over a folder, `/dev/null`
/// over a file.
fn hide_secrets(c: &mut Command, files: impl IntoIterator<Item = PathBuf>) {
    for f in files {
        if f.is_dir() {
            c.arg("--tmpfs").arg(f);
        } else {
            c.arg("--ro-bind").arg("/dev/null").arg(f);
        }
    }
}

/// Why a verifier may not `read` this path, if it may not: only files in the work it checks (its
/// workspace), never a secret file there, however the path is named (symlinks resolved). The same
/// view as its sandboxed shell, without the rest of the machine it doesn't need.
fn confined(workspace: &Path, args: &Value) -> Result<(), String> {
    let path = resolve(workspace, args.get("path").and_then(Value::as_str).unwrap_or(""));
    let real = path.canonicalize().map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let root = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let secret = crate::secrets::secret_files().iter().filter_map(|f| f.canonicalize().ok()).any(|f| real.starts_with(f));
    if !real.starts_with(&root) || secret {
        return Err(format!("a verifier reads only the work it checks (in {}), and no secret files", root.display()));
    }
    Ok(())
}

/// What a command printed (stdout and stderr as they arrived; past MAX_CAPTURE only counted) and how
/// it ended: `None` when it timed out and its process group was killed.
pub struct Shell {
    pub text: String,
    pub status: Option<std::io::Result<std::process::ExitStatus>>,
}

/// Linux seccomp ABI (seccomp_data: nr at byte 0, arch at byte 4). Network namespaces do not
/// isolate pathname Unix sockets, so deny connection/datagram operations and io_uring bypasses.
/// Reject foreign syscall ABIs (including x32) rather than comparing the wrong syscall numbers.
pub(crate) fn network_filter() -> Result<std::fs::File,String> {
    let mut rules = Vec::new();
    for nr in [libc::SYS_connect,libc::SYS_sendto,libc::SYS_sendmsg,libc::SYS_sendmmsg,libc::SYS_io_uring_setup] {
        rules.push((0x15,0,1,nr as u32));
        rules.push((0x06,0,0,0x00050001)); // SECCOMP_RET_ERRNO | EPERM
    }
    seccomp(rules)
}

/// The contained sandbox up to the program to run (D-067): the machine read-only, `dir` writable
/// when `writable`, `tmp` (else an empty one) as /tmp, an empty /run, the zen home read-only with
/// its secrets hidden, `masks` unreadable, and the network only through the proxy `socket` (the
/// command then runs under `zend bridge`). Shells (`run_shell`) and MCP stdio servers (mcp.rs) use
/// it; append the program and its arguments. `filter` must stay open until the spawn.
pub fn contained_command(filter: &std::fs::File, dir: &Path, writable: bool, tmp: Option<&Path>, socket: Option<&Path>, masks: &[PathBuf]) -> Command {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home".into()));
    let mut c = sandbox_command(filter);
    c.args(OFFLINE_SANDBOX);
    // A home under the emptied /tmp (a test's) comes back read-only; then the workspace; then the
    // zen home read-only again (prompt files, skills, the wiki change only through the kernel's
    // tools), its secrets hidden.
    if let Some(tmp) = tmp {
        c.arg("--bind").arg(tmp).arg("/tmp");
    }
    if home.starts_with("/tmp") || home.starts_with("/run") {
        c.arg("--ro-bind").arg(&home).arg(&home);
    }
    c.arg(if writable { "--bind" } else { "--ro-bind" }).arg(dir).arg(dir);
    let zen = crate::zen_home();
    if zen.exists() {
        c.arg("--ro-bind").arg(&zen).arg(&zen);
    }
    hide_secrets(&mut c, crate::secrets::secret_files());
    // A read-only directory does not isolate pathname Unix sockets. Hide every proxy capability
    // first, then bind back only this process's socket below.
    let proxies = crate::proxy::dir();
    if proxies.exists() { c.arg("--tmpfs").arg(proxies); }
    crate::fsview::mask(&mut c, masks);
    let bridge = std::env::var("ZEN_BRIDGE_BIN").map(PathBuf::from).or_else(|_| std::env::current_exe());
    match (socket, bridge) {
        (Some(sock), Ok(exe)) => {
            let proxy = format!("http://127.0.0.1:{}", crate::proxy::BRIDGE_PORT);
            c.arg("--bind").arg(sock).arg(sock);
            for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "ALL_PROXY"] {
                c.env(k, &proxy);
            }
            c.env("NO_PROXY", "localhost,127.0.0.1").env("no_proxy", "localhost,127.0.0.1");
            c.arg("--chdir").arg(dir).arg(&exe).arg("bridge").arg(sock).arg(crate::proxy::BRIDGE_PORT.to_string());
        }
        _ => {
            c.arg("--chdir").arg(dir);
        }
    }
    c
}

/// The seccomp filter for a contained process: io_uring refused always; with no network, every
/// connect and send too.
pub fn contained_filter(net: bool) -> Result<std::fs::File, String> {
    if net { uring_filter() } else { network_filter() }
}

/// A session's private /tmp for its contained shells (`<system tmp>/zen-sandbox/<session>`, mode 700;
/// the nightly `prune` job removes those a week old).
pub fn session_tmp(session: &str) -> Option<PathBuf> {
    let id: uuid::Uuid = session.parse().ok()?;
    let root = std::env::temp_dir().join("zen-sandbox");
    let dir = root.join(id.to_string());
    std::fs::create_dir_all(&dir).ok()?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    Some(dir)
}

/// A contained shell with the proxy: only io_uring (which bypasses seccomp's socket rules) is refused.
fn uring_filter() -> Result<std::fs::File,String> {
    seccomp(vec![(0x15,0,1,libc::SYS_io_uring_setup as u32), (0x06,0,0,0x00050001)])
}

/// A host shell with masks (fsview.rs) keeps the network but makes no Unix socket: a host
/// daemon, tmux server or container socket would run commands outside the masks. As Anthropic's
/// sandbox-runtime does by default: `socket(AF_UNIX, ...)` and io_uring fail with EPERM.
fn unix_filter() -> Result<std::fs::File,String> {
    seccomp(vec![
        (0x15,0,3,libc::SYS_socket as u32), (0x20,0,0,16), (0x15,0,1,libc::AF_UNIX as u32), (0x06,0,0,0x00050001),
        (0x20,0,0,0), (0x15,0,1,libc::SYS_io_uring_setup as u32), (0x06,0,0,0x00050001),
    ])
}

/// A seccomp program: foreign ABIs killed, x32 refused, then `rules` on the syscall number
/// (loaded in A), then allow.
fn seccomp(rules: Vec<(u16,u8,u8,u32)>) -> Result<std::fs::File,String> {
    use std::io::{Seek,Write};
    use std::os::fd::FromRawFd;
    #[cfg(target_arch="x86_64")]
    let arch = 0xc000003e;
    #[cfg(target_arch="aarch64")]
    let arch = 0xc00000b7;
    #[cfg(not(any(target_arch="x86_64",target_arch="aarch64")))]
    return Err("sandbox seccomp supports x86_64 and aarch64 only".into());
    let mut instructions: Vec<(u16,u8,u8,u32)> = vec![
        (0x20,0,0,4), (0x15,1,0,arch), (0x06,0,0,0x80000000),
        (0x20,0,0,0), (0x45,0,1,0x40000000), (0x06,0,0,0x00050001),
    ];
    instructions.extend(rules);
    instructions.push((0x06,0,0,0x7fff0000)); // SECCOMP_RET_ALLOW
    // SAFETY: static NUL-terminated name; the returned fd is owned exactly once by File.
    let fd = unsafe {libc::memfd_create(c"zen-offline-filter".as_ptr(),libc::MFD_CLOEXEC)};
    if fd < 0 {return Err(format!("creating sandbox filter: {}",std::io::Error::last_os_error()));}
    let mut file = unsafe {std::fs::File::from_raw_fd(fd)};
    for (code,jt,jf,k) in instructions {
        let mut instruction = Vec::from(code.to_ne_bytes());
        instruction.extend([jt,jf]);
        instruction.extend(k.to_ne_bytes());
        file.write_all(&instruction).map_err(|e|format!("writing sandbox filter: {e}"))?;
    }
    file.rewind().map_err(|e|format!("rewinding sandbox filter: {e}"))?;
    Ok(file)
}

fn sandbox_command(filter: &std::fs::File) -> Command {
    use std::os::fd::AsRawFd;
    let fd = filter.as_raw_fd();
    let mut c = Command::new("bwrap");
    c.arg("--seccomp").arg(fd.to_string());
    // SAFETY: fcntl is async-signal-safe. Change only the child's fd flags, and keep the
    // parent's owning File alive until spawn; bwrap consumes/closes this descriptor.
    unsafe {c.pre_exec(move || {
        if libc::fcntl(fd,libc::F_SETFD,0) < 0 {return Err(std::io::Error::last_os_error());}
        Ok(())
    });}
    c
}

/// Run bash in its own process group, killed on timeout or cancellation. Sandbox modes use
/// read-only mounts, private writable /tmp, isolated processes/network and a socket syscall
/// filter; None is the explicit host-capability route. Kernel secret variables are removed.
/// `masks` (fsview.rs) are files the shell must not see: each is unreadable (a nodev /dev/null), in
/// every mode; a host shell with masks runs in its own pid namespace with no_new_privs.
pub async fn run_shell(dir: &Path, command: &str, env: &[(&str, &str)], sandbox: Sandbox, masks: &[PathBuf], timeout: Duration) -> Result<Shell, String> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home".into()));
    let filter = match sandbox {
        Sandbox::ReadOnly | Sandbox::Contained { net: false, .. } => Some(network_filter()?),
        // The bridge reaches the proxy over a Unix socket; the host's sockets are out of sight
        // (/run and /tmp emptied, secret paths hidden), abstract ones in another namespace.
        Sandbox::Contained { net: true, .. } => Some(uring_filter()?),
        _ if !masks.is_empty() => Some(unix_filter()?),
        _ => None,
    };
    let mut cmd = match sandbox {
        Sandbox::ReadOnly => {
            let mut c = sandbox_command(filter.as_ref().expect("sandbox filter initialized"));
            // The home folder (keys, sign-ins, the zen home) is replaced by an empty one, as for a
            // made tool (workshop.rs); the workspace is bound back after it and the private /tmp, in
            // case it lives under either. Secret files still inside it are hidden.
            c.args(READ_ONLY_SANDBOX).arg("--tmpfs").arg(&home).arg("--ro-bind").arg(dir).arg(dir);
            // (Those under the home but outside the work are gone with it already.)
            hide_secrets(&mut c, crate::secrets::secret_files().into_iter().filter(|f| !f.starts_with(&home) || f.starts_with(dir)));
            crate::fsview::mask(&mut c, masks);
            c.arg("--chdir").arg(dir).args(["bash", "-c"]);
            c
        }
        Sandbox::Contained { writable, net } => {
            let session = env.iter().find(|(k, _)| *k == "ZEN_SESSION_ID").map(|(_, v)| v.to_string());
            // The session's own /tmp, kept between its commands (build caches, scratch files) and
            // never shared with the host's or another session's.
            let tmp = session.as_deref().and_then(session_tmp);
            let socket = session.and_then(|s| s.parse().ok()).map(crate::proxy::socket_path).filter(|p| net && p.exists());
            let mut c = contained_command(filter.as_ref().expect("sandbox filter initialized"), dir, writable, tmp.as_deref(), socket.as_deref(), masks);
            c.args(["bash", "-c"]);
            c
        }
        Sandbox::GatedFile => return Err("file policy cannot execute shell commands".into()),
        Sandbox::None if masks.is_empty() => {
            let mut c = Command::new("bash");
            c.arg("-c");
            c
        }
        Sandbox::None => {
            // The host as it is, but with its own /proc (no other process's root to reach
            // masked files through), background processes ending with the shell, and no sudo.
            let mut c = sandbox_command(filter.as_ref().expect("sandbox filter initialized"));
            c.args(["--dev-bind", "/", "/", "--unshare-pid", "--proc", "/proc", "--die-with-parent"]);
            // Nor through the kernel's own API: its token is hidden too.
            hide_secrets(&mut c, [crate::zen_home().join("token")].into_iter().filter(|t| t.exists()));
            crate::fsview::mask(&mut c, masks);
            c.arg("--chdir").arg(dir).args(["bash", "-c"]);
            c
        }
    };
    // The kernel's secrets (API keys, its token, the database URL) stay out of the agent's shell.
    for (k, _) in std::env::vars().filter(|(k, _)| crate::secrets::is_secret_var(k)) {
        cmd.env_remove(k);
    }
    let mut child = cmd
        .arg(command)
        .current_dir(dir)
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| match sandbox {
            Sandbox::ReadOnly => format!("the read-only shell (bubblewrap) couldn't start: {e}; use read instead"),
            Sandbox::Contained { .. } => format!("the contained shell (bubblewrap) couldn't start: {e}; every session's shell runs contained"),
            Sandbox::None | Sandbox::GatedFile => format!("failed to start bash: {e}"),
        })?;
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

async fn bash(workspace: &Path, args: &Value, env: &[(&str, &str)], sandbox: Sandbox, masks: &[PathBuf], output_dir: Option<PathBuf>) -> Result<ToolOutput, ToolOutput> {
    let command = str_arg(args, "command")?;
    let timeout = args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(120).clamp(1, 600);
    let Shell { text, status } = run_shell(workspace, command, env, sandbox, masks, Duration::from_secs(timeout)).await.map_err(err)?;
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
    if matches!(sandbox, Sandbox::Contained { writable: false, .. }) {
        body.push_str(OFFLINE_NOTE);
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

async fn read(workspace: &Path, args: &Value, origin: Option<Origin<'_>>) -> Result<ToolOutput, ToolOutput> {
    const MAX_READ_FILE: u64 = crate::media::MAX_FILE_BYTES as u64;
    let path = resolve(workspace, str_arg(args, "path")?);
    let file = tokio::fs::OpenOptions::new().read(true).custom_flags(0x800).open(&path).await
        .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    let meta = file.metadata().await.map_err(|e| err(format!("cannot stat {}: {e}", path.display())))?;
    // Check the opened object, not just its name, to defeat symlink swaps and hardlink aliases.
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let opened = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|e| err(format!("cannot validate opened file: {e}")))?;
    if ["/proc", "/sys", "/dev", "/run"].iter().any(|p| opened.starts_with(p))
        || crate::secrets::secret_files().iter().any(|p| opened.starts_with(p)
            || std::fs::metadata(p).is_ok_and(|s| s.dev() == meta.dev() && s.ino() == meta.ino())) {
        return Err(err("cannot read a secret or process/device file"));
    }
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
        let rating = match origin {
            Some(o) => Some(crate::sources::read_binary_rating(o.db, o.session, &opened, &bytes).await
                .map_err(|e| err(format!("media provenance unavailable: {e}")))?),
            None => None,
        };
        if let Some((content, content_blocks)) = crate::media::read(&bytes, args).await
            .map_err(|e| err(format!("cannot inspect media: {e:#}")))? {
            return Ok(ToolOutput { rating, content, content_blocks, saved_outputs: Vec::new(), is_error: false });
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
    let rating = if let Some(origin) = origin {
        crate::sources::depend_file(origin.db,origin.session,&text).await.map_err(|e|err(format!("file origin dependencies unavailable: {e}")))?;
        Some(crate::sources::Rating {risk: crate::sources::file_rating(origin.db,&opened,&text).await.map_err(|e|err(format!("file provenance unavailable: {e}")))?,origin:format!("file:{}",opened.display())})
    } else {None};
    let text = crate::secrets::mask(&text);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).map(|l| (l as usize).max(1)).unwrap_or(DEFAULT_READ_LINES);
    let total = text.lines().count();
    if total == 0 {
        return Ok(ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), rating,content:"[empty file]\n".into(),is_error:false});
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
    Ok(ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), rating,content:out,is_error:false})
}

// ---------- write / edit ----------

/// Replace a file's content all at once: a temporary file next to it, then a rename, so a crash or a
/// concurrent reader never sees it half written. A symlink is followed (its target is replaced, not
/// the link), and an existing file keeps its permissions.
async fn write_atomic(path: &Path, content: &[u8], follow: bool) -> std::io::Result<()> {
    let target = if follow {
        tokio::fs::canonicalize(path).await.unwrap_or_else(|_| path.to_path_buf())
    } else { path.to_path_buf() };
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

/// Anchor every parent directory in an open descriptor. Concurrent symlink replacements cannot
/// redirect the write. The final rename replaces a link itself, never writes through it.
fn anchored(workspace: &Path, path: &Path) -> std::io::Result<(std::fs::File, PathBuf)> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let root = workspace.canonicalize()?;
    let relative = path.strip_prefix(workspace).map_err(|_| std::io::Error::other("outside workspace"))?;
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() || components.iter().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(std::io::Error::other("non-normal file path"));
    }
    let open_dir = |p: &Path| std::fs::OpenOptions::new().read(true).custom_flags(0x10000 | 0x20000).open(p);
    let mut dir = open_dir(&root)?;
    for component in &components[..components.len() - 1] {
        let next = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(component.as_os_str());
        match std::fs::create_dir(&next) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(e) => return Err(e),
        }
        dir = open_dir(&next)?;
    }
    let file = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(components.last().unwrap().as_os_str());
    Ok((dir, file))
}

async fn write(workspace: &Path, args: &Value, gated: bool, origin: Option<Origin<'_>>) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let content = str_arg(args, "content")?;
    let _guard = lock_files(&[&path]).await;
    let anchor = if gated { Some(anchored(workspace, &path).map_err(|e| err(format!("cannot confine write: {e}")))?) } else { None };
    if !gated {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| err(format!("cannot create {}: {e}", parent.display())))?;
        }
    }
    let target = anchor.as_ref().map(|(_, p)| p.as_path()).unwrap_or(&path);
    if let Some(o) = origin { crate::sources::record_bytes(o.db,o.session,&path,content,None).await.map_err(|e|err(format!("not written: provenance unavailable: {e}")))?; }
    write_atomic(target, content.as_bytes(), !gated).await.map_err(|e| err(format!("cannot write {}: {e}", path.display())))?;
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

async fn edit(workspace: &Path, args: &Value, gated: bool, origin: Option<Origin<'_>>) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(workspace, str_arg(args, "path")?);
    let edits = edit_requests(args)?;
    let _guard = lock_files(&[&path]).await;
    let anchor = if gated { Some(anchored(workspace, &path).map_err(|e| err(format!("cannot confine edit: {e}")))?) } else { None };
    let target = anchor.as_ref().map(|(_, p)| p.as_path()).unwrap_or(&path);
    let bytes = if gated {
        let file = tokio::fs::OpenOptions::new().read(true).custom_flags(0x20000).open(target).await
            .map_err(|e| err(format!("cannot open confined edit: {e}")))?;
        let meta = file.metadata().await.map_err(|e| err(format!("cannot stat confined edit: {e}")))?;
        use std::os::unix::fs::MetadataExt;
        if !meta.is_file() || meta.len() > 16 * 1024 * 1024 ||
            crate::secrets::secret_files().iter().any(|p| std::fs::metadata(p)
                .is_ok_and(|s| s.dev() == meta.dev() && s.ino() == meta.ino())) {
            return Err(err("confined edit refuses a secret, non-regular or oversized file"));
        }
        let mut bytes = Vec::new();
        file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes).await
            .map_err(|e| err(format!("cannot read confined edit: {e}")))?;
        if bytes.len() > 16 * 1024 * 1024 { return Err(err("confined edit exceeds 16 MiB")); }
        bytes
    } else {
        tokio::fs::read(&path).await.map_err(|e| err(format!("cannot read {}: {e}", path.display())))?
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
    if let Some(o) = origin { crate::sources::record_bytes(o.db,o.session,&path,&updated,Some(&original)).await.map_err(|e|err(format!("not edited: provenance unavailable: {e}")))?; }
    write_atomic(target, updated.as_bytes(), !gated)
        .await
        .map_err(|e| err(format!("cannot write {}: {e}", path.display())))?;
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
            bash(ws, &args, &[], Sandbox::None, &[], Some(ws.join("outputs"))).await.unwrap_or_else(|e| e)
        } else {
            execute(ws, name, &args, &[], Sandbox::None).await
        }
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
            execute(&root, "read", &json!({"path":"pipe"}), &[], Sandbox::None)).await.unwrap();
        assert!(result.is_error && result.content.contains("regular files"));
    }

    /// Check actual mount and syscall enforcement (D-067): a high-risk or locked session's shell
    /// can't write the workspace or reach the network; any contained shell keeps private /tmp,
    /// can't use sudo (no_new_privs), and sees the zen home read-only.
    #[tokio::test]
    async fn contained_shells_hold_their_bounds() {
        let root = scratch("contained");
        std::fs::write(root.join("AGENTS.md"),"owner instructions").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let command = format!("if echo changed > AGENTS.md 2>/dev/null; then echo WRITE_OPEN; else echo WRITE_BLOCKED; fi; echo scratch > /tmp/zen-code-output; cat /tmp/zen-code-output; python3 -c 'import socket; s=socket.socket(); s.settimeout(1); print(\"NET_BLOCKED\" if s.connect_ex((\"127.0.0.1\", {port})) else \"NET_OPEN\")'; sudo -n true 2>/dev/null && echo ROOT; touch {}/x 2>/dev/null && echo HOMEDIR_OPEN; echo end", crate::zen_home().display());
        let out = run_shell(&root,&command,&[],Sandbox::Contained { writable: false, net: false },&[],Duration::from_secs(10)).await.unwrap();
        assert!(out.text.contains("WRITE_BLOCKED") && out.text.contains("NET_BLOCKED") && out.text.contains("scratch"),"{}",out.text);
        assert!(!out.text.contains("WRITE_OPEN") && !out.text.contains("NET_OPEN") && !out.text.contains("ROOT") && !out.text.contains("HOMEDIR_OPEN"), "{}", out.text);
        assert_eq!(std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),"owner instructions");
        // A session's /tmp is its own and lasts between its commands.
        let sid = uuid::Uuid::new_v4().to_string();
        let env = [("ZEN_SESSION_ID", sid.as_str())];
        run_shell(&root,"echo kept > /tmp/zen-scratch",&env,Sandbox::Contained { writable: true, net: false },&[],Duration::from_secs(10)).await.unwrap();
        let out = run_shell(&root,"cat /tmp/zen-scratch",&env,Sandbox::Contained { writable: false, net: false },&[],Duration::from_secs(10)).await.unwrap();
        assert!(out.text.contains("kept"), "{}", out.text);
        assert!(!std::path::Path::new("/tmp/zen-scratch").exists(), "not the host's /tmp");
        // A low-risk session's shell writes the workspace; the host's loopback stays unreachable
        // (the shell has its own network namespace; outside traffic goes through the proxy).
        let out = run_shell(&root,&command,&[],Sandbox::Contained { writable: true, net: true },&[],Duration::from_secs(10)).await.unwrap();
        assert!(out.text.contains("WRITE_OPEN") && out.text.contains("NET_BLOCKED") && !out.text.contains("ROOT") && !out.text.contains("HOMEDIR_OPEN"), "{}", out.text);
        assert_eq!(std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),"changed\n");
        // The host route (an approved `host: true` command) is the host.
        let out = run_shell(&root,"echo approved > AGENTS.md",&[],Sandbox::None,&[],Duration::from_secs(10)).await.unwrap();
        assert!(out.status.unwrap().unwrap().success());
        assert_eq!(std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),"approved\n");
    }

    #[tokio::test]
    async fn sandbox_modes_cannot_connect_to_host_unix_sockets() {
        let root = scratch("unix-boundary");
        let _stream = std::os::unix::net::UnixListener::bind(root.join("stream.sock")).unwrap();
        let _datagram = std::os::unix::net::UnixDatagram::bind(root.join("datagram.sock")).unwrap();
        let command = format!("python3 -c 'import socket; s=socket.socket(socket.AF_UNIX); print(\"STREAM_BLOCKED\" if s.connect_ex(\"{0}/stream.sock\") == 1 else \"STREAM_OPEN\")'; python3 -c 'import socket; s=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM)\ntry:\n s.sendto(b\"x\", \"{0}/datagram.sock\"); print(\"DGRAM_OPEN\")\nexcept PermissionError:\n print(\"DGRAM_BLOCKED\")'",root.display());
        for mode in [Sandbox::Contained { writable: false, net: false },Sandbox::ReadOnly] {
            let out = run_shell(&root,&command,&[],mode,&[],Duration::from_secs(10)).await.unwrap();
            assert!(out.text.contains("STREAM_BLOCKED") && out.text.contains("DGRAM_BLOCKED"),"{mode:?}: {}",out.text);
            assert!(!out.text.contains("_OPEN"),"{mode:?}: {}",out.text);
        }
    }

    #[tokio::test]
    async fn gated_writes_never_follow_symlinks() {
        let root = scratch("gated-write");
        let ws = root.join("work");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(root.join("outside.txt"), "untouched").unwrap();
        std::os::unix::fs::symlink(root.join("outside.txt"), ws.join("link.txt")).unwrap();
        let r = execute(&ws, "write", &json!({"path":"link.txt","content":"local"}), &[], Sandbox::GatedFile).await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(root.join("outside.txt")).unwrap(), "untouched");
        assert_eq!(std::fs::read_to_string(ws.join("link.txt")).unwrap(), "local");
        std::os::unix::fs::symlink(&*root, ws.join("escape")).unwrap();
        let r = execute(&ws, "write", &json!({"path":"escape/outside.txt","content":"bad"}), &[], Sandbox::GatedFile).await;
        assert!(r.is_error);
        assert_eq!(std::fs::read_to_string(root.join("outside.txt")).unwrap(), "untouched");
        let r = execute(&ws, "edit", &json!({"path":"link.txt","old_text":"local","new_text":"edited"}), &[], Sandbox::GatedFile).await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(ws.join("link.txt")).unwrap(), "edited");
    }

    #[tokio::test]
    async fn a_verifier_reads_only_its_work() {
        let root = scratch("confined");
        let ws = root.join("work");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("a.txt"), "inside\n").unwrap();
        std::fs::write(root.join("secret.txt"), "outside\n").unwrap();
        std::os::unix::fs::symlink(root.join("secret.txt"), ws.join("link")).unwrap();
        assert!(!execute(&ws, "read", &json!({ "path": "a.txt" }), &[], Sandbox::ReadOnly).await.is_error);
        for p in ["../secret.txt", "link", "/proc/self/environ"] {
            let r = execute(&ws, "read", &json!({ "path": p }), &[], Sandbox::ReadOnly).await;
            assert!(r.is_error && r.content.contains("reads only the work it checks"), "{p}: {}", r.content);
        }
        assert!(!run(&ws, "read", json!({ "path": "../secret.txt" })).await.is_error, "other sessions aren't confined");
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

    /// A masked file can't be read in any shell mode, through a symlink and through another
    /// process's root; other files stay readable and a host shell can still write.
    #[tokio::test]
    async fn masked_files_are_empty_in_every_shell() {
        let d = crate::test_util::TestDir::new("masks");
        let ws = d.canonicalize().unwrap();
        std::fs::write(ws.join("bad"), "SECRET-PAYLOAD").unwrap();
        std::fs::write(ws.join("ok"), "fine").unwrap();
        std::os::unix::fs::symlink(ws.join("bad"), ws.join("link")).unwrap();
        let masks = vec![ws.join("bad")];
        let cmd = "cat bad link ok; cat /proc/1/root$PWD/bad 2>/dev/null; echo";
        for mode in [Sandbox::None, Sandbox::Contained { writable: true, net: false }, Sandbox::ReadOnly] {
            let out = run_shell(&ws, cmd, &[], mode, &masks, Duration::from_secs(10)).await.unwrap();
            assert!(!out.text.contains("SECRET") && out.text.contains("fine"), "{mode:?}: {}", out.text);
        }
        let out = run_shell(&ws, "echo new > made; sudo -n true 2>/dev/null && echo ROOT; python3 -c 'import socket; socket.socket(socket.AF_UNIX)' 2>/dev/null && echo UNIX; python3 -c 'import socket; socket.socket()' && echo inet; (sleep 30 &) ; echo done",
            &[], Sandbox::None, &masks, Duration::from_secs(10)).await.unwrap();
        assert!(out.text.contains("done") && out.text.contains("inet") && !out.text.contains("ROOT") && !out.text.contains("UNIX"), "{}", out.text);
        assert_eq!(std::fs::read_to_string(ws.join("made")).unwrap(), "new\n");
        assert_eq!(std::fs::read_to_string(ws.join("bad")).unwrap(), "SECRET-PAYLOAD", "the host file is untouched");
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
        let fut = execute(&ws, "bash", &args, &[], Sandbox::None);
        let _ = tokio::time::timeout(Duration::from_millis(300), fut).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "command kept running after its future was dropped");
    }
}
