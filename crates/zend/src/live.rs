//! The live change stream: one kernel-wide channel of small invalidation events, so the owner's
//! clients refetch what changed instead of polling. `GET /api/events` streams it as Server-Sent
//! Events: one `data: {"topic": "...", "id"?: "..."}` per change, and a `hello` first.
//!
//! An event says only *what kind* of thing changed; clients read the thing itself through the usual
//! routes. Topics: `board` (sessions: created, busy, idle, waiting, renamed, archived; their
//! children), `agents` (an agent's files: SOUL, IDENTITY, roles.toml, agent.toml; `id` is its
//! name), `models` (engines and the model list), `prefs` (preferences, USER.md), `jobs` (jobs and
//! their runs), `tasks` (vault tasks and their agents), `plugins`, `memory` (memories, records,
//! MEMORY.md), `skills`, `mcp`, `version` (installed version, upgrades).
//!
//! Where they come from, at a few choke points rather than at every write:
//! - `after_write`: middleware on the API; a successful write (any method but GET) touches the
//!   topics of its route (`topics_for`).
//! - `App::emit` (main.rs): a session event that changes the board (busy, end, title, a question…).
//! - `after_tool` (dispatch.rs): a tool that succeeded touches its topic (`remember` → memory,
//!   `schedule` → jobs…) and wakes the scan for tools that may have written a file.
//! - `scan_loop`: every `SCAN` (and when woken), the mtimes of the files under the zen home that
//!   clients show (`watched`) and cheap fingerprints of the tables they read (`DB_PRINTS`, plus
//!   the busy sessions): what changed out of band (an editor, Obsidian, a job, another kernel path)
//!   is caught here. A plain mtime scan: a few hundred stats, no watcher dependency, nothing missed
//!   when a watch would overflow.
//!
//! `touch` only records a topic; a flusher sends each pending one once per `COALESCE`, so a burst
//! (a write that touches many files, a streaming turn) is one event per topic.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use serde_json::json;
use tokio::sync::{Notify, broadcast};

use crate::AppState;

const COALESCE: Duration = Duration::from_millis(200);
const SCAN: Duration = Duration::from_secs(2);

struct Hub {
    tx: broadcast::Sender<String>,
    pending: Mutex<BTreeSet<(String, Option<String>)>>,
    flush: Notify,
    scan: Notify,
}

fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub { tx: broadcast::channel(256).0, pending: Mutex::default(), flush: Notify::new(), scan: Notify::new() })
}

/// Record that something under `topic` changed; sent within `COALESCE`, once per burst.
pub(crate) fn touch(topic: &str) {
    touch_id(topic, None);
}

pub(crate) fn touch_id(topic: &str, id: Option<&str>) {
    let h = hub();
    h.pending.lock().unwrap_or_else(|e| e.into_inner()).insert((topic.to_string(), id.map(str::to_string)));
    h.flush.notify_one();
}

/// Run the next scan now (a tool may have written a watched file).
pub(crate) fn scan_soon() {
    hub().scan.notify_one();
}

pub(crate) fn subscribe() -> broadcast::Receiver<String> {
    hub().tx.subscribe()
}

/// Send what `touch` recorded, at most once per `COALESCE` per topic.
async fn flush_loop() {
    let h = hub();
    loop {
        h.flush.notified().await;
        tokio::time::sleep(COALESCE).await;
        let batch = std::mem::take(&mut *h.pending.lock().unwrap_or_else(|e| e.into_inner()));
        for (topic, id) in batch {
            let ev = match id {
                Some(id) => json!({ "topic": topic, "id": id }),
                None => json!({ "topic": topic }),
            };
            let _ = h.tx.send(ev.to_string()); // no listener: nothing to do
        }
    }
}

