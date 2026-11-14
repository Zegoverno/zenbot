//! A job's `report_file`: checked when it's set (inside the workspace, not a secret file) and read,
//! failing closed, when a run ends (`read_report`); a sandboxed agent's job only inside its sandbox.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::Outcome;
use crate::tools;

/// A run's report from its `report_file`, read as the run left it: inside the workspace (and, for a
/// sandboxed agent's job, inside its `sandbox`), written during the run; anything wrong is the
/// run's error.
pub(super) async fn file_report(workspace: PathBuf, file: &str, started: std::time::SystemTime, sandbox: Option<crate::sandbox::Sandbox>) -> Outcome {
    let secrets = crate::secrets::secret_files();
    let path = tools::resolve(&workspace, file);
    let report = match tokio::task::spawn_blocking(move || read_report(&workspace, &path, started, &secrets, sandbox.as_ref())).await {
        Ok(r) => r,
        Err(e) => Err(anyhow::anyhow!("reading the report file: {e}")),
    };
    let report = report.map(|r| r.map(|raw| raw.trim_end().to_string()));
    match report {
        Ok(Some(text)) => Outcome::ok(crate::secrets::mask_off_thread(text).await),
        Ok(None) => Outcome::silent(),
        Err(e) => Outcome::error(format!("{e:#}"), None),
    }
}

/// The largest report file a run may leave.
const REPORT_MAX: u64 = 60 * 1024;

/// `path` with the part of it that exists resolved (symlinks, `..`), and the rest appended: where a
/// file not written yet would really be. None when the part that doesn't exist ends in `..`.
pub(super) fn real_path(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(real) = cur.canonicalize() {
            return Some(rest.iter().rev().fold(real, |p, c| p.join(c)));
        }
        rest.push(cur.file_name()?.to_os_string());
        cur = cur.parent()?.to_path_buf();
    }
}

/// Why `real` (a resolved path) may not be a job's report file, if it may not: it must lie inside
/// the workspace and not be (or be in) a secret file (secrets.rs, the guard a verifier's `read` has).
fn report_place(workspace: &Path, real: &Path, secrets: &[PathBuf]) -> Result<()> {
    let root = workspace.canonicalize().with_context(|| format!("the job's workspace {} can't be read", workspace.display()))?;
    anyhow::ensure!(real.starts_with(&root) && real != root, "a report file lies inside the job's workspace ({}); {} doesn't", root.display(), real.display());
    anyhow::ensure!(!secrets.iter().filter_map(|f| f.canonicalize().ok()).any(|f| real.starts_with(f)), "a secret file can't be a job's report");
    Ok(())
}

/// A report file checked when it's set: where it would really be is inside the job's workspace and
/// isn't a secret file. Returns the absolute path that's stored (it's checked again at each run).
pub(super) fn checked_report_file(workspace: &Path, file: &str) -> Result<String> {
    let path = tools::resolve(workspace, file);
    let real = real_path(&path).with_context(|| format!("the report file {} can't be resolved", path.display()))?;
    report_place(workspace, &real, &crate::secrets::secret_files())?;
    Ok(path.display().to_string())
}

