//! What a shell may see of the filesystem (D-064, D-062 step 3).
//!
//! Every shell the kernel runs for a session (`bash`, `verify` checks, a verifier's shell) sees the
//! real machine, with each file whose current bytes came from a denied (contaminated) or level-5
//! session masked: bubblewrap `--ro-bind /dev/null <path>`, as Anthropic's sandbox-runtime hides
//! deny-read paths (the bind is nodev, so opening it fails with "Permission denied"). Masks go on canonical paths, so a symlink leads into the
//! mask; other hard links to a masked inode in the workspace are masked too, and a shell refuses
//! to run when some can't be found. With no masks a shell runs exactly as before, at no cost.
//! With masks, a host shell gets its own pid namespace (no `/proc/<pid>/root` way around them;
//! background processes end with it), no_new_privs (no sudo) and the Docker socket hidden.
//!
//! `prepare` runs before the gate and finds the masks and the rated files the shell will see in the
//! workspace. A `bash` result takes the rating of those whose text it shows (`exposure`: a line of
//! 20 characters or more of the file appears in the output), and records their writers as the
//! session's inputs, so it is wrapped like a `read` and a later deny of a writer reaches the
//! session. Risky sessions can't write scripts or build files without the owner (gate.rs), so a
//! rated file is data a command shows, not code it runs. `verify` rates the whole view up front
//! (`rate_all`), before its gate. `finish` runs after: what a host-capable shell
//! wrote in the workspace (files whose ctime, which a command can't set back, is after the start)
//! is registered by its exact bytes at the session's risk, so a later deny hides it; and a result
//! is withheld when the session was stopped while the command ran.
//!
//! Limits: the product scan skips build and cache folders (`target`, `node_modules`, `.cache`,
//! `.cargo`, `.rustup`, `.git/objects`) and files over 16 MiB; root-equivalent tools used outside
//! these shells, and processes outside zenbot, are out of reach.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_WALK: usize = 200_000;
const SKIP: [&str; 5] = ["target", "node_modules", ".cache", ".cargo", ".rustup"];

/// The Docker socket gives root; a shell that must not read a masked file must not reach it.
const ROOT_SOCKETS: [&str; 1] = ["/run/docker.sock"];

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A file's digest, cached by inode, size and change time (a write changes ctime).
fn file_digest(path: &Path, meta: &std::fs::Metadata) -> std::io::Result<String> {
    type Key = (PathBuf, u64, u64, u64, i64, i64);
    static CACHE: Mutex<Option<HashMap<Key, String>>> = Mutex::new(None);
    let key = (path.to_path_buf(), meta.dev(), meta.ino(), meta.len(), meta.ctime(), meta.ctime_nsec());
    if let Some(d) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&key) {
        return Ok(d.clone());
    }
    let d = digest(&std::fs::read(path)?);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() > 50_000 { cache.clear(); }
    cache.insert(key, d.clone());
    Ok(d)
}

