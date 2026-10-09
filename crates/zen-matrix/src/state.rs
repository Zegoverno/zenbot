//! The bridge's settings and what it remembers between runs, all under `~/.zenbot/matrix/` (mode
//! 700): the Matrix login, the encrypted key store, the room ↔ session map and the job-report
//! cursor. Secrets are written with mode 600 and never logged.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// zenbot's home: `ZEN_HOME`, else `~/.zenbot`.
pub fn zen_home() -> PathBuf {
    match std::env::var_os("ZEN_HOME").filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".zenbot"),
    }
}

/// `KEY=value` lines (blank lines, `#` comments and `export ` allowed; quotes around the value
/// dropped), as `~/.zenbot/env` and `~/.zenbot/matrix.env` are written.
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))).unwrap_or(v);
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

fn env_file(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path).map(|t| parse_env(&t)).unwrap_or_default()
}

/// What the bridge needs to run: from `~/.zenbot/matrix.env`, the process environment winning.
pub struct Config {
    pub user: String,
    pub password: Option<String>,
    /// The only Matrix account the bridge answers.
    pub owner: String,
    pub homeserver: Option<String>,
    pub recovery_key: Option<String>,
    pub kernel_url: String,
    pub kernel_token: String,
    pub dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = zen_home();
        let file = env_file(&home.join("matrix.env"));
        let get = |k: &str| std::env::var(k).ok().or_else(|| file.get(k).cloned()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let zen_env = env_file(&home.join("env"));
        let user = get("MATRIX_USER").context("MATRIX_USER is not set (in ~/.zenbot/matrix.env): the bot's Matrix ID, like @zen-bot:matrix.org")?;
        let owner = get("MATRIX_OWNER").context("MATRIX_OWNER is not set (in ~/.zenbot/matrix.env): the owner's Matrix ID, the only one the bot answers")?;
        for (k, v) in [("MATRIX_USER", &user), ("MATRIX_OWNER", &owner)] {
            if !is_user_id(v) {
                bail!("{k} must be a full Matrix ID like @name:server.org, not `{v}`");
            }
        }
        let port = std::env::var("ZEN_PORT").ok().or_else(|| zen_env.get("ZEN_PORT").cloned()).unwrap_or_else(|| "8100".into());
        let kernel_url = get("ZEN_URL").unwrap_or_else(|| format!("http://127.0.0.1:{port}"));
        let kernel_token = match get("ZEN_TOKEN") {
            Some(t) => t,
            None => std::fs::read_to_string(home.join("token")).context("no zenbot token (~/.zenbot/token)")?.trim().to_string(),
        };
        Ok(Config {
            user,
            password: get("MATRIX_PASSWORD"),
            owner,
            homeserver: get("MATRIX_HOMESERVER"),
            recovery_key: get("MATRIX_RECOVERY_KEY"),
            kernel_url,
            kernel_token,
            dir: home.join("matrix"),
        })
    }

    /// The server part of the bot's ID, to discover its homeserver from.
    pub fn server_name(&self) -> &str {
        self.user.split_once(':').map(|(_, s)| s).unwrap_or("matrix.org")
    }
}

pub fn is_user_id(s: &str) -> bool {
    s.starts_with('@') && s.split_once(':').is_some_and(|(l, srv)| l.len() > 1 && !srv.is_empty()) && !s.contains(char::is_whitespace)
}

/// Rooms and sessions, and how far job reports have gone.
#[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
pub struct State {
    /// The room that's the owner's main zen chat.
    pub main_room: Option<String>,
    /// The room job reports go to.
    pub jobs_room: Option<String>,
    /// Room id → session id.
    pub rooms: BTreeMap<String, String>,
    /// Job runs up to this id were there before the bridge first started: not reported.
    pub jobs_floor: Option<i64>,
    /// Runs above the floor already reported.
    pub jobs_reported: BTreeSet<i64>,
}

impl State {
    pub fn load(dir: &Path) -> Result<Self> {
        match std::fs::read_to_string(dir.join("state.json")) {
            Ok(t) => serde_json::from_str(&t).context("reading ~/.zenbot/matrix/state.json"),
            Err(_) => Ok(State::default()),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        write_private(&dir.join("state.json"), &serde_json::to_string_pretty(self)?)
    }

    /// Finished runs to report now, oldest first; marks them reported. The first call only sets
    /// the floor, so the history from before the bridge isn't replayed.
    pub fn new_runs<'a>(&mut self, runs: &'a [serde_json::Value]) -> Vec<&'a serde_json::Value> {
        let ids = runs.iter().filter_map(|r| r["id"].as_i64());
        let Some(floor) = self.jobs_floor else {
            self.jobs_floor = Some(ids.max().unwrap_or(0));
            return vec![];
        };
        let mut out = vec![];
        for r in runs.iter().rev() {
            let Some(id) = r["id"].as_i64() else { continue };
            let done = !matches!(r["status"].as_str(), Some("running") | None);
            if id > floor && done && self.jobs_reported.insert(id) {
                out.push(r);
            }
        }
        // Only the ids the kernel still lists can come back: forget the rest.
        if let Some(min) = runs.iter().filter_map(|r| r["id"].as_i64()).min() {
            self.jobs_reported.retain(|&i| i >= min);
        }
        out
    }
}

/// Write a file only its owner can read (create the directory 700 first).
pub fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn env_files_parse() {
        let e = parse_env("# bot\nMATRIX_USER=@zen:matrix.org\nexport MATRIX_PASSWORD=\"p=ss word\"\n\nX='y'\nbad line\n");
        assert_eq!(e["MATRIX_USER"], "@zen:matrix.org");
        assert_eq!(e["MATRIX_PASSWORD"], "p=ss word");
        assert_eq!(e["X"], "y");
        assert_eq!(e.len(), 3);
    }

    #[test]
    fn user_ids() {
        assert!(is_user_id("@ze:beeper.com"));
        assert!(!is_user_id("ze:beeper.com"));
        assert!(!is_user_id("@ze"));
        assert!(!is_user_id("@:beeper.com"));
    }

    #[test]
    fn job_runs_are_reported_once_and_history_is_skipped() {
        let mut s = State::default();
        let first = vec![json!({ "id": 7, "status": "ok" }), json!({ "id": 6, "status": "ok" })];
        assert!(s.new_runs(&first).is_empty(), "the first look sets the floor");
        assert_eq!(s.jobs_floor, Some(7));
        let runs = vec![json!({ "id": 10, "status": "running" }), json!({ "id": 9, "status": "silent" }), json!({ "id": 8, "status": "ok" }), json!({ "id": 7, "status": "ok" })];
        let ids: Vec<i64> = s.new_runs(&runs).iter().map(|r| r["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![8, 9], "oldest first; running ones wait");
        assert!(s.new_runs(&runs).is_empty(), "once");
        let runs = vec![json!({ "id": 10, "status": "ok" }), json!({ "id": 9, "status": "silent" })];
        let ids: Vec<i64> = s.new_runs(&runs).iter().map(|r| r["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![10]);
        assert_eq!(s.jobs_reported, BTreeSet::from([9, 10]), "ids the kernel no longer lists are forgotten");
    }

    #[test]
    fn private_files_are_600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("zen-matrix-test-{}", std::process::id()));
        let p = dir.join("sub/secret.json");
        write_private(&p, "{}").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::remove_dir_all(dir).ok();
    }
}
