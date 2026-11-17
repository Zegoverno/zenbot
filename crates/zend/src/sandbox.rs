//! The sandbox of every agent but zenbot. One list of mounts (`Agent::mounts`) says what the agent
//! can reach; `bash` (and `verify` checks, script jobs) runs inside bubblewrap built from it, and
//! `read`/`write`/`edit` refuse any path outside it. zenbot runs unsandboxed, as the owner.
//!
//! The sandbox's root is empty: the system folders are bound read-only, the owner's home is an
//! empty tmpfs (so `HOME` stays the owner's path, writable but thrown away after each command:
//! tools find `~/.gitconfig` and `~/.local/bin` where they expect them and can write caches), and
//! on top of it only the agent's own folders, the shared instructions and the paths its
//! `agent.toml` names. Its own folder (`agents/<name>/`) is read-only but for `IDENTITY.md`, `roles.toml` and its
//! default workspace `work/`: the kernel acts on the rest (`agent.toml`, `mcp.json`, `SOUL.md`,
//! `skills/`), so the agent can't change those or create a missing one. A mount deeper in the tree wins over a shallower one, in bubblewrap (bound
//! later) and in `check` (the longest match) alike. `/tmp` is private. The PID namespace is the
//! agent's own, so a background process ends with its command.
//!
//! The network is private too: bubblewrap runs inside pasta (`passt`), a network namespace of its
//! own with outbound internet but no host loopback and no gateway mapping, so the
//! agent can't reach services on the host's 127.0.0.1 (the kernel, the web app, local engines) that
//! trust whoever connects or forgeable headers. IPv4 only (`-4`: pasta otherwise warns on a host
//! without IPv6, into the agent's output); no port forwarding either way. pasta passes on the exit
//! status, stays in the command's process group (killing the group on a timeout ends it all) and
//! its child dies with it, as bubblewrap's (`--die-with-parent`). DNS goes to the host's resolver;
//! when that is on the host's loopback (systemd-resolved's 127.0.0.53, unreachable from here), to
//! the upstream servers it uses itself (`/run/systemd/resolve/resolv.conf`), named in a resolv.conf
//! of the sandbox's own (`zen/sandbox/resolv.conf`, bound over the host's) and given to pasta (`-D`).
//!
//! The kernel runs the file tools itself, outside the sandbox, so a path is checked twice: before,
//! for a clear message, and on the file actually opened (`/proc/self/fd`), since the agent's `bash`
//! can swap a symlink in between. `read` checks what it opened; `write`/`edit` open without
//! following a final symlink and without truncating, check, then write through that same
//! descriptor (tools.rs `open_sandboxed`). Residual risk: a folder swapped for a link out between
//! the check and the open can make the kernel create empty folders (`create_dir_all`) or, for a
//! moment, an empty file outside (removed when the check fails); never content, and no existing
//! file outside is changed.
//!
//! The kernel also reads and writes in the agent's vault for it (wiki pages it indexes, records,
//! `MEMORY.md`), and reads instruction files (`AGENTS.md`) of its workspace into its prompt: an agent
//! can plant a link there, so those go through the file actually opened too (`read_vault_file`,
//! `open_vault_dir`: never out of the vault the path names; `context::read_for`: only what its
//! sandbox shows it).
//!
//! bubblewrap and pasta are required: without either a sandboxed agent's `bash` fails; it never
//! runs unsandboxed or on the host's network.

use std::path::{Component, Path, PathBuf};

/// Where bubblewrap is expected.
pub const BWRAP: &str = "/usr/bin/bwrap";

/// Where pasta (package `passt`) is expected.
pub const PASTA: &str = "/usr/bin/pasta";

/// pasta's arguments up to `--`: a new network namespace with outbound access only (no gateway
/// mapped to the host, no ports forwarded), IPv4, quiet.
const PASTA_ARGS: &[&str] = &["--config-net", "--no-map-gw", "-T", "none", "-U", "none", "--quiet", "-4"];

/// The host's resolver settings.
const RESOLV: &str = "/etc/resolv.conf";

/// systemd-resolved's own upstream servers, when the host's resolver is it (on loopback).
const RESOLVED_UPSTREAM: &str = "/run/systemd/resolve/resolv.conf";

/// Whether a resolv.conf names only resolvers on loopback (none of them reachable from the
/// sandbox's own network).
fn loopback_dns(resolv: &str) -> bool {
    let servers: Vec<&str> = resolv.lines().filter_map(|l| l.trim().strip_prefix("nameserver")).map(str::trim).collect();
    !servers.is_empty() && servers.iter().all(|s| s.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
}

/// The resolvers a resolv.conf names that aren't on loopback.
fn upstream_servers(resolv: &str) -> Vec<String> {
    resolv
        .lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .map(str::trim)
        .filter(|s| s.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| !ip.is_loopback()))
        .map(String::from)
        .collect()
}

/// The resolvers the sandbox uses instead of the host's, when those are on loopback: the upstream
/// servers systemd-resolved uses itself. None when the host's resolver is reachable as it is (or
/// nothing better is known).
fn sandbox_servers() -> Option<Vec<String>> {
    std::fs::read_to_string(RESOLV).ok().filter(|t| loopback_dns(t))?;
    Some(upstream_servers(&std::fs::read_to_string(RESOLVED_UPSTREAM).ok()?)).filter(|s| !s.is_empty())
}

