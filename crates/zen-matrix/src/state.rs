//! The bridge's settings and what it remembers between runs, all under `~/.zenbot/matrix/` (mode
//! 700): the Matrix login, the encrypted key store, the room ↔ session map, how far each room has
//! got, the owner's pinned identity and the job-report cursor. Secrets are written with mode 600 and never logged.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use zen_client::{parse_env, zen_home};

use crate::kernel::valid_id;

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
        check_ids(&user, &owner)?;
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

/// Both ids are full Matrix ids, and different: a bot that is its own owner would answer itself.
pub fn check_ids(user: &str, owner: &str) -> Result<()> {
    for (k, v) in [("MATRIX_USER", user), ("MATRIX_OWNER", owner)] {
        if !is_user_id(v) {
            bail!("{k} must be a full Matrix ID like @name:server.org, not `{v}`");
        }
    }
    if user.eq_ignore_ascii_case(owner) {
        bail!("MATRIX_OWNER is the bot's own account ({user}): it must be yours, a different account");
    }
    Ok(())
}

pub fn is_user_id(s: &str) -> bool {
    s.starts_with('@') && s.split_once(':').is_some_and(|(l, srv)| l.len() > 1 && !srv.is_empty()) && !s.contains(char::is_whitespace)
}

/// Rooms and sessions, how far each room and job reports have gone, and the owner's pinned identity.
#[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
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
    /// Room id → the `seq` of the last session message the room has had: after a reconnect, a
    /// resync or a restart the bridge posts what came after it.
    pub seen: BTreeMap<String, i64>,
    /// The owner's cross-signing master key (base64), pinned when the first message was accepted.
    /// A different key is refused until `zen-matrix repin-owner` clears it.
    pub owner_key: Option<String>,
}

impl State {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut s: State = match std::fs::read_to_string(dir.join("state.json")) {
            Ok(t) => serde_json::from_str(&t).context("reading ~/.zenbot/matrix/state.json")?,
            Err(_) => State::default(),
        };
        for bad in s.drop_bad_ids() {
            eprintln!("state.json: ignoring {bad}");
        }
        Ok(s)
    }

    /// Drops ids that aren't well-formed (a hand-edited or damaged file), so nothing later slices
    /// or routes one; returns what was dropped.
    fn drop_bad_ids(&mut self) -> Vec<String> {
        let mut bad = vec![];
        for r in [&mut self.main_room, &mut self.jobs_room] {
            if r.as_deref().is_some_and(|id| !is_room_id(id)) {
                bad.push(format!("room {:?}", r.take().unwrap_or_default()));
            }
        }
        self.rooms.retain(|room, session| {
            let ok = is_room_id(room) && valid_id(session);
            if !ok {
                bad.push(format!("room {room:?} → session {session:?}"));
            }
            ok
        });
        self.seen.retain(|room, _| is_room_id(room));
        bad
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
            let done = !matches!(r["status"].as_str(), Some("running") | None) && !r["pending_to"].is_string();
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

/// A Matrix room id (`!opaque:server`, or `!opaque` in room version 12).
pub fn is_room_id(s: &str) -> bool {
    matrix_sdk::ruma::RoomId::parse(s).is_ok()
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
    fn user_ids() {
        assert!(is_user_id("@ze:beeper.com"));
        assert!(!is_user_id("ze:beeper.com"));
        assert!(!is_user_id("@ze"));
        assert!(!is_user_id("@:beeper.com"));
        assert!(check_ids("@zen-bot:matrix.org", "@ze:beeper.com").is_ok());
        assert!(check_ids("@zen-bot:matrix.org", "@Zen-Bot:matrix.org").is_err(), "the bot can't be its own owner");
        assert!(check_ids("zen-bot", "@ze:beeper.com").is_err());
    }

    #[test]
    fn bad_ids_in_state_are_dropped_before_use() {
        let mut s: State = serde_json::from_value(json!({
            "main_room": "not a room",
            "jobs_room": "!jobs:matrix.org",
            "rooms": { "!a:matrix.org": "0f9e3c1a-7b2d-4c1e-9a8b-1234567890ab", "!b:matrix.org": "é-not-a-session", "lobby": "0f9e3c1a" },
        }))
        .unwrap();
        assert_eq!(s.drop_bad_ids().len(), 3);
        assert_eq!(s.main_room, None);
        assert_eq!(s.jobs_room.as_deref(), Some("!jobs:matrix.org"));
        assert_eq!(s.rooms.keys().collect::<Vec<_>>(), vec!["!a:matrix.org"]);
        assert!(s.seen.is_empty() && s.owner_key.is_none(), "fields a v1 state.json lacks default");
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
    fn queued_reports_wait_until_delivered_or_fallback() {
        let mut s = State { jobs_floor: Some(0), ..State::default() };
        assert!(s.new_runs(&[json!({ "id": 1, "status": "ok", "pending_to": "target" })]).is_empty());
        assert!(!s.jobs_reported.contains(&1), "pending is not consumed");
        assert_eq!(s.new_runs(&[json!({ "id": 1, "status": "ok", "pending_to": null })]).len(), 1, "fallback is still reported");
        assert!(s.new_runs(&[json!({ "id": 1, "status": "ok" })]).is_empty(), "reported once");
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
