//! Self-update: notice when origin/main has moved past the running version, and run
//! scripts/self-update.sh (pull + upgrade.sh) on request. The restart itself is done by
//! upgrade.sh once no session is working, so the kernel only has to start the job.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

pub struct Updater {
    repo: PathBuf,
    last_check: Mutex<Value>,
    job: Arc<Mutex<Option<Job>>>,
}

struct Job {
    started_at: String,
    status: &'static str, // running | scheduled | failed
    log: String,
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

async fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(Duration::from_secs(60), Command::new("git").arg("-C").arg(repo).args(args).stdin(Stdio::null()).output())
        .await
        .ok()?
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Updater {
    pub fn new(repo: PathBuf) -> Self {
        Updater { repo, last_check: Mutex::new(Value::Null), job: Arc::default() }
    }

    /// The installed commit (~/.zenbot/version). Read each time: apply-upgrade.sh writes it only
    /// after the new kernel has passed its health check, so it can change after startup.
    pub fn running(&self) -> String {
        let home = std::env::var("HOME").unwrap_or_default();
        std::fs::read_to_string(format!("{home}/.zenbot/version")).unwrap_or_default().trim().to_string()
    }

    /// The last check's result (without checking again).
    pub async fn info(&self) -> Value {
        let last = self.last_check.lock().await.clone();
        if last.is_null() {
            json!({ "running": self.running(), "checked_at": null })
        } else {
            last
        }
    }

    /// Fetch origin/main and compare it with the running version.
    pub async fn check(&self) -> Value {
        let repo = &self.repo;
        let result = async {
            git(repo, &["fetch", "-q", "origin", "main"]).await.ok_or("could not fetch origin/main (offline?)")?;
            let latest = git(repo, &["rev-parse", "origin/main"]).await.ok_or("no origin/main")?;
            let running = self.running();
            let base = if running.is_empty() { "HEAD".to_string() } else { running };
            let running_full = git(repo, &["rev-parse", &format!("{base}^{{commit}}")]).await.ok_or("running version is not in the repository")?;
            let range = format!("{running_full}..{latest}");
            let behind: u64 = git(repo, &["rev-list", "--count", &range]).await.and_then(|n| n.parse().ok()).unwrap_or(0);
            let commits: Vec<String> = git(repo, &["log", "--format=%h %s", "-n", "15", &range])
                .await
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect();
            let prebuilt = if behind > 0 {
                Command::new(repo.join("scripts/fetch-release.sh"))
                    .args(["--check", &latest])
                    .stdin(Stdio::null())
                    .output()
                    .await
                    .is_ok_and(|o| o.status.success())
            } else {
                false
            };
            Ok::<Value, &str>(json!({
                "running": short(&running_full),
                "latest": short(&latest),
                "behind": behind,
                "available": behind > 0,
                "commits": commits,
                "prebuilt_ready": prebuilt,
            }))
        }
        .await;
        let mut info = match result {
            Ok(v) => v,
            Err(e) => json!({ "running": self.running(), "error": e }),
        };
        info["checked_at"] = json!(now());
        *self.last_check.lock().await = info.clone();
        info
    }

    /// Check now and then every ZEN_UPDATE_CHECK_SECS (default an hour; 0 turns it off).
    pub async fn check_periodically(self: Arc<Self>) {
        let every: u64 = std::env::var("ZEN_UPDATE_CHECK_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(3600);
        if every == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            let info = self.check().await;
            if info["available"] == true {
                tracing::info!("update available: {} -> {} ({} commits)", info["running"], info["latest"], info["behind"]);
            }
            tokio::time::sleep(Duration::from_secs(every)).await;
        }
    }

    /// Start scripts/self-update.sh in the background. Errors if one is already running.
    pub async fn start(&self) -> Result<String, String> {
        let mut slot = self.job.lock().await;
        if slot.as_ref().is_some_and(|j| j.status == "running") {
            return Err("an upgrade is already running".into());
        }
        let mut child = Command::new("bash")
            .arg(self.repo.join("scripts/self-update.sh"))
            .current_dir(&self.repo)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start scripts/self-update.sh: {e}"))?;
        let started_at = now();
        *slot = Some(Job { started_at: started_at.clone(), status: "running", log: String::new() });
        drop(slot);

        let job = self.job.clone();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        tokio::spawn(async move {
            let append = |job: Arc<Mutex<Option<Job>>>, pipe: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>| async move {
                let Some(pipe) = pipe else { return };
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(j) = job.lock().await.as_mut() {
                        j.log.push_str(&line);
                        j.log.push('\n');
                    }
                }
            };
            let out = append(job.clone(), stdout.map(|p| Box::new(p) as _));
            let err = append(job.clone(), stderr.map(|p| Box::new(p) as _));
            let (status, _, _) = tokio::join!(child.wait(), out, err);
            if let Some(j) = job.lock().await.as_mut() {
                j.status = if status.is_ok_and(|s| s.success()) { "scheduled" } else { "failed" };
            }
        });
        Ok(started_at)
    }

    /// The current job (if this kernel started one) and the last line of ~/.zenbot/upgrade.log.
    pub async fn status(&self) -> Value {
        let job = self.job.lock().await.as_ref().map(|j| json!({ "started_at": j.started_at, "status": j.status, "log": j.log }));
        let home = std::env::var("HOME").unwrap_or_default();
        let last = std::fs::read_to_string(format!("{home}/.zenbot/upgrade.log"))
            .unwrap_or_default()
            .lines()
            .rev()
            .find(|l| l.starts_with("20"))
            .unwrap_or("")
            .to_string();
        json!({ "running": self.running(), "job": job, "last_result": last })
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}