/// The topics a successful API write changes, by its route (after `/api/`).
pub(crate) fn topics_for(method: &Method, path: &str) -> &'static [&'static str] {
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        return &[];
    }
    let path = path.trim_start_matches('/');
    let first = path.split('/').next().unwrap_or("");
    match first {
        "sessions" | "board" => &["board"],
        "tasks" if path.ends_with("/agent") => &["tasks", "board"],
        "tasks" => &["tasks"],
        "agents" => &["agents", "board"],
        "prompt-files" => &["agents", "prefs"],
        "jobs" => &["jobs", "board"],
        "plugins" => &["plugins", "jobs", "skills"],
        "prefs" => &["prefs", "models"],
        "engines" => &["models"],
        "memory" | "brain" | "shared-brain" => &["memory"],
        "mcp" => &["mcp"],
        "upgrade" => &["version"],
        _ => &[],
    }
}

/// Middleware: a write that succeeded touches its route's topics.
pub(crate) async fn after_write(req: Request, next: Next) -> Response {
    let topics = topics_for(req.method(), req.uri().path());
    let res = next.run(req).await;
    if res.status().is_success() {
        topics.iter().for_each(|t| touch(t));
    }
    res
}

/// A tool call that succeeded: its topic now, and a scan soon for tools that may write files.
pub(crate) fn after_tool(name: &str) {
    match name {
        "remember" | "brain" => touch("memory"),
        "save_skill" => touch("skills"),
        "schedule" => touch("jobs"),
        "delegate" | "verify" | "ask" => touch("board"),
        "write" | "edit" | "bash" | "call_tool" => scan_soon(),
        _ => {}
    }
}

/// A session event that changes what the board shows (not the streaming ones).
pub(crate) fn session_event(kind: Option<&str>) {
    if matches!(kind, Some("busy" | "end" | "title" | "questions" | "child_end" | "model" | "error" | "message")) {
        touch("board");
    }
}

/// `GET /api/events`: the change stream as Server-Sent Events, with a keep-alive comment every 15 s.
pub(crate) async fn events(State(_app): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = subscribe();
    let hello = futures_util::stream::once(async { Ok(Event::default().data(json!({ "topic": "hello" }).to_string())) });
    let rest = futures_util::stream::unfold(rx, |mut rx| async move {
        let data = match rx.recv().await {
            Ok(data) => data,
            // Fell behind: the client can't know what it missed, so it refetches everything.
            Err(broadcast::error::RecvError::Lagged(_)) => json!({ "topic": "resync" }).to_string(),
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        Some((Ok(Event::default().data(data)), rx))
    });
    Sse::new(futures_util::StreamExt::chain(hello, rest)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

// ---- the scan ----

/// The files under `home` clients show, by topic: (topic, path, recurse). An agent's folder adds
/// its own files with the agent's name as the event's id.
fn watched(home: &Path, skills: &Path) -> Vec<(&'static str, Option<String>, PathBuf, bool)> {
    let mut out: Vec<(&'static str, Option<String>, PathBuf, bool)> = vec![
        ("agents", None, home.join("agents"), false),
        ("prefs", None, home.join("USER.md"), false),
        ("agents", None, home.join("AGENTS.md"), false),
        ("memory", None, home.join("global/MEMORY.md"), false),
        ("memory", None, home.join("global/memory"), true),
        ("memory", None, home.join("global/entities"), true),
        ("skills", None, skills.to_path_buf(), true),
        ("tasks", None, home.join("global/Projects"), true),
        ("plugins", None, home.join("plugins"), false),
        ("models", None, home.join("engines.json"), false),
        ("mcp", None, home.join("mcp.json"), false),
        ("version", None, home.join("version"), false),
        ("version", None, home.join("web.version"), false),
        ("version", None, home.join("upgrade.log"), false),
    ];
    if let Ok(rd) = std::fs::read_dir(home.join("agents")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !e.path().is_dir() {
                continue;
            }
            for f in ["SOUL.md", "IDENTITY.md", "roles.toml", "agent.toml", "mcp.json"] {
                out.push(("agents", Some(name.clone()), e.path().join(f), false));
            }
            out.push(("skills", None, e.path().join("skills"), true));
        }
    }
    if let Ok(rd) = std::fs::read_dir(home.join("plugins")) {
        for e in rd.flatten() {
            out.push(("plugins", None, e.path(), false));
        }
    }
    out
}

/// A path's (mtime in ns, size), folded into `acc`; with `recurse`, every file and folder under it
/// (not dot folders, at most a few levels down, symlinks not followed).
fn stamp(path: &Path, recurse: bool, depth: usize, acc: &mut u64) {
    let Ok(m) = std::fs::symlink_metadata(path) else { return };
    let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0);
    *acc = acc.wrapping_mul(1_000_003).wrapping_add(t ^ m.len()).wrapping_add(path.as_os_str().len() as u64);
    if recurse && depth < 6 && m.is_dir() {
        let Ok(rd) = std::fs::read_dir(path) else { return };
        let mut entries: Vec<_> = rd.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.')).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            stamp(&p, true, depth + 1, acc);
        }
    }
}

