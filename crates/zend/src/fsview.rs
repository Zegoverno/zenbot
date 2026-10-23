//! What a shell may see of the filesystem (D-064, D-062 step 3).
//!
//! Every shell the kernel runs for a session (`bash`, `verify` checks, a verifier's shell) sees the
//! real machine, with each file whose current bytes came from a denied (contaminated) or level-5
//! session masked: bubblewrap `--ro-bind /dev/null <path>`, as Anthropic's sandbox-runtime hides
//! deny-read paths (the bind is nodev, so opening it fails with "Permission denied"). Masks match
//! by content digest, so a copy is masked like its original; they go on canonical paths, so a
//! symlink leads into the mask; other hard links to a masked inode in the workspace are masked too,
//! and a shell refuses to run when some can't be found. A product the kernel couldn't read is
//! rated for any content its path holds (`ANY`; a directory is masked with an empty tmpfs). With
//! no masks a shell runs exactly as before, at no cost. With masks, a host shell gets its own pid
//! namespace (no `/proc/<pid>/root` way around them; background processes end with it),
//! no_new_privs (no sudo) and no Unix sockets (tools.rs `unix_filter`: no host daemon, tmux or
//! container socket runs a command outside the masks).
//!
//! `prepare` runs before the gate and finds the masks and the rated files the shell will see in the
//! workspace. A `bash` result takes the rating of those whose text it shows (`exposure`: a line of
//! 20 characters or more of the file appears in the output), and records their writers as the
//! session's inputs, so it is wrapped like a `read` and a later deny of a writer reaches the
//! session. Risky sessions can't write scripts or build files without the owner (gate.rs), so a
//! rated file is data a command shows, not code it runs. `verify` rates the whole view up front
//! (`rate_all`), before its gate. `finish` runs after: what a host-capable shell wrote in the
//! workspace (entries whose ctime, which a command can't set back, is after the start) is
//! registered at the session's risk, so a later deny masks it; a scan it can't complete, or a
//! name that isn't UTF-8, withholds the result, as does a deny while the command ran.
//!
//! Limits: products outside the workspace, in build and cache folders (`target`, `node_modules`,
//! `.cache`, `.cargo`, `.rustup`, `.git/objects`), and anything done outside these shells
//! (root-equivalent tools, processes outside zenbot) are out of reach.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use sqlx::{SqlitePool, Row};
use uuid::Uuid;

const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_WALK: usize = 1_000_000;
const SKIP: [&str; 5] = ["target", "node_modules", ".cache", ".cargo", ".rustup"];
/// The digest of a product the kernel couldn't read: its rating applies to whatever is there.
const ANY: &str = "*";

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

type Digested = Arc<(String, Vec<String>)>;
type Version<'a> = (&'a str, Option<Uuid>, i16);

/// A file's digest and its lines of 20 characters or more (at most 200, each cut to its first 200
/// characters: a shown line contains them), cached by inode, size and change time (any write
/// changes ctime).
fn file_digest(path: &Path, meta: &std::fs::Metadata) -> std::io::Result<Digested> {
    type Key = (PathBuf, u64, u64, u64, i64, i64);
    static CACHE: Mutex<Option<HashMap<Key, Digested>>> = Mutex::new(None);
    let key = (path.to_path_buf(), meta.dev(), meta.ino(), meta.len(), meta.ctime(), meta.ctime_nsec());
    if let Some(d) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&key) {
        return Ok(d.clone());
    }
    let bytes = std::fs::read(path)?;
    let lines = String::from_utf8_lossy(&bytes).lines().map(str::trim)
        .filter(|l| l.chars().count() >= 20).take(200).map(|l| l.chars().take(200).collect()).collect();
    let d = Arc::new((digest(&bytes), lines));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() > 50_000 { cache.clear(); }
    cache.insert(key, d.clone());
    Ok(d)
}