/// A resolv.conf like `resolv`, but naming `servers` as its resolvers.
fn sandbox_resolv(resolv: &str, servers: &[String]) -> String {
    let mut out = String::from("# zenbot's sandbox: the host's resolver is on its loopback; its upstream servers instead.\n");
    for s in servers {
        out.push_str(&format!("nameserver {s}\n"));
    }
    for l in resolv.lines().filter(|l| !l.trim().starts_with("nameserver") && !l.trim().starts_with('#') && !l.trim().is_empty()) {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// System folders bound read-only (those that exist; a symlink, as `/bin` on a usrmerge system, is
/// recreated as one).
const SYSTEM: &[&str] = &["/usr", "/bin", "/lib", "/lib64", "/lib32", "/libx32", "/etc", "/opt", "/sbin", "/snap", "/var/lib"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Visible, read-only.
    Read,
    /// Visible and writable.
    Write,
    /// An empty tmpfs: hides what's below; the file tools never reach it.
    Empty,
}

/// One mount: `path` as the agent sees it, `real` the host path behind it (symlinks resolved).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub path: PathBuf,
    pub real: PathBuf,
    pub access: Access,
}

impl Mount {
    pub fn new(path: impl Into<PathBuf>, access: Access) -> Mount {
        let path = path.into();
        let real = path.canonicalize().unwrap_or_else(|_| path.clone());
        Mount { path, real, access }
    }
}

/// An agent's sandbox: its name (for messages) and its mounts, parents before children.
#[derive(Clone, Debug)]
pub struct Sandbox {
    pub agent: String,
    pub mounts: Vec<Mount>,
    /// No network at all (a plugin without `network`): bubblewrap's own empty network namespace
    /// (`--unshare-net`, loopback only) instead of pasta's.
    pub offline: bool,
    pinned: Vec<(PathBuf, std::sync::Arc<std::fs::File>)>,
}

impl Sandbox {
    pub fn new(agent: &str, mut mounts: Vec<Mount>) -> Sandbox {
        // Parents first (a stable sort keeps the given order among equals): a child is bound on top.
        mounts.sort_by_key(|m| m.path.components().count());
        Sandbox {
            agent: agent.to_string(),
            mounts,
            offline: false,
            pinned: Vec::new(),
        }
    }

    /// Pin plugin grants before spawn. Every component is opened relative to its pinned
    /// parent with O_NOFOLLOW: a replacement symlink is refused, never canonicalized.
    pub fn pin_plugin_sources(
        &mut self,
        dirs: &[&Path],
        files: Option<&Path>,
        runtime: Option<&Path>,
    ) -> anyhow::Result<()> {
        for path in dirs.iter().copied().chain(files).chain(runtime) {
            let fd = std::sync::Arc::new(pin_path(path)?);
            let mount = self
                .mounts
                .iter()
                .find(|m| m.path == path)
                .ok_or_else(|| anyhow::anyhow!("missing plugin mount {}", path.display()))?;
            self.pinned.push((mount.path.clone(), fd));
        }
        Ok(())
    }

    /// Resolve and create the scoped grant relative to the already pinned root, never
    /// through a host pathname. Renaming any parent cannot redirect mkdir or the bind.
    pub fn pin_plugin_scope(&mut self, root: &Path, sub: &Path) -> anyhow::Result<()> {
        let relative = sub.strip_prefix(root)?;
        anyhow::ensure!(!relative.as_os_str().is_empty()
            && relative.components().all(|p| matches!(p, Component::Normal(_)))
            && relative.components().next().unwrap().as_os_str() != "agents", "invalid scoped plugin grant");
        let (_, root_fd) = self.pinned.iter().find(|(p, _)| p == root)
            .ok_or_else(|| anyhow::anyhow!("files root is not pinned"))?;
        let fd = pin_relative_dir(root_fd.try_clone()?, relative)?;
        anyhow::ensure!(self.mounts.iter().any(|m| m.path == sub), "scoped mount missing");
        self.pinned.push((sub.to_path_buf(), std::sync::Arc::new(fd)));
        Ok(())
    }

    fn inherit_pins(&self, cmd: &mut tokio::process::Command) {
        use std::os::fd::AsRawFd;
        let pins = self.pinned.clone();
        // Only the forked child drops CLOEXEC. The parent retains CLOEXEC, so concurrent
        // commands cannot inherit these grants. Captured Arcs keep them alive until spawn.
        unsafe {
            cmd.pre_exec(move || {
                for (_, fd) in &pins {
                    if libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }

    /// The same sandbox with no network at all.
    pub fn offline(mut self) -> Sandbox {
        self.offline = true;
        self
    }

    /// Why `path` is out of reach (for writing when `write`), if it is. Symlinks are resolved and
    /// `..` handled; a path that doesn't exist yet is judged by its nearest existing parent.
    pub fn check(&self, path: &Path, write: bool) -> Result<(), String> {
        let real = real_path(path);
        let found = self.mounts.iter().enumerate().filter(|(_, m)| real.starts_with(&m.real)).max_by_key(|(i, m)| (m.real.components().count(), *i)).map(|(_, m)| m);
        match found.map(|m| m.access) {
            Some(Access::Write) => Ok(()),
            Some(Access::Read) if !write => Ok(()),
            Some(Access::Read) => Err(format!("{} is read-only for {}; it can write only under: {}", path.display(), self.agent, self.roots(true))),
            _ => Err(format!("{} is outside {}'s sandbox; it can reach only: {}", path.display(), self.agent, self.roots(false))),
        }
    }

    /// The roots it may write (`write`) or reach, for messages.
    fn roots(&self, write: bool) -> String {
        let r: Vec<String> = self.mounts.iter().filter(|m| if write { m.access == Access::Write } else { m.access != Access::Empty }).map(|m| m.path.display().to_string()).collect();
        r.join(", ")
    }

    /// bubblewrap's arguments up to `--`: the mounts, then the namespaces, starting in `cwd`.
    pub fn args(&self, cwd: &Path) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        let s = |p: &Path| p.display().to_string();
        for m in &self.mounts {
            match m.access {
                Access::Empty => a.extend(["--tmpfs".into(), s(&m.path)]),
                access if self.pinned.iter().any(|(p, _)| p == &m.path) => {
                    use std::os::fd::AsRawFd;
                    let (_, fd) = self.pinned.iter().find(|(p, _)| p == &m.path).unwrap();
                    let flag = if access == Access::Write {
                        "--bind-fd"
                    } else {
                        "--ro-bind-fd"
                    };
                    a.extend([flag.into(), fd.as_raw_fd().to_string(), s(&m.path)]);
                }
                access => match std::fs::read_link(&m.path) {
                    // A system symlink (`/bin -> usr/bin`) is recreated, not bound.
                    Ok(target) if m.path.parent() == Some(Path::new("/")) => a.extend(["--symlink".into(), s(&target), s(&m.path)]),
                    _ => {
                        let flag = if access == Access::Write { "--bind" } else { "--ro-bind" };
                        a.extend([flag.into(), s(&m.real), s(&m.path)]);
                    }
                },
            }
        }
        if self.offline {
            a.push("--unshare-net".into());
        }
        a.extend(
            ["--dev", "/dev", "--proc", "/proc", "--unshare-pid", "--unshare-ipc", "--unshare-uts", "--die-with-parent", "--new-session", "--chdir"]
                .map(String::from),
        );
        a.push(s(cwd));
        a
    }

    /// A command running `bash -c command` in the sandbox, from `cwd`; an error when `cwd` is outside
    /// it or bubblewrap or pasta is missing (never a fallback to running unsandboxed).
    pub fn command(&self, cwd: &Path, command: &str) -> Result<tokio::process::Command, String> {
        self.wrap(cwd, "bash", ["-c", command])
    }

    /// A command running `program` with `args` in the sandbox, from `cwd` (its own MCP servers run
    /// so); the same errors as `command`.
    pub fn wrap<I, S>(&self, cwd: &Path, program: &str, args: I) -> Result<tokio::process::Command, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        if !Path::new(BWRAP).is_file() {
            return Err(format!("{}'s commands run only sandboxed, and bubblewrap isn't installed ({BWRAP}); install it with `sudo apt-get install -y bubblewrap`", self.agent));
        }
        self.check(cwd, false).map_err(|e| format!("can't run there: {e}"))?;
        if self.offline {
            let mut cmd = tokio::process::Command::new(BWRAP);
            cmd.args(self.args(cwd)).arg("--").arg(program).args(args).current_dir("/");
            self.inherit_pins(&mut cmd);
            return Ok(cmd);
        }
        if !Path::new(PASTA).is_file() {
            return Err(format!("{}'s commands run only on a private network, and pasta isn't installed ({PASTA}); install it with `sudo apt-get install -y passt`", self.agent));
        }
        let mut cmd = tokio::process::Command::new(PASTA);
        cmd.args(PASTA_ARGS);
        for s in sandbox_servers().unwrap_or_default() {
            cmd.args(["-D", &s]);
        }
        cmd.arg("--").arg(BWRAP).args(self.args(cwd)).arg("--").arg(program).args(args).current_dir("/");
        self.inherit_pins(&mut cmd);
        Ok(cmd)
    }
}

/// Open an absolute path without following any symlink, even in intermediate components.
fn pin_path(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    if !path.is_absolute() {
        return Err(std::io::Error::other("plugin mount must be absolute"));
    }
    let mut parent = std::fs::File::open("/")?;
    for part in path.components() {
        let Component::Normal(name) = part else {
            if part == Component::RootDir {
                continue;
            }
            return Err(std::io::Error::other("invalid plugin mount component"));
        };
        let name = std::ffi::CString::new(name.as_bytes())?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        parent = unsafe { std::fs::File::from_raw_fd(fd) };
        if parent.metadata()?.file_type().is_symlink() {
            return Err(std::io::Error::other("symlink in plugin mount"));
        }
    }
    Ok(parent)
}

/// Create/open each scope component under a directory fd. O_DIRECTORY rejects symlinks
/// together with O_NOFOLLOW, including a symlink installed between mkdirat and openat.
fn pin_relative_dir(mut parent: std::fs::File, relative: &Path) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    for part in relative.components() {
        let Component::Normal(name) = part else { return Err(std::io::Error::other("invalid scoped grant")); };
        let name = std::ffi::CString::new(name.as_bytes())?;
        let made = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if made < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists { return Err(error); }
        }
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 { return Err(std::io::Error::last_os_error()); }
        parent = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(parent)
}

/// `path` with symlinks resolved and `..` handled; for a path that doesn't exist, its nearest
/// existing parent resolved and the rest added (with `..` applied lexically).
pub fn real_path(path: &Path) -> PathBuf {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(base) = cur.canonicalize() {
            let mut out = base;
            for part in rest.iter().rev() {
                match Path::new(part).components().next() {
                    Some(Component::ParentDir) => {
                        out.pop();
                    }
                    Some(Component::CurDir) | None => {}
                    _ => out.push(part),
                }
            }
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent.to_path_buf();
            }
            // `..` or `.` at the end, or the root: take the component off lexically.
            (Some(parent), None) if !parent.as_os_str().is_empty() => {
                rest.push(cur.components().next_back().map(|c| c.as_os_str().to_os_string()).unwrap_or_default());
                cur = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The vault a path of the global vault is named under: `global/agents/<name>/` (an agent's) or
/// `global/` (zenbot's, other agents' vaults excluded); None for a path outside the global vault.
fn vault_root(global: &Path, named: &Path) -> Option<(PathBuf, bool)> {
    let agents = global.join("agents");
    match named.strip_prefix(&agents).ok().and_then(|r| r.components().next()) {
        Some(Component::Normal(n)) => Some((agents.join(n), true)),
        _ => named.starts_with(global).then(|| (global.to_path_buf(), false)),
    }
}

/// Whether `real` (a resolved path) is really in the vault `named` is under: an agent can link
/// any path into its own vault (and, with `global = "write"`, into the global one), and the kernel
/// must not read or write the file behind the link for it.
fn confined_in(global: &Path, named: &Path, real: &Path) -> bool {
    match vault_root(global, named) {
        None => true,
        Some((root, own)) => {
            let root = real_path(&root);
            real.starts_with(&root) && (own || !real.starts_with(root.join("agents")))
        }
    }
}

fn outside(named: &Path, real: &Path) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{} leads out of its vault ({})", named.display(), real.display()))
}

/// Where the open `file` really is.
fn fd_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(file)))
}

/// The kernel's read of a vault file (wiki pages, records, `MEMORY.md`): refused when the file
/// really is outside the vault it's named under (a symlink as its last part, or a folder on the
/// way swapped for one), judged on the file actually opened, so no swap after a check helps.
pub fn read_vault_file(named: &Path) -> std::io::Result<String> {
    read_vault_file_in(&crate::layout::global_dir(), named)
}

fn read_vault_file_in(global: &Path, named: &Path) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(named)?;
    let real = fd_path(&file)?;
    if !confined_in(global, named, &real) {
        return Err(outside(named, &real));
    }
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(format!("{} isn't a regular file", named.display())));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// The folder `dir` of a vault file the kernel is about to write, opened, once it is sure to be in
/// the vault: write through `/proc/self/fd/<fd>/<name>` so no folder swapped afterwards redirects it.
pub fn open_vault_dir(dir: &Path) -> std::io::Result<std::fs::File> {
    open_vault_dir_in(&crate::layout::global_dir(), dir)
}

