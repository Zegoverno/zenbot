//! Running git from the kernel (the updater, the workflow's diff baseline, the repositories the
//! agent keeps: wiki, skills, its own tools): async, with a time limit, and reading no more output
//! than the caller keeps. The one place the kernel runs git.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Kernel Git must not execute user configuration, fsmonitor commands or hooks. Shell Git used
/// for development is separate and keeps its normal configuration inside its execution boundary.
fn command(repo: &Path) -> Command {
    let mut c = Command::new("/usr/bin/git");
    c.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", "/nonexistent")
        .env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null",
               "-c", "core.pager=cat", "-c", "core.attributesFile=/dev/null",
               "-c", "commit.gpgSign=false", "-c", "credential.helper=",
               "-c", "core.sshCommand=/usr/bin/ssh -F /dev/null"])
        .arg("-C").arg(repo);
    c
}

/// Run git in `repo` (60s at most). Returns whether it succeeded and its stdout, of which at most
/// `cap` bytes are read (git is stopped once that much has arrived, and then counts as failed).
/// None when git couldn't run or took too long.
pub async fn output(repo: &Path, args: &[&str], cap: usize) -> Option<(bool, String)> {
    let mut child = command(repo)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let run = async {
        let mut buf = Vec::new();
        stdout.take(cap as u64).read_to_end(&mut buf).await.ok()?;
        let capped = buf.len() >= cap;
        if capped {
            let _ = child.start_kill();
        }
        let status = child.wait().await.ok()?;
        Some((status.success() && !capped, String::from_utf8_lossy(&buf).into_owned()))
    };
    tokio::time::timeout(Duration::from_secs(60), run).await.ok()?
}

/// git's stdout, trimmed, when it succeeded.
pub async fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let (ok, out) = output(repo, args, 16 * 1024 * 1024).await?;
    ok.then(|| out.trim().to_string())
}

/// Run git in `dir` for its effect (60s at most), discarding its output: whether it succeeded.
pub async fn run(dir: &Path, args: &[&str]) -> bool {
    let status = command(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(tokio::time::timeout(Duration::from_secs(60), status).await, Ok(Ok(s)) if s.success())
}

/// Commit everything in `dir` as zenbot, making the folder a git repository first if it isn't one
/// (the wiki, the skills and the agent's tools are each a repository of their own). Nothing to
/// commit is not an error; failures are ignored (the files are written either way).
pub async fn commit_all(dir: &Path, message: &str) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if !dir.join(".git").exists() {
        run(dir, &["init", "-q"]).await;
    }
    run(dir, &["add", "-A"]).await;
    run(dir, &["-c", "user.name=zenbot", "-c", "user.email=zenbot@localhost", "commit", "-q", "-m", message]).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kernel_git_does_not_execute_fsmonitor_or_hooks() {
        let repo = crate::test_util::TestDir::new("git-no-hooks");
        assert!(git(&repo, &["init", "-q"]).await.is_some());
        let marker = repo.join("executed");
        let hook = repo.join("hook.sh");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::copy(&hook, repo.join(".git/hooks/pre-commit")).unwrap();
        // Supply hostile local config through an ordinary external Git invocation.
        assert!(std::process::Command::new("/usr/bin/git").arg("-C").arg(&*repo)
            .args(["config", "core.fsmonitor"]).arg(&hook).status().unwrap().success());
        std::fs::write(repo.join("file.txt"), "work").unwrap();
        commit_all(&repo, "safe").await;
        assert!(!marker.exists(), "kernel Git executed fsmonitor");
    }

    #[tokio::test]
    async fn output_is_capped() {
        let repo = crate::test_util::TestDir::new("git");
        assert!(git(&repo, &["init", "-q"]).await.is_some());
        for i in 0..50 {
            std::fs::write(repo.join(format!("file-{i:02}.txt")), "x").unwrap();
        }
        let all = git(&repo, &["ls-files", "--others"]).await.unwrap();
        assert_eq!(all.lines().count(), 50);
        let (ok, cut) = output(&repo, &["ls-files", "--others"], 40).await.unwrap();
        assert!(!ok && cut.len() == 40 && all.starts_with(&cut), "{ok} {cut:?}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).await, None, "a failed command gives nothing");
    }

    #[tokio::test]
    async fn commit_all_makes_the_repository() {
        let dir = crate::test_util::TestDir::new("git-commit");
        let sub = dir.join("wiki");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a.md"), "a").unwrap();
        commit_all(&sub, "first").await;
        assert_eq!(git(&sub, &["log", "--format=%s"]).await.as_deref(), Some("first"));
        commit_all(&sub, "nothing new").await;
        assert_eq!(git(&sub, &["rev-list", "--count", "HEAD"]).await.as_deref(), Some("1"), "nothing to commit is no commit");
        assert!(!run(&sub, &["no-such-command"]).await);
    }
}