/// Regular files and unreadable directories (`true`) under `root`, outside build and cache
/// folders, with their metadata; symlinks aren't followed. False when it stopped at the limit.
fn walk(root: &Path, mut keep: impl FnMut(&Path, &std::fs::Metadata, bool)) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(root) else { return false };
    let mut stack = vec![(root.to_path_buf(), meta)];
    let mut seen = 0;
    while let Some((dir, meta)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { keep(&dir, &meta, true); continue };
        for e in entries {
            seen += 1;
            if seen > MAX_WALK { return false; }
            let Ok(e) = e else { keep(&dir, &meta, true); continue };
            // An entry that lists but can't be looked at (a directory without search permission).
            let (path, Ok(m)) = (e.path(), e.metadata()) else { keep(&dir, &meta, true); continue };
            let name = e.file_name();
            if m.is_dir() {
                let objects = name == "objects" && dir.file_name().is_some_and(|n| n == ".git");
                if !objects && !SKIP.iter().any(|s| name == *s) { stack.push((path, m)); }
            } else if m.is_file() {
                keep(&path, &m, false);
            }
        }
    }
    true
}

/// A rated file in the workspace as the shell sees it: its rating, writers and longer lines.
pub struct Seen { risk: i16, writers: Vec<Uuid>, lines: Vec<String> }

/// What a shell in a workspace will see: files to mask, and the rated files it can show.
#[derive(Default)]
pub struct View { pub masks: Vec<PathBuf>, seen: Vec<Seen> }

impl View {
    /// The rating and writers of the rated files whose text `output` shows.
    pub fn exposure(&self, output: &str) -> (i16, Vec<Uuid>) {
        fold(self.seen.iter().filter(|f| f.lines.iter().any(|l| output.contains(l.as_str()))))
    }

    /// Raise the session to the whole view's rating and record every writer as its input.
    pub async fn rate_all(&self, db: &SqlitePool, session: Uuid) -> Result<()> {
        let (risk, writers) = fold(self.seen.iter());
        record(db, session, risk, &writers).await
    }
}

fn fold<'a>(files: impl Iterator<Item = &'a Seen>) -> (i16, Vec<Uuid>) {
    let (mut risk, mut writers) = (1i16, Vec::new());
    for f in files {
        risk = risk.max(f.risk);
        writers.extend(&f.writers);
    }
    writers.sort();
    writers.dedup();
    (risk, writers)
}

pub async fn record(db: &SqlitePool, session: Uuid, risk: i16, writers: &[Uuid]) -> Result<()> {
    crate::sources::depend(db, session, writers).await?;
    crate::sources::raise(db, session, risk, "shell output").await
}

/// Find the masks and rated files of the view a shell in `workspace` will have. Errors mean the
/// shell must not run (fail closed).
pub async fn prepare(db: &SqlitePool, workspace: &Path) -> Result<View> {
    let ws = workspace.canonicalize().context("workspace")?;
    let prefix = format!("{}/", ws.to_string_lossy().trim_end_matches('/'));
    // Sessions a deny reaches: the contaminated ones and, through parents and inputs, their
    // readers. Then the ratings in the workspace, the contaminated ones, and every copy of those.
    let rows = sqlx::query(
        "WITH RECURSIVE edges AS (SELECT id AS reader, parent AS writer FROM sessions WHERE parent IS NOT NULL
             UNION ALL SELECT reader, writer FROM session_inputs),
           bad(id) AS (SELECT affected FROM contamination WHERE cleared_at IS NULL
             UNION SELECT e.reader FROM edges e JOIN bad b ON e.writer = b.id),
           rated AS (SELECT r.path, r.sha256, r.session_id, CASE WHEN b.id IS NOT NULL THEN 5 ELSE r.risk END AS risk
             FROM item_ratings r LEFT JOIN bad b ON b.id = r.session_id)
         SELECT * FROM rated WHERE substr(path,1,length($1))=$1 OR risk = 5
           OR sha256 IN (SELECT sha256 FROM rated WHERE risk = 5 AND sha256 != '*')")
        .bind(&prefix).fetch_all(db).await?;
    let rows: Vec<Row3> = rows.iter().map(|r| (r.get("path"), r.get("sha256"), r.get("session_id"), r.get("risk"))).collect();
    tokio::task::spawn_blocking(move || view(&ws, rows)).await?
}