/// One fingerprint per (topic, id) of the watched files under `home`.
pub(crate) fn file_prints(home: &Path, skills: &Path) -> BTreeMap<(String, Option<String>), u64> {
    let mut out = BTreeMap::new();
    for (topic, id, path, recurse) in watched(home, skills) {
        let acc = out.entry((topic.to_string(), id)).or_insert(17u64);
        stamp(&path, recurse, 0, acc);
    }
    out
}

/// What each table clients read looks like, cheaply: counts, latest times, a few sums. Catches
/// writes from paths that don't touch a topic themselves (jobs, the sleep, kernel sessions…).
const DB_PRINTS: &[(&str, &str)] = &[
    ("board", "SELECT count(*)||'|'||coalesce(sum(archived),0)||'|'||coalesce(max(updated_at),'')||'|'||coalesce(max(created_at),'')||'|'||total(length(title))||'|'||total(length(model))||'|'||count(outcome) FROM sessions"),
    ("jobs", "SELECT count(*)||'|'||coalesce(max(updated_at),'')||'|'||coalesce(max(last_run_at),'')||'|'||coalesce(max(next_run_at),'')||'|'||coalesce(sum(enabled),0)||'|'||total(failures)||'|'||count(removed_at) FROM jobs"),
    ("jobs", "SELECT coalesce(max(id),0)||'|'||coalesce(sum(status='running'),0)||'|'||coalesce(max(ended_at),'') FROM job_runs"),
    ("memory", "SELECT count(*)||'|'||coalesce(max(id),0)||'|'||coalesce(max(updated_at),'')||'|'||total(length(text)) FROM memories"),
    ("plugins", "SELECT count(*)||'|'||coalesce(max(updated_at),'')||'|'||coalesce(sum(enabled),0)||'|'||coalesce(sum(ai_enabled),0) FROM plugins"),
    ("prefs", "SELECT count(*)||'|'||coalesce(max(updated_at),'') FROM prefs"),
    ("tasks", "SELECT count(*)||'' FROM task_agents"),
];

async fn db_prints(app: &AppState) -> BTreeMap<(String, Option<String>), u64> {
    use std::hash::{Hash, Hasher};
    let mut out = BTreeMap::new();
    for (topic, sql) in DB_PRINTS {
        let v: Option<String> = sqlx::query_scalar(sql).fetch_one(&app.db).await.ok().flatten();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        v.hash(&mut h);
        let acc = out.entry((topic.to_string(), None)).or_insert(17u64);
        *acc = acc.wrapping_mul(1_000_003).wrapping_add(h.finish());
    }
    // Who is busy lives in memory, not in a table.
    let mut busy: Vec<String> = app.turns.lock().await.keys().map(|k| k.to_string()).collect();
    busy.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    busy.hash(&mut h);
    out.insert(("board".to_string(), Some(String::new())), h.finish());
    out
}

/// The (topic, id) whose fingerprint differs between two scans (or appeared, or went).
pub(crate) fn changed(before: &BTreeMap<(String, Option<String>), u64>, now: &BTreeMap<(String, Option<String>), u64>) -> BTreeSet<(String, Option<String>)> {
    let mut out = BTreeSet::new();
    for (k, v) in now {
        if before.get(k) != Some(v) {
            out.insert(k.clone());
        }
    }
    for k in before.keys() {
        if !now.contains_key(k) {
            out.insert(k.clone());
        }
    }
    out
}

/// Start the flusher and the scan.
pub(crate) fn start(app: AppState) {
    tokio::spawn(flush_loop());
    tokio::spawn(scan_loop(app));
}