/// The report a run left in its job's report file: None when the file is empty (the run is
/// `silent`). Fails closed, so the run becomes an error with the reason: missing, a symlink or not a
/// regular file, outside the workspace or a secret file (judged on the file actually opened, through
/// `/proc/self/fd`, so a path swapped after a check can't point elsewhere), last modified before the
/// run started, over 60 KB, or not UTF-8. A sandboxed agent's job reads only what its sandbox may.
fn read_report(workspace: &Path, path: &Path, started: std::time::SystemTime, secrets: &[PathBuf], sandbox: Option<&crate::sandbox::Sandbox>) -> Result<Option<String>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let shown = path.display();
    // No symlink as the last part (O_NOFOLLOW), and a FIFO put there can't hold the kernel (O_NONBLOCK).
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow::anyhow!("the run didn't write its report file {shown}"),
        _ if e.raw_os_error() == Some(libc::ELOOP) => anyhow::anyhow!("the report file {shown} is a symlink; it must be a regular file"),
        _ => anyhow::anyhow!("the report file {shown} can't be opened: {e}"),
    })?;
    let meta = file.metadata()?;
    anyhow::ensure!(meta.is_file(), "the report file {shown} isn't a regular file");
    anyhow::ensure!(meta.nlink() == 1, "the report file {shown} is hardlinked; it must have a single link");
    let real = std::fs::read_link(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&file))).context("can't tell where the report file really is")?;
    report_place(workspace, &real, secrets)?;
    if let Some(s) = sandbox {
        s.check(&real, false).map_err(|e| anyhow::anyhow!("the report file can't be read: {e}"))?;
    }
    anyhow::ensure!(meta.modified()? >= started, "the report file {shown} wasn't written by this run (it's older than the run)");
    anyhow::ensure!(meta.len() <= REPORT_MAX, "the report file {shown} has {} bytes; a report is at most 60 KB", meta.len());
    let mut bytes = Vec::new();
    (&file).take(REPORT_MAX + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    anyhow::ensure!(
        (meta.len(), meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec(), meta.nlink())
            == (after.len(), after.mtime(), after.mtime_nsec(), after.ctime(), after.ctime_nsec(), after.nlink()),
        "the report file {shown} changed while it was read"
    );
    anyhow::ensure!(bytes.len() as u64 <= REPORT_MAX, "the report file {shown} grew past 60 KB while it was read");
    let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("the report file {shown} isn't UTF-8 text"))?;
    // Exact bytes, as read: their rating is looked up by digest before trimming.
    Ok((!text.trim().is_empty()).then_some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_file_is_read_only_when_it_is_safe_and_fresh() {
        let dir = crate::test_util::TestDir::new("report-file");
        let ws = dir.join("ws");
        std::fs::create_dir_all(ws.join("out")).unwrap();
        let secret = ws.join("token");
        std::fs::write(&secret, "s3cret").unwrap();
        std::fs::write(dir.join("outside.md"), "elsewhere").unwrap();
        let secrets = vec![secret.clone()];
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::write(ws.join("out/stale.md"), "yesterday").unwrap();
        std::fs::File::options().write(true).open(ws.join("out/stale.md")).unwrap().set_modified(old).unwrap();
        let started = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        let read = |p: &str| read_report(&ws, &ws.join(p), started, &secrets, None);
        std::fs::write(ws.join("out/message.md"), "Leitura I\n\nPalavra do Senhor.\n\n").unwrap();
        assert_eq!(read("out/message.md").unwrap().as_deref(), Some("Leitura I\n\nPalavra do Senhor.\n\n"), "the exact bytes (trimmed after rating)");
        std::fs::hard_link(ws.join("out/message.md"), ws.join("out/hardlink.md")).unwrap();
        assert!(read("out/hardlink.md").unwrap_err().to_string().contains("hardlinked"));
        std::fs::remove_file(ws.join("out/hardlink.md")).unwrap();
        std::fs::write(ws.join("out/empty.md"), " \n").unwrap();
        assert_eq!(read("out/empty.md").unwrap(), None, "empty means silent");
        let err = |p: &str| format!("{:#}", read(p).unwrap_err());
        assert!(err("out/missing.md").contains("didn't write"));
        assert!(err("out/stale.md").contains("older than the run"));
        std::os::unix::fs::symlink(dir.join("outside.md"), ws.join("out/link.md")).unwrap();
        assert!(err("out/link.md").contains("symlink"));
        std::os::unix::fs::symlink(&*dir, ws.join("up")).unwrap();
        assert!(err("up/outside.md").contains("inside the job's workspace"), "a folder symlink out is judged by where the file is");
        assert!(err("token").contains("secret"));
        std::fs::write(ws.join("out/big.md"), "x".repeat(REPORT_MAX as usize + 1)).unwrap();
        assert!(err("out/big.md").contains("at most 60 KB"));
        std::fs::write(ws.join("out/bin.md"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(err("out/bin.md").contains("UTF-8"));
        assert!(err("out").contains("regular file"));
    }

    #[test]
    fn a_sandboxed_agents_job_reads_its_report_only_inside_its_sandbox() {
        use crate::sandbox::{Access, Mount, Sandbox};
        let dir = crate::test_util::TestDir::new("report-sandbox");
        let ws = dir.join("ws");
        std::fs::create_dir_all(ws.join("mine")).unwrap();
        std::fs::create_dir_all(ws.join("hidden")).unwrap();
        std::fs::write(ws.join("mine/r.md"), "ok").unwrap();
        std::fs::write(ws.join("hidden/r.md"), "not yours").unwrap();
        let s = Sandbox::new("garry", vec![Mount::new(ws.join("mine"), Access::Write)]);
        let started = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        let read = |p: &str| read_report(&ws, &ws.join(p), started, &[], Some(&s));
        assert_eq!(read("mine/r.md").unwrap().as_deref(), Some("ok"));
        let e = format!("{:#}", read("hidden/r.md").unwrap_err());
        assert!(e.contains("outside garry's sandbox"), "{e}");
        std::os::unix::fs::symlink(ws.join("hidden"), ws.join("mine/up")).unwrap();
        assert!(format!("{:#}", read("mine/up/r.md").unwrap_err()).contains("outside garry's sandbox"), "judged by the opened file");
    }

    #[test]
    fn a_report_file_is_set_only_inside_the_workspace() {
        let dir = crate::test_util::TestDir::new("report-set");
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let stored = checked_report_file(&ws, "out/message.md").unwrap();
        assert_eq!(stored, ws.join("out/message.md").display().to_string(), "stored absolute, before it exists");
        assert!(checked_report_file(&ws, "../elsewhere.md").is_err());
        assert!(checked_report_file(&ws, "/tmp/message.md").is_err());
        assert!(checked_report_file(&ws, "out/../../x.md").is_err());
        assert!(checked_report_file(&ws, ".").is_err(), "not the workspace itself");
        std::os::unix::fs::symlink("/tmp", ws.join("tmp")).unwrap();
        assert!(checked_report_file(&ws, "tmp/message.md").is_err(), "a symlink out of the workspace is outside");
    }
}