fn open_vault_dir_in(global: &Path, dir: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(dir)?;
    let real = fd_path(&file)?;
    if !confined_in(global, dir, &real) {
        return Err(outside(dir, &real));
    }
    Ok(file)
}

/// The mounts for `agent`'s sandbox. `user_home` is the owner's home, `zen` the zen home, `global`
/// the global vault; `workspace`, `vault` and `dir` the agent's; `read`/`write` its extra paths.
pub struct Layout<'a> {
    pub user_home: &'a Path,
    pub zen: &'a Path,
    pub global: &'a Path,
    pub dir: &'a Path,
    pub vault: &'a Path,
    pub workspace: &'a Path,
    pub global_level: crate::agents::Global,
    pub read: &'a [PathBuf],
    pub write: &'a [PathBuf],
}

pub fn mounts(l: &Layout) -> Vec<Mount> {
    use crate::agents::Global;
    let mut m = system_mounts(l.zen, true);
    m.push(Mount::new(l.user_home, Access::Empty));
    m.push(Mount::new("/tmp", Access::Empty));
    for rel in [".local/node", ".local/bin", ".cargo/bin", ".gitconfig"] {
        let p = l.user_home.join(rel);
        if p.exists() {
            m.push(Mount::new(p, Access::Read));
        }
    }
    for f in ["USER.md", "AGENTS.md"] {
        if l.zen.join(f).is_file() {
            m.push(Mount::new(l.zen.join(f), Access::Read));
        }
    }
    match l.global_level {
        Global::None => {}
        level => {
            m.push(Mount::new(l.global, if level == Global::Write { Access::Write } else { Access::Read }));
            // Other agents' vaults stay hidden; its own is bound back below.
            m.push(Mount::new(l.global.join("agents"), Access::Empty));
        }
    }
    m.push(Mount::new(l.vault, Access::Write));
    // Its folder is read-only: the kernel trusts what's in it (agent.toml, mcp.json, SOUL.md,
    // skills/), so the agent must not create or change those. It keeps only its IDENTITY.md (made
    // before the mount: `Agent::mounts`) and its default workspace `work/`, bound back writable.
    m.push(Mount::new(l.dir, Access::Read));
    // Both files it keeps itself: its IDENTITY.md and its roles.toml (roles.rs).
    for kept in ["IDENTITY.md", crate::roles::FILE] {
        if l.dir.join(kept).is_file() {
            m.push(Mount::new(l.dir.join(kept), Access::Write));
        }
    }
    m.push(Mount::new(l.workspace, Access::Write));
    m.extend(l.read.iter().map(|p| Mount::new(p, Access::Read)));
    m.extend(l.write.iter().map(|p| Mount::new(p, Access::Write)));
    m
}