async fn scan_loop(app: AppState) {
    let home = crate::zen_home();
    let skills = crate::skills::root();
    let scan = |home: PathBuf, skills: PathBuf| async move { tokio::task::spawn_blocking(move || file_prints(&home, &skills)).await.unwrap_or_default() };
    let mut files = scan(home.clone(), skills.clone()).await;
    let mut db = db_prints(&app).await;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(SCAN) => {}
            _ = hub().scan.notified() => tokio::time::sleep(COALESCE).await,
        }
        let f = scan(home.clone(), skills.clone()).await;
        let d = db_prints(&app).await;
        for (topic, id) in changed(&files, &f).into_iter().chain(changed(&db, &d)) {
            // The busy-set fingerprint is keyed with an empty id: it's the board, not one session.
            touch_id(&topic, id.as_deref().filter(|i| !i.is_empty()));
        }
        files = f;
        db = d;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_touch_their_routes_topics() {
        assert_eq!(topics_for(&Method::GET, "/agents"), &[] as &[&str]);
        assert_eq!(topics_for(&Method::PUT, "/agents/zenbot"), &["agents", "board"]);
        assert_eq!(topics_for(&Method::PATCH, "/prefs"), &["prefs", "models"]);
        assert_eq!(topics_for(&Method::POST, "/tasks/abc/agent"), &["tasks", "board"]);
        assert_eq!(topics_for(&Method::PATCH, "/jobs/x"), &["jobs", "board"]);
        assert_eq!(topics_for(&Method::POST, "/complete"), &[] as &[&str]);
    }

    #[tokio::test]
    async fn every_table_print_reads_as_text() {
        // A print that fails (a renamed column, a number where text is read) would never change,
        // so its topic would go quiet: each must read on the real schema.
        let (_dir, db) = crate::test_util::database().await;
        for (topic, sql) in DB_PRINTS {
            let v: Option<String> = sqlx::query_scalar(sql).fetch_one(&db).await.unwrap_or_else(|e| panic!("{topic}: {e}"));
            assert!(v.is_some(), "{topic}: {sql}");
        }
    }

    #[test]
    fn a_changed_roles_file_is_an_agents_event_with_the_agents_name() {
        let dir = crate::test_util::TestDir::new("live-scan");
        let home: &Path = &dir;
        let roles = home.join("agents/zenbot/roles.toml");
        std::fs::create_dir_all(roles.parent().unwrap()).unwrap();
        std::fs::write(&roles, "default_model = \"a\"\n").unwrap();
        std::fs::create_dir_all(home.join("global/skills/work/x")).unwrap();
        let skills = home.join("global/skills");
        let before = file_prints(home, &skills);
        assert!(changed(&before, &file_prints(home, &skills)).is_empty(), "nothing changed, nothing sent");
        // Same size, later mtime: an edit tool's rewrite.
        std::fs::write(&roles, "default_model = \"b\"\n").unwrap();
        let later = std::time::SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options().write(true).open(&roles).unwrap().set_modified(later).unwrap();
        let after = file_prints(home, &skills);
        assert_eq!(changed(&before, &after).into_iter().collect::<Vec<_>>(), vec![("agents".to_string(), Some("zenbot".to_string()))]);
        // A new skill is a skills event.
        std::fs::write(home.join("global/skills/work/x/SKILL.md"), "---\nname: x\n---\n").unwrap();
        let skill = changed(&after, &file_prints(home, &skills));
        assert!(skill.contains(&("skills".to_string(), None)), "{skill:?}");
    }

    #[tokio::test]
    async fn touches_coalesce_into_one_event_per_topic() {
        tokio::spawn(flush_loop());
        let mut rx = subscribe();
        for _ in 0..5 {
            touch("live-test-topic");
        }
        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
        assert_eq!(first, r#"{"topic":"live-test-topic"}"#);
        // Nothing more for that burst (other tests may touch other topics on the shared hub).
        let more = tokio::time::timeout(COALESCE * 3, async {
            loop {
                if rx.recv().await.unwrap() == first {
                    return;
                }
            }
        })
        .await;
        assert!(more.is_err(), "a burst is one event");
    }
}