type Row3 = (String, String, Option<Uuid>, i16);

fn view(ws: &Path, rows: Vec<Row3>) -> Result<View> {
    let mut by_digest: HashMap<&str, Vec<(Option<Uuid>, i16)>> = HashMap::new();
    let mut by_path: HashMap<&str, Vec<Version>> = HashMap::new();
    for (path, sha, writer, risk) in &rows {
        by_digest.entry(sha).or_default().push((*writer, *risk));
        by_path.entry(path).or_default().push((sha, *writer, *risk));
    }
    let (mut masks, mut seen, mut linked) = (Vec::new(), Vec::new(), HashMap::new());
    for (path, versions) in by_path {
        let p = PathBuf::from(path);
        let any_bad = versions.iter().any(|(_, _, r)| *r == 5);
        let meta = match std::fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            // Can't look: masked if any version was contaminated (an inaccessible mask fails the shell).
            Err(_) => { if any_bad { masks.push(p); } continue }
        };
        if versions.iter().any(|(sha, _, r)| *sha == ANY && *r == 5) {
            masks.push(p);
            continue;
        }
        if !meta.is_file() { continue; }
        let current = if meta.len() > MAX_FILE { None } else { file_digest(&p, &meta).ok() };
        let Some(current) = current else {
            if any_bad { masks.push(p); }
            continue;
        };
        let (d, lines) = &*current;
        // Every rating of these bytes, wherever they were registered, and any of this path's own.
        let any = versions.iter().filter(|(sha, _, _)| *sha == ANY).map(|(_, w, r)| (*w, *r));
        let matched: Vec<_> = by_digest.get(d.as_str()).into_iter().flatten().copied().chain(any).collect();
        if matched.iter().any(|(_, r)| *r == 5) {
            if meta.nlink() > 1 { linked.insert((meta.dev(), meta.ino()), meta.nlink()); }
            masks.push(p);
        } else if !matched.is_empty() && p.starts_with(ws) {
            seen.push(Seen {
                risk: matched.iter().map(|m| m.1).max().unwrap_or(1),
                writers: matched.iter().filter_map(|m| m.0).collect(),
                lines: lines.clone(),
            });
        }
    }
    if !linked.is_empty() {
        let mut found: HashMap<(u64, u64), u64> = HashMap::new();
        let mut extra = Vec::new();
        let complete = walk(ws, |p, m, _| if linked.contains_key(&(m.dev(), m.ino())) {
            *found.entry((m.dev(), m.ino())).or_default() += 1;
            if !masks.iter().any(|x| x == p) { extra.push(p.to_path_buf()); }
        });
        anyhow::ensure!(complete && linked.iter().all(|(k, n)| found.get(k) == Some(n)),
            "a contaminated file has hard links outside the workspace; the owner must remove them first");
        masks.extend(extra);
    }
    Ok(View { masks, seen })
}