/// Regular files under `root`, outside build and cache folders, with their metadata.
fn walk(root: &Path, mut keep: impl FnMut(&Path, &std::fs::Metadata)) -> bool {
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK { return false; }
            let (path, Ok(meta)) = (e.path(), e.metadata()) else { continue };
            let name = e.file_name();
            if meta.is_dir() {
                let objects = name == "objects" && dir.file_name().is_some_and(|n| n == ".git");
                if !objects && !SKIP.iter().any(|s| name == *s) { stack.push(path); }
            } else if meta.is_file() {
                keep(&path, &meta);
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
        let shown = self.seen.iter().filter(|f| f.lines.iter().any(|l| output.contains(l.as_str())));
        fold(shown)
    }

    /// Raise the session to the whole view's rating and record every writer as its input.
    pub async fn rate_all(&self, db: &PgPool, session: Uuid) -> Result<()> {
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

pub async fn record(db: &PgPool, session: Uuid, risk: i16, writers: &[Uuid]) -> Result<()> {
    crate::sources::depend(db, session, writers).await?;
    crate::sources::raise(db, session, risk).await
}

/// Find the masks and rated files of the view a shell in `workspace` will have. Errors mean the
/// shell must not run (fail closed).
pub async fn prepare(db: &PgPool, workspace: &Path) -> Result<View> {
    let ws = workspace.canonicalize().context("workspace")?;
    let prefix = format!("{}/", ws.to_string_lossy().trim_end_matches('/'));
    // Sessions a deny reaches: the contaminated ones and, through parents and inputs, their readers.
    let rows = sqlx::query(
        "WITH RECURSIVE edges AS (SELECT id AS reader, parent AS writer FROM sessions WHERE parent IS NOT NULL
             UNION ALL SELECT reader, writer FROM session_inputs),
           bad(id) AS (SELECT affected FROM contamination WHERE cleared_at IS NULL
             UNION SELECT e.reader FROM edges e JOIN bad b ON e.writer = b.id)
         SELECT r.path, r.sha256, r.session_id, CASE WHEN b.id IS NOT NULL THEN 5::smallint ELSE r.risk END AS risk
         FROM item_ratings r LEFT JOIN bad b ON b.id = r.session_id
         WHERE starts_with(r.path, $1) OR r.risk = 5 OR b.id IS NOT NULL")
        .bind(&prefix).fetch_all(db).await?;
    let mut by_path: HashMap<String, Vec<(String, Option<Uuid>, i16)>> = HashMap::new();
    for r in rows {
        by_path.entry(r.get("path")).or_default().push((r.get("sha256"), r.get("session_id"), r.get("risk")));
    }
    tokio::task::spawn_blocking(move || view(&ws, by_path)).await?
}

type Rated = HashMap<String, Vec<(String, Option<Uuid>, i16)>>;

fn view(ws: &Path, by_path: Rated) -> Result<View> {
    let (mut masks, mut seen, mut linked) = (Vec::new(), Vec::new(), HashMap::new());
    for (path, entries) in by_path {
        let p = PathBuf::from(&path);
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        if !meta.is_file() { continue; }
        let bad = |d: &str| entries.iter().any(|(sha, _, r)| sha == d && *r == 5);
        let current = if meta.len() > MAX_FILE { None } else { file_digest(&p, &meta).ok() };
        let masked = match &current {
            Some(d) => bad(d),
            // Unreadable or oversized: masked if any version of it is contaminated.
            None => entries.iter().any(|(_, _, r)| *r == 5),
        };
        if masked {
            if meta.nlink() > 1 { linked.insert((meta.dev(), meta.ino()), meta.nlink()); }
            masks.push(p);
        } else if let Some(d) = current.filter(|_| p.starts_with(ws)) {
            let matched: Vec<_> = entries.iter().filter(|(sha, _, _)| *sha == d).collect();
            if matched.is_empty() { continue; }
            let text = std::fs::read(&p).unwrap_or_default();
            let lines = String::from_utf8_lossy(&text).lines().map(str::trim)
                .filter(|l| l.chars().count() >= 20).take(200).map(String::from).collect();
            seen.push(Seen {
                risk: matched.iter().map(|m| m.2).max().unwrap_or(1),
                writers: matched.iter().filter_map(|m| m.1).collect(),
                lines,
            });
        }
    }
    if !linked.is_empty() {
        let mut found: HashMap<(u64, u64), u64> = HashMap::new();
        let mut extra = Vec::new();
        let complete = walk(ws, |p, m| if linked.contains_key(&(m.dev(), m.ino())) {
            *found.entry((m.dev(), m.ino())).or_default() += 1;
            if !masks.iter().any(|x| x == p) { extra.push(p.to_path_buf()); }
        });
        anyhow::ensure!(complete && linked.iter().all(|(k, n)| found.get(k) == Some(n)),
            "a contaminated file has hard links outside the workspace; the owner must remove them first");
        masks.extend(extra);
    }
    if !masks.is_empty() {
        masks.extend(ROOT_SOCKETS.iter().map(PathBuf::from).filter(|p| p.exists()));
    }
    Ok(View { masks, seen })
}

/// After a shell: register what a host-capable one (`host`) wrote in `workspace` since `start`,
/// then refuse the result if the session was stopped meanwhile.
pub async fn finish(db: &PgPool, session: Uuid, workspace: &Path, start: SystemTime, host: bool) -> Result<()> {
    if host {
        let ws = workspace.to_path_buf();
        let since = start.duration_since(SystemTime::UNIX_EPOCH)?.as_secs() as i64 - 1;
        let products = tokio::task::spawn_blocking(move || {
            let mut out: Vec<(String, String)> = Vec::new();
            let complete = walk(&ws, |p, m| if m.ctime() >= since && m.len() <= MAX_FILE {
                if let (Ok(d), Ok(real)) = (file_digest(p, m), p.canonicalize()) { out.push((real.to_string_lossy().into_owned(), d)); }
            });
            if !complete { tracing::warn!("product scan of {} stopped at {MAX_WALK} entries", ws.display()); }
            out
        }).await?;
        if !products.is_empty() {
            let (paths, digests): (Vec<String>, Vec<String>) = products.into_iter().unzip();
            sqlx::query("INSERT INTO item_ratings(path,sha256,session_id,risk)
                SELECT p, d, s.id, s.risk FROM unnest($1::text[], $2::text[]) AS t(p,d), sessions s WHERE s.id=$3
                ON CONFLICT(path,sha256,session_id) DO UPDATE SET risk=GREATEST(item_ratings.risk,EXCLUDED.risk)")
                .bind(&paths).bind(&digests).bind(session).execute(db).await?;
        }
    }
    anyhow::ensure!(!crate::taint::stopped(db, session).await?, "withheld: the session was stopped by contamination while the command ran; owner review required");
    Ok(())
}

/// bubblewrap arguments that mask `masks`; they go after every other mount.
pub fn mask(c: &mut tokio::process::Command, masks: &[PathBuf]) {
    for m in masks {
        c.arg("--ro-bind").arg("/dev/null").arg(m);
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
        assert!(walk(&d, |p, _| seen.push(p.to_path_buf())));
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
        let mut rated: Rated = HashMap::new();
        let key = |n: &str| ws.join(n).to_string_lossy().into_owned();
        rated.insert(key("bad"), vec![(digest(b"evil"), Some(w), 5)]);
        rated.insert(key("ok"), vec![(digest(b"short\n  a line long enough to be recognised  \n"), Some(w), 3)]);
        rated.insert(key("edited"), vec![(digest(b"old bytes"), Some(w), 5)]);
        let v = view(&ws, rated).unwrap();
        assert!(v.masks.contains(&ws.join("bad")) && v.masks.contains(&ws.join("alias")));
        assert!(!v.masks.contains(&ws.join("edited")), "only the contaminated bytes are masked");
        assert_eq!(v.exposure("short, nothing more"), (1, vec![]));
        assert_eq!(v.exposure("$ cat ok\na line long enough to be recognised\n"), (3, vec![w]));
    }
}