/// The system folders, read-only, and with `dns` what name resolution needs.
fn system_mounts(zen: &Path, dns: bool) -> Vec<Mount> {
    let mut m = Vec::new();
    for p in SYSTEM {
        if std::fs::symlink_metadata(p).is_ok() {
            m.push(Mount::new(*p, Access::Read));
        }
    }
    // DNS: /etc/resolv.conf often points into /run. A resolver on the host's loopback (systemd-
    // resolved's 127.0.0.53) isn't reachable from the agent's own network: it gets a resolv.conf
    // naming that resolver's upstream servers instead (`sandbox_servers`).
    let target = Path::new(RESOLV).canonicalize().ok();
    if let Some(dir) = target.as_ref().and_then(|r| r.parent().map(Path::to_path_buf)).filter(|d| !d.starts_with("/etc")) {
        m.push(Mount::new(dir, Access::Read));
    }
    if let (true, Some(target), Some(text), Some(servers)) = (dns, target, std::fs::read_to_string(RESOLV).ok(), sandbox_servers()) {
        let file = zen.join("sandbox/resolv.conf");
        let ours = sandbox_resolv(&text, &servers);
        let written = std::fs::read_to_string(&file).is_ok_and(|t| t == ours)
            || (std::fs::create_dir_all(zen.join("sandbox")).is_ok() && std::fs::write(&file, &ours).is_ok());
        if written {
            m.push(Mount { path: target, real: file, access: Access::Read });
        }
    }
    m
}