/// After a shell: register what a host-capable one (`host`) wrote in `workspace` since `start`,
/// then refuse the result if that can't be done or the session was stopped meanwhile.
pub async fn finish(db: &SqlitePool, session: Uuid, workspace: &Path, start: SystemTime, host: bool) -> Result<()> {
    if host {
        let ws = workspace.canonicalize().context("workspace")?;
        let since = start.duration_since(SystemTime::UNIX_EPOCH)?.as_secs() as i64 - 1;
        let products = tokio::task::spawn_blocking(move || -> Result<Vec<(String, String)>> {
            let (mut out, mut odd) = (Vec::new(), None);
            let complete = walk(&ws, |p, m, unreadable| if m.ctime() >= since {
                let d = if unreadable || m.len() > MAX_FILE { ANY.to_string() }
                    else { file_digest(p, m).map(|d| d.0.clone()).unwrap_or_else(|_| ANY.to_string()) };
                match p.to_str() {
                    Some(name) => out.push((name.to_string(), d)),
                    None => odd = Some(p.to_path_buf()),
                }
            });
            anyhow::ensure!(complete, "withheld: the command's products couldn't all be checked (more than {MAX_WALK} entries in {})", ws.display());
            if let Some(p) = odd { anyhow::bail!("withheld: the command wrote {}, whose name isn't UTF-8; rename it", p.display()); }
            out.sort();
            out.dedup();
            Ok(out)
        }).await??;
        if !products.is_empty() {
            let (paths, digests): (Vec<String>, Vec<String>) = products.into_iter().unzip();
            sqlx::query("INSERT INTO item_ratings(path,sha256,session_id,risk)
                SELECT p.value, d.value, s.id, CASE WHEN s.risk>=4 THEN s.risk ELSE 1 END FROM json_each($1) p JOIN json_each($2) d ON p.key=d.key JOIN sessions s ON s.id=$3
                ON CONFLICT(path,sha256,session_id) DO UPDATE SET risk=MAX(item_ratings.risk,EXCLUDED.risk)")
                .bind(serde_json::json!(paths)).bind(serde_json::json!(digests)).bind(session).execute(db).await?;
        }
    }
    anyhow::ensure!(!crate::taint::stopped(db, session).await?, "withheld: the session was stopped by contamination while the command ran; owner review required");
    Ok(())
}

/// bubblewrap arguments that mask `masks` (a directory with an empty tmpfs); they go after
/// every other mount.
pub fn mask(c: &mut tokio::process::Command, masks: &[PathBuf]) {
    for m in masks {
        if std::fs::symlink_metadata(m).is_ok_and(|m| m.is_dir()) {
            c.arg("--tmpfs").arg(m);
        } else {
            c.arg("--ro-bind").arg("/dev/null").arg(m);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_postgres_sha256_hex() {
        assert_eq!(digest(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn walk_skips_build_folders_and_finds_files() {
        let d = crate::test_util::TestDir::new("fsview-walk");
        std::fs::create_dir_all(d.join("target/x")).unwrap();
        std::fs::create_dir_all(d.join("src/.git/objects")).unwrap();
        std::fs::write(d.join("target/x/a"), "a").unwrap();
        std::fs::write(d.join("src/.git/objects/b"), "b").unwrap();
        std::fs::write(d.join("src/c"), "c").unwrap();
        let mut seen = Vec::new();
        assert!(walk(&d, |p, _, _| seen.push(p.to_path_buf())));
        assert_eq!(seen, vec![d.join("src/c")]);
    }

    #[test]
    fn hard_links_and_contaminated_versions_are_masked() {
        let d = crate::test_util::TestDir::new("fsview-view");
        let ws = d.canonicalize().unwrap();
        std::fs::write(ws.join("bad"), "evil").unwrap();
        std::fs::hard_link(ws.join("bad"), ws.join("alias")).unwrap();
        std::fs::write(ws.join("ok"), "short\n  a line long enough to be recognised  \n").unwrap();
        std::fs::write(ws.join("edited"), "new bytes").unwrap();
        let w = Uuid::new_v4();
        std::fs::write(ws.join("copy"), "evil").unwrap();
        let key = |n: &str| ws.join(n).to_string_lossy().into_owned();
        let mut rated: Vec<Row3> = vec![(key("bad"), digest(b"evil"), Some(w), 5), (key("copy"), digest(b"evil"), None, 1)];
        rated.push((key("ok"), digest(b"short\n  a line long enough to be recognised  \n"), Some(w), 3));
        rated.push((key("edited"), digest(b"old bytes"), Some(w), 5));
        let v = view(&ws, rated).unwrap();
        assert!(v.masks.contains(&ws.join("bad")) && v.masks.contains(&ws.join("alias")));
        assert!(v.masks.contains(&ws.join("copy")), "a copy is masked by its bytes");
        assert!(!v.masks.contains(&ws.join("edited")), "only the contaminated bytes are masked");
        assert_eq!(v.exposure("short, nothing more"), (1, vec![]));
        assert_eq!(v.exposure("$ cat ok\na line long enough to be recognised\n"), (3, vec![w]));
    }
}
