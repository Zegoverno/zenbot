//! Running git from the kernel (the updater, the workflow's diff baseline): async, with a time
//! limit, and reading no more output than the caller keeps.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Run git in `repo` (60s at most). Returns whether it succeeded and its stdout, of which at most
/// `cap` bytes are read (git is stopped once that much has arrived, and then counts as failed).
/// None when git couldn't run or took too long.
pub async fn output(repo: &Path, args: &[&str], cap: usize) -> Option<(bool, String)> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