/// What a plugin's server reaches (plugins.rs): the system folders, the node runtime's folder, its
/// own folder read-only, its data folder read-write and, as its permissions say, the files root
/// (other agents' vaults under it hidden) read-only or read-write, or read-only with one subfolder
/// writable. The owner's home is otherwise an empty tmpfs: no `.ssh`, no zen home, no tokens.
pub struct PluginLayout<'a> {
    pub user_home: &'a Path,
    pub zen: &'a Path,
    pub plugin: &'a Path,
    pub data: &'a Path,
    /// The folder holding the node runtime, when it isn't under a system folder.
    pub runtime: Option<&'a Path>,
    pub files: Option<(&'a Path, Access)>,
    pub write_sub: Option<&'a Path>,
    pub network: bool,
}

pub fn plugin_mounts(l: &PluginLayout) -> Vec<Mount> {
    let mut m = system_mounts(l.zen, l.network);
    m.push(Mount::new(l.user_home, Access::Empty));
    m.push(Mount::new("/tmp", Access::Empty));
    if let Some(r) = l.runtime {
        m.push(Mount::new(r, Access::Read));
    }
    if let Some((files, access)) = l.files {
        m.push(Mount::new(files, access));
        if files.join("agents").is_dir() {
            m.push(Mount::new(files.join("agents"), Access::Empty));
        }
        if let Some(sub) = l.write_sub {
            m.push(Mount::new(sub, Access::Write));
        }
    }
    m.push(Mount::new(l.plugin, Access::Read));
    m.push(Mount::new(l.data, Access::Write));
    m
}

/// Why an extra path (`workspace`, `read`, `write` in `agent.toml`) can't be given to the agent
/// whose folder is `dir` and vault `vault`, if it can't: it must be an existing absolute path,
/// neither the zen home, a folder holding it, nor inside it except in the agent's own folder or vault.
pub fn refuse_path(p: &Path, zen: &Path, dir: &Path, vault: &Path) -> Option<String> {
    if !p.is_absolute() {
        return Some(format!("{} isn't an absolute path", p.display()));
    }
    let Ok(real) = p.canonicalize() else { return Some(format!("{} doesn't exist", p.display())) };
    let zen = zen.canonicalize().unwrap_or_else(|_| zen.to_path_buf());
    let own = |d: &Path| real.starts_with(d.canonicalize().unwrap_or_else(|_| d.to_path_buf()));
    if zen.starts_with(&real) || (real.starts_with(&zen) && !own(dir) && !own(vault)) {
        return Some(format!("{} would expose the zen home ({}): only the agent's own folder and vault are allowed there", p.display(), zen.display()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Global;

    #[test]
    fn plugin_bindings_survive_rename_and_refuse_symlink_replacement() {
        use std::os::unix::fs::symlink;
        let t = crate::test_util::TestDir::new("plugin-pins");
        let grant = t.join("grant");
        let moved = t.join("moved");
        let outside = t.join("outside");
        std::fs::create_dir(&grant).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(grant.join("marker"), "granted").unwrap();
        std::fs::write(outside.join("marker"), "secret").unwrap();
        let mut sandbox = Sandbox::new("plugin", vec![Mount::new(&grant, Access::Write)]).offline();
        sandbox.pin_plugin_sources(&[&grant], None, None).unwrap();
        let mut commands = Vec::new();
        if Path::new(BWRAP).is_file() {
            for offline in [true, false] {
                if !offline && !Path::new(PASTA).is_file() {
                    continue;
                }
                let mut runnable = sandbox.clone();
                runnable.offline = offline;
                // System executables plus pinned grant, without widening owner files.
                runnable
                    .mounts
                    .extend(system_mounts(&t.join("zen"), !offline));
                commands.push(runnable.wrap(&grant, "/bin/cat", ["marker"]).unwrap());
            }
        }
        std::fs::rename(&grant, &moved).unwrap();
        symlink(&outside, &grant).unwrap();
        use std::os::fd::AsRawFd;
        let fd = sandbox.pinned[0].1.as_raw_fd();
        assert_eq!(
            std::fs::read_to_string(format!("/proc/self/fd/{fd}/marker")).unwrap(),
            "granted"
        );
        assert!(sandbox
            .args(&grant)
            .windows(2)
            .any(|w| w == ["--bind-fd", &fd.to_string()]));
        // Deterministic swap between mount validation and pinning must fail closed.
        let mut raced = Sandbox::new(
            "plugin",
            vec![Mount {
                path: grant.clone(),
                real: grant.clone(),
                access: Access::Write,
            }],
        );
        assert!(raced.pin_plugin_sources(&[&grant], None, None).is_err());
        assert!(pin_path(&grant.join("nested")).is_err());
        for mut command in commands {
            let out = command.as_std_mut().output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(out.stdout, b"granted");
        }
    }

    #[test]
    fn scoped_grants_walk_the_pinned_root_and_never_follow_replacements() {
        use std::os::unix::fs::symlink;
        use std::os::fd::AsRawFd;
        let t = crate::test_util::TestDir::new("scope-pins");
        let root = t.join("files");
        let moved = t.join("moved");
        let outside = t.join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let scope = root.join("Reading/new");
        let mut sandbox = Sandbox::new("plugin", vec![
            Mount::new(&root, Access::Read), Mount::new(&scope, Access::Write),
        ]);
        sandbox.pin_plugin_sources(&[], Some(&root), None).unwrap();
        // Swap the root after validation and opening, before scope resolution/creation.
        std::fs::rename(&root, &moved).unwrap();
        symlink(&outside, &root).unwrap();
        sandbox.pin_plugin_scope(&root, &scope).unwrap();
        assert!(moved.join("Reading/new").is_dir());
        assert!(!outside.join("Reading").exists());
        let fd = sandbox.pinned.last().unwrap().1.as_raw_fd();
        std::fs::rename(moved.join("Reading"), moved.join("old")).unwrap();
        symlink(&outside, moved.join("Reading")).unwrap();
        std::fs::write(format!("/proc/self/fd/{fd}/allowed"), "yes").unwrap();
        assert_eq!(std::fs::read_to_string(moved.join("old/new/allowed")).unwrap(), "yes");
        assert!(!outside.join("allowed").exists());
        assert!(sandbox.pin_plugin_scope(&root, &scope).is_err());
        assert!(!outside.join("new").exists(), "no mkdir through a replacement symlink");
    }

    struct Home {
        _t: crate::test_util::TestDir,
        user: PathBuf,
        zen: PathBuf,
    }

    fn home() -> Home {
        let t = crate::test_util::TestDir::new("sandbox");
        let user = t.join("home");
        let zen = user.join(".zenbot");
        for d in ["global/agents/garry/wiki", "global/agents/other", "global/wiki", "agents/garry/work", "agents/other"] {
            std::fs::create_dir_all(zen.join(d)).unwrap();
        }
        for f in ["token", "USER.md", "AGENTS.md", "agents/garry/agent.toml", "agents/garry/SOUL.md", "agents/garry/IDENTITY.md", "agents/garry/roles.toml", "global/wiki/page.md", "global/agents/other/secret.md"] {
            std::fs::write(zen.join(f), "x").unwrap();
        }
        std::fs::create_dir_all(user.join("repo")).unwrap();
        Home { _t: t, user, zen }
    }

    fn sandbox(h: &Home, level: Global, read: &[PathBuf], write: &[PathBuf]) -> Sandbox {
        let (global, dir) = (h.zen.join("global"), h.zen.join("agents/garry"));
        let (vault, workspace) = (global.join("agents/garry"), dir.join("work"));
        let l = Layout { user_home: &h.user, zen: &h.zen, global: &global, dir: &dir, vault: &vault, workspace: &workspace, global_level: level, read, write };
        Sandbox::new("garry", mounts(&l))
    }

    #[test]
    fn the_file_tools_reach_only_the_mounts() {
        let h = home();
        let s = sandbox(&h, Global::None, &[], &[]);
        let z = &h.zen;
        // Its own folders, writable.
        for p in ["agents/garry/work/new/file.md", "global/agents/garry/wiki/x.md", "agents/garry/IDENTITY.md", "agents/garry/roles.toml"] {
            assert_eq!(s.check(&z.join(p), true), Ok(()), "{p}");
        }
        // The rest of its folder (settings it has or not, skills, any new file) and the shared
        // instructions: read only.
        for p in ["agents/garry/agent.toml", "agents/garry/SOUL.md", "agents/garry/mcp.json", "agents/garry/skills/x/SKILL.md", "agents/garry/new.md", "USER.md"] {
            assert!(s.check(&z.join(p), false).is_ok() && s.check(&z.join(p), true).unwrap_err().contains("read-only"), "{p}");
        }
        // Everything else of the home is out of reach.
        for p in ["token", "global/wiki/page.md", "global/agents/other/secret.md", "agents/other/SOUL.md", "../repo/x", "agents/garry/work/../../other/x"] {
            let e = s.check(&z.join(p), false).unwrap_err();
            assert!(e.contains("outside garry's sandbox") && e.contains("work"), "{p}: {e}");
        }
        assert!(s.check(&h.user.join(".ssh/id"), false).is_err() && s.check(Path::new("/tmp/x"), true).is_err());
        assert!(s.check(Path::new("/usr/bin/env"), false).is_ok() && s.check(Path::new("/usr/bin/env"), true).is_err());
        // A symlink out of its workspace leads nowhere.
        std::os::unix::fs::symlink(z.join("token"), z.join("agents/garry/work/link")).unwrap();
        assert!(s.check(&z.join("agents/garry/work/link"), false).is_err());
        std::os::unix::fs::symlink(z, z.join("agents/garry/work/dir")).unwrap();
        assert!(s.check(&z.join("agents/garry/work/dir/token"), false).is_err());
        assert!(s.check(&z.join("agents/garry/work/dir/agents/garry/work/f"), true).is_ok(), "resolved back inside");
    }

    #[test]
    fn the_global_vault_and_extra_paths() {
        let h = home();
        let z = &h.zen;
        let read = sandbox(&h, Global::Read, &[], &[]);
        assert!(read.check(&z.join("global/wiki/page.md"), false).is_ok() && read.check(&z.join("global/wiki/page.md"), true).is_err());
        assert!(read.check(&z.join("global/agents/other/secret.md"), false).is_err(), "other vaults stay hidden");
        assert!(read.check(&z.join("global/agents/garry/wiki/x"), true).is_ok(), "its own vault on top");
        let write = sandbox(&h, Global::Write, &[], &[h.user.join("repo")]);
        assert!(write.check(&z.join("global/wiki/new.md"), true).is_ok() && write.check(&z.join("global/agents/other/secret.md"), false).is_err());
        assert!(write.check(&h.user.join("repo/a/b"), true).is_ok());
        // Parents are bound before children.
        let depth: Vec<usize> = write.mounts.iter().map(|m| m.path.components().count()).collect();
        assert!(depth.windows(2).all(|w| w[0] <= w[1]));
        let (dir, vault) = (z.join("agents/garry"), z.join("global/agents/garry"));
        assert!(refuse_path(&h.user.join("repo"), z, &dir, &vault).is_none());
        assert!(refuse_path(&dir.join("work"), z, &dir, &vault).is_none());
        for bad in [h.user.clone(), z.clone(), z.join("token"), z.join("agents/other"), z.join("global")] {
            assert!(refuse_path(&bad, z, &dir, &vault).unwrap().contains("zen home"), "{}", bad.display());
        }
        assert!(refuse_path(Path::new("rel"), z, &dir, &vault).is_some() && refuse_path(&h.user.join("nope"), z, &dir, &vault).unwrap().contains("doesn't exist"));
    }

    #[tokio::test]
    async fn the_file_tools_refuse_outside_it() {
        let h = home();
        let s = sandbox(&h, Global::None, &[], &[]);
        let ws = h.zen.join("agents/garry/work");
        let run = |name: &'static str, args: serde_json::Value| {
            let (s, ws) = (s.clone(), ws.clone());
            async move { crate::tools::execute(&ws, name, &args, &[], Some(&s)).await }
        };
        assert!(!run("write", serde_json::json!({"path": "notes.md", "content": "hi"})).await.is_error);
        assert!(!run("read", serde_json::json!({"path": "notes.md"})).await.is_error);
        let out = run("read", serde_json::json!({"path": "../../../token"})).await;
        assert!(out.is_error && out.content.contains("outside garry's sandbox"), "{}", out.content);
        let out = run("edit", serde_json::json!({"path": "../agent.toml", "old_text": "x", "new_text": "y"})).await;
        assert!(out.is_error && out.content.contains("read-only"), "{}", out.content);
        // Nothing new in its folder (a missing mcp.json, skills) but in work/; it keeps IDENTITY.md.
        for p in ["../mcp.json", "../agent2.toml", "../skills/x/SKILL.md"] {
            let out = run("write", serde_json::json!({"path": p, "content": "{}"})).await;
            assert!(out.is_error && out.content.contains("read-only"), "{p}: {}", out.content);
        }
        assert!(!h.zen.join("agents/garry/mcp.json").exists() && !h.zen.join("agents/garry/skills").exists());
        assert!(!run("edit", serde_json::json!({"path": "../IDENTITY.md", "old_text": "x", "new_text": "y"})).await.is_error);
        assert!(!run("edit", serde_json::json!({"path": "../roles.toml", "old_text": "x", "new_text": "y"})).await.is_error, "it keeps its roles");
        std::os::unix::fs::symlink(h.zen.join("token"), ws.join("t")).unwrap();
        assert!(run("read", serde_json::json!({"path": "t"})).await.is_error, "a link out");
    }

    #[test]
    fn the_kernel_reads_and_writes_a_vault_only_inside_it() {
        let h = home();
        let g = h.zen.join("global");
        let own = g.join("agents/garry/wiki");
        std::fs::write(own.join("ok.md"), "fine").unwrap();
        assert_eq!(read_vault_file_in(&g, &own.join("ok.md")).unwrap(), "fine");
        // Links out of its vault, to another vault, or to the home: refused, whatever the name.
        for (name, target) in [("token.md", h.zen.join("token")), ("other.md", g.join("agents/other/secret.md")), ("global.md", g.join("wiki/page.md"))] {
            std::os::unix::fs::symlink(&target, own.join(name)).unwrap();
            assert!(read_vault_file_in(&g, &own.join(name)).is_err(), "{name}");
        }
        // A folder of its vault swapped for a link out.
        std::os::unix::fs::symlink(g.join("agents/other"), g.join("agents/garry/memory")).unwrap();
        assert!(read_vault_file_in(&g, &g.join("agents/garry/memory/secret.md")).is_err());
        assert!(open_vault_dir_in(&g, &g.join("agents/garry/memory")).is_err() && open_vault_dir_in(&g, &own).is_ok());
        // The global vault never reaches into an agent's, through a link in it.
        std::os::unix::fs::symlink(g.join("agents/other/secret.md"), g.join("wiki/leak.md")).unwrap();
        assert!(read_vault_file_in(&g, &g.join("wiki/leak.md")).is_err() && read_vault_file_in(&g, &g.join("wiki/page.md")).is_ok());
        // Outside the global vault: not the vault's business.
        assert!(read_vault_file_in(&g, &h.zen.join("token")).is_ok());
    }

    #[test]
    fn a_resolver_on_loopback_is_replaced_by_its_upstream() {
        let resolved = "# systemd-resolved\nnameserver 127.0.0.53\noptions edns0 trust-ad\nsearch lan\n";
        assert!(loopback_dns(resolved) && loopback_dns("nameserver ::1"));
        assert!(!loopback_dns("nameserver 169.254.169.254") && !loopback_dns("nameserver 127.0.0.53\nnameserver 1.1.1.1") && !loopback_dns(""));
        let upstream = upstream_servers("# resolved's upstream\nnameserver 168.63.129.16\nnameserver 127.0.0.1\nnameserver fe80::1\nsearch lan\n");
        assert_eq!(upstream, ["168.63.129.16"], "IPv4 off loopback only (the sandbox is IPv4)");
        let ours = sandbox_resolv(resolved, &upstream);
        assert!(ours.contains("nameserver 168.63.129.16\n") && !ours.contains("127.0.0.53") && ours.contains("options edns0 trust-ad\nsearch lan\n"), "{ours}");
    }

    #[test]
    fn real_paths() {
        let h = home();
        let z = h.zen.canonicalize().unwrap();
        assert_eq!(real_path(&h.zen.join("agents/garry/work/a/../b")), z.join("agents/garry/work/b"));
        assert_eq!(real_path(&h.zen.join("nope/../token")), z.join("token"));
        assert_eq!(real_path(&h.zen.join("agents/garry/work/x/y")), z.join("agents/garry/work/x/y"));
    }

    /// Runs bubblewrap in pasta for real (skipped where either isn't installed; CI installs both).
    #[tokio::test]
    async fn bash_in_the_sandbox() {
        if let Some(missing) = [BWRAP, PASTA].into_iter().find(|p| !Path::new(p).is_file()) {
            eprintln!("note: {missing} missing; the sandbox's bash test is skipped");
            return;
        }
        let h = home();
        let z = &h.zen;
        let ws = z.join("agents/garry/work");
        async fn run(s: &Sandbox, ws: &Path, cmd: String) -> (bool, String) {
            let out = s.command(ws, &cmd).unwrap().output().await.unwrap();
            (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
        }
        let none = sandbox(&h, Global::None, &[], &[]);
        let (ok, out) = run(&none, &ws, "echo hi > made && echo v > ../../../global/agents/garry/wiki/v.md && pwd && cat made".into()).await;
        assert!(ok && out.contains("hi"), "{out}");
        assert!(z.join("agents/garry/work/made").is_file() && z.join("global/agents/garry/wiki/v.md").is_file());
        let probe = format!(
            "for p in {0}/token {0}/global/agents/other/secret.md {0}/global/wiki/page.md {0}/agents/other; do test -e $p && echo SEEN $p; done; echo y > {0}/agents/garry/agent.toml || echo settings-ro",
            z.display()
        );
        let (_, out) = run(&none, &ws, probe).await;
        assert!(!out.contains("SEEN") && out.contains("settings-ro"), "{out}");
        assert_eq!(std::fs::read_to_string(z.join("agents/garry/agent.toml")).unwrap(), "x");
        // Its folder is read-only but for IDENTITY.md and work/: it can't create the files the
        // kernel trusts (mcp.json would run servers as the owner, agent.toml widen its sandbox).
        let dir = z.join("agents/garry");
        std::fs::remove_file(dir.join("agent.toml")).unwrap();
        let creates = ["mcp.json", "agent.toml", "new.md", "skills/x/SKILL.md"]
            .map(|f| format!("(mkdir -p $(dirname {f}) 2>/dev/null; echo '{{}}' > {f}) 2>/dev/null && echo MADE {f}; mv work/made moved 2>/dev/null && echo MADE moved;"))
            .join(" ");
        let cmd = format!("cd {} && {creates} echo me >> IDENTITY.md && echo w > work/w && echo done", dir.display());
        let (ok, out) = run(&none, &ws, cmd).await;
        assert!(ok && out.contains("done") && !out.contains("MADE"), "{out}");
        for f in ["mcp.json", "agent.toml", "new.md", "skills", "moved"] {
            assert!(!dir.join(f).exists(), "{f} was created");
        }
        assert_eq!(std::fs::read_to_string(dir.join("IDENTITY.md")).unwrap(), "xme\n");
        assert!(dir.join("work/w").is_file());
        let cmd = format!("cat {0}/global/wiki/page.md && echo y > {0}/global/wiki/page.md || echo global-ro; test -e {0}/global/agents/other/secret.md && echo SEEN", z.display());
        let (_, out) = run(&sandbox(&h, Global::Read, &[], &[]), &ws, cmd).await;
        assert!(out.starts_with('x') && out.contains("global-ro") && !out.contains("SEEN"), "{out}");
        // Outside its mounts it doesn't start at all.
        assert!(sandbox(&h, Global::None, &[], &[]).command(&h.user.join("repo"), "true").unwrap_err().contains("outside garry's sandbox"));

        // Its network is its own: the host's loopback services are out of reach (the same probe
        // reaches the listener from the host), and pasta prints nothing of its own.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = format!("(exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null && echo REACHED || echo unreachable");
        let host = std::process::Command::new("bash").arg("-c").arg(&probe).output().unwrap();
        assert!(String::from_utf8_lossy(&host.stdout).contains("REACHED"), "the probe works on the host");
        let (ok, out) = run(&none, &ws, probe).await;
        assert!(ok && out == "unreachable\n", "{out:?}");
        drop(listener);
        // Exit codes come through pasta and bubblewrap; a timeout kills the whole group.
        let shell = |cmd: &'static str, secs: u64| {
            let (s, ws) = (none.clone(), ws.clone());
            async move { crate::tools::run_shell(&ws, cmd, &[], std::time::Duration::from_secs(secs), Some(&s)).await.unwrap() }
        };
        let done = shell("echo out; exit 7", 30).await;
        assert_eq!((done.text.as_str(), done.status.unwrap().unwrap().code()), ("out\n", Some(7)));
        let marker = format!("zen-sandbox-timeout-{}", std::process::id());
        let cmd: &'static str = format!("(exec -a {marker} sleep 300) & exec -a {marker} sleep 301").leak();
        let started = std::time::Instant::now();
        assert!(shell(cmd, 1).await.status.is_none() && started.elapsed().as_secs() < 10);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let left = std::process::Command::new("pgrep").arg("-f").arg(&marker).output().unwrap();
        assert!(left.stdout.is_empty(), "left running: {}", String::from_utf8_lossy(&left.stdout));
    }
}
