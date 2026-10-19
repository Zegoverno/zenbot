//! The owner's trust list (D-067): `~/.zenbot/trust.toml`, changed only by the owner (by hand, or
//! `zen trust`, which runs with the owner's token). The first input to context risk: what's on it
//! keeps its level; everything else is rated by System One (`s1/source_risk.toml`), or counts as
//! unknown.
//!
//! ```toml
//! [[trust]]
//! kind = "url"         # url (host or host/path) | api (host) | mcp (server name) | repo (git remote)
//! match = "docs.rs"
//! level = "trusted"    # trusted | known | partly-open | blocked
//! note = "Rust docs"
//! ```
//!
//! - `url` and `api` match an exact HTTPS host, or whole path segments under it (`x.com/paulg`
//!   matches `x.com/paulg/status/1`, not `x.com/paulgraham`); no subdomains.
//! - `repo` matches a git remote as `host/owner/name`, `*` standing for one segment
//!   (`github.com/Zegoverno/*`): every clone and worktree with that remote counts, wherever it is.
//! - Levels map to ratings: trusted 1 (like the owner's own files), known 2 (reputable: read, never
//!   obeyed), partly-open 3 (user content), blocked 5 (never read).
//! - Built in, under the owner's entries: package registries (`api`, trusted), so builds stay low.
//! - A file that fails to parse never takes effect: the last good list stays and the error is
//!   reported with its line (`zen trust`, `zen status`).

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use std::time::{Instant, SystemTime};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    Trusted,
    Known,
    PartlyOpen,
    Blocked,
}

impl Level {
    pub fn rating(self) -> i16 {
        match self {
            Level::Trusted => 1,
            Level::Known => 2,
            Level::PartlyOpen => 3,
            Level::Blocked => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Url,
    Api,
    Mcp,
    Repo,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: Kind,
    #[serde(rename = "match")]
    pub pattern: String,
    pub level: Level,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    trust: Vec<Entry>,
}

/// Package registries: what builds download from. Their content is code, not instructions to the
/// agent, so reaching them keeps a session low. The owner's entries override these.
const BUILT_IN: &[(&str, Level)] = &[
    ("crates.io", Level::Trusted),
    ("index.crates.io", Level::Trusted),
    ("static.crates.io", Level::Trusted),
    ("static.rust-lang.org", Level::Trusted),
    ("pypi.org", Level::Trusted),
    ("files.pythonhosted.org", Level::Trusted),
    ("registry.npmjs.org", Level::Trusted),
    ("proxy.golang.org", Level::Trusted),
    ("sum.golang.org", Level::Trusted),
    ("deb.debian.org", Level::Trusted),
    ("security.debian.org", Level::Trusted),
    ("archive.ubuntu.com", Level::Trusted),
    ("security.ubuntu.com", Level::Trusted),
    ("ports.ubuntu.com", Level::Trusted),
    // Git hosting: code transport; issues and comments on it are user content.
    ("github.com", Level::Known),
    ("codeload.github.com", Level::Known),
    ("objects.githubusercontent.com", Level::Known),
    ("gitlab.com", Level::Known),
];

pub fn path() -> PathBuf {
    crate::zen_home().join("trust.toml")
}

struct State {
    entries: Vec<Entry>,
    problem: Option<String>,
    stamp: Option<SystemTime>,
    checked: Instant,
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| RwLock::new(load(None)));

/// Parse the owner's file; errors carry their line and column.
pub fn parse(src: &str) -> Result<Vec<Entry>, String> {
    let f: File = toml::from_str(src).map_err(|e| crate::s1::check::position(src, &e))?;
    for (i, e) in f.trust.iter().enumerate() {
        validate(e).map_err(|m| format!("entry {}: {m}", i + 1))?;
    }
    Ok(f.trust)
}

fn validate(e: &Entry) -> Result<(), String> {
    let p = e.pattern.trim();
    match e.kind {
        Kind::Url | Kind::Api => crate::sources::key(p).map(|_| ()).map_err(|e| e.to_string()),
        Kind::Repo if p.split('/').count() != 3 || p.contains("://") => Err(format!("repo `{p}`: write it as host/owner/name, e.g. github.com/Zegoverno/zenbot (* for any)")),
        Kind::Mcp if p.is_empty() => Err("mcp needs the server's name from mcp.json".into()),
        _ => Ok(()),
    }
}

fn load(previous: Option<&State>) -> State {
    let stamp = std::fs::metadata(path()).and_then(|m| m.modified()).ok();
    let (entries, problem) = match std::fs::read_to_string(path()) {
        Err(_) => (Vec::new(), None),
        Ok(src) => match parse(&src) {
            Ok(e) => (e, None),
            Err(e) => {
                tracing::warn!("{}: {e} (the last good list stays)", path().display());
                (previous.map(|p| p.entries.clone()).unwrap_or_default(), Some(e))
            }
        },
    };
    State { entries, problem, stamp, checked: Instant::now() }
}

/// The owner's entries, reloaded when the file changed (checked at most every 2 s).
fn entries() -> (Vec<Entry>, Option<String>) {
    let stale = STATE.read().map(|s| s.checked.elapsed().as_secs_f64() > 2.0).unwrap_or(false);
    if stale {
        if let Ok(mut s) = STATE.write() {
            let stamp = std::fs::metadata(path()).and_then(|m| m.modified()).ok();
            if stamp != s.stamp {
                let fresh = load(Some(&s));
                *s = fresh;
            } else {
                s.checked = Instant::now();
            }
        }
    }
    let s = STATE.read().unwrap_or_else(|e| e.into_inner());
    (s.entries.clone(), s.problem.clone())
}

/// The owner's level for a URL (an `url` or `api` entry; the longest match wins), else None. The
/// built-in registries rate hosts a shell reaches (`host_level`), never content: a page on
/// github.com is user content until the owner or System One says otherwise.
pub fn url_level(url: &str) -> Option<i16> {
    let u = crate::sources::canonical(url, false).ok()?;
    let (entries, _) = entries();
    entries
        .iter()
        .filter(|e| matches!(e.kind, Kind::Url | Kind::Api))
        .filter_map(|e| crate::sources::key(&e.pattern).ok().map(|k| (k, e.level)))
        .filter(|(k, _)| crate::sources::matches(k, &u))
        .max_by_key(|(k, _)| k.len())
        .map(|(_, l)| l.rating())
}

/// The level for a host the shell reached (egress): the owner's host-wide `url`/`api` entry, else a
/// built-in registry's.
pub fn host_level(host: &str) -> Option<i16> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let (entries, _) = entries();
    entries
        .iter()
        .filter(|e| matches!(e.kind, Kind::Url | Kind::Api) && e.pattern.trim().trim_start_matches("https://").trim_end_matches('/') == host)
        .map(|e| e.level.rating())
        .min()
        .or_else(|| host_built_in(&host))
}

fn host_built_in(host: &str) -> Option<i16> {
    BUILT_IN.iter().find(|(h, _)| *h == host).map(|(_, l)| l.rating())
}

/// The owner's level for an MCP server by its name in `mcp.json`.
pub fn mcp_level(name: &str) -> Option<i16> {
    let (entries, _) = entries();
    entries.iter().find(|e| e.kind == Kind::Mcp && e.pattern == name).map(|e| e.level.rating())
}

/// The remote (`host/owner/name`) of the git repository holding `path`, read from its config (a
/// worktree's from its main checkout's), `origin` first.
pub fn remote_of(path: &Path) -> Option<String> {
    let top = path.ancestors().find(|d| d.join(".git").exists())?;
    let dot_git = top.join(".git");
    let git_dir = if dot_git.is_file() {
        let link = std::fs::read_to_string(&dot_git).ok()?;
        let entry = PathBuf::from(link.strip_prefix("gitdir:")?.trim());
        // `<main>/.git/worktrees/<name>` → `<main>/.git`.
        let common = std::fs::read_to_string(entry.join("commondir")).ok().map(|c| entry.join(c.trim()));
        common.unwrap_or(entry)
    } else {
        dot_git
    };
    let config = std::fs::read_to_string(git_dir.join("config")).ok()?;
    let mut remotes: Vec<(String, String)> = Vec::new();
    let mut current = None;
    for line in config.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("[remote \"") {
            current = rest.strip_suffix("\"]").map(String::from);
        } else if line.starts_with('[') {
            current = None;
        } else if let (Some(name), Some(url)) = (&current, line.strip_prefix("url").map(str::trim).and_then(|l| l.strip_prefix('=')).map(str::trim)) {
            remotes.push((name.clone(), url.to_string()));
        }
    }
    remotes.sort_by_key(|(n, _)| n != "origin");
    remotes.into_iter().find_map(|(_, url)| normalize_remote(&url))
}

/// `https://github.com/a/b.git`, `git@github.com:a/b.git`, `ssh://git@github.com/a/b` → `github.com/a/b`.
pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let rest = if let Some((_, r)) = url.split_once("://") {
        r.rsplit_once('@').map(|(_, h)| h).unwrap_or(r).to_string()
    } else if let Some((user_host, path)) = url.split_once(':') {
        format!("{}/{path}", user_host.rsplit_once('@').map(|(_, h)| h).unwrap_or(user_host))
    } else {
        return None;
    };
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    (parts.len() >= 3).then(|| format!("{}/{}/{}", parts[0].to_ascii_lowercase(), parts[1], parts[2]))
}

/// The owner's level for the repository holding `path`, by its remote.
pub fn repo_level(path: &Path) -> Option<i16> {
    let remote = remote_of(path)?;
    let (entries, _) = entries();
    entries.iter().filter(|e| e.kind == Kind::Repo).find(|e| repo_matches(&e.pattern, &remote)).map(|e| e.level.rating())
}

fn repo_matches(pattern: &str, remote: &str) -> bool {
    let (p, r): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), remote.split('/').collect());
    p.len() == r.len() && p.iter().zip(&r).enumerate().all(|(i, (p, r))| *p == "*" || if i == 0 { p.eq_ignore_ascii_case(r) } else { p == r })
}

/// The list as the owner sees it, with the built-in registries and any problem in the file.
pub fn list() -> serde_json::Value {
    let (entries, problem) = entries();
    serde_json::json!({
        "path": path().display().to_string(),
        "entries": entries,
        "built_in": BUILT_IN.iter().map(|(h, l)| serde_json::json!({ "kind": "api", "match": h, "level": l })).collect::<Vec<_>>(),
        "problem": problem,
    })
}

/// Add or replace (`level` Some) or remove (None) an entry: the owner's `zen trust`. Rewrites the
/// file from its entries.
pub fn change(kind: Kind, pattern: &str, level: Option<Level>, note: &str) -> Result<serde_json::Value> {
    let current = match std::fs::read_to_string(path()) {
        Ok(src) => parse(&src).map_err(|e| anyhow::anyhow!("{}: {e}; fix it by hand first", path().display()))?,
        Err(_) => Vec::new(),
    };
    let pattern = pattern.trim().to_string();
    let mut entries: Vec<Entry> = current.into_iter().filter(|e| !(e.kind == kind && e.pattern == pattern)).collect();
    if let Some(level) = level {
        let entry = Entry { kind, pattern: pattern.clone(), level, note: note.to_string() };
        validate(&entry).map_err(|e| anyhow::anyhow!(e))?;
        entries.push(entry);
    } else if entries.len() == std::fs::read_to_string(path()).ok().and_then(|s| parse(&s).ok()).map(|v| v.len()).unwrap_or(0) {
        bail!("no {kind:?} entry `{pattern}`");
    }
    std::fs::write(path(), render(&entries))?;
    if let Ok(mut s) = STATE.write() {
        let fresh = load(Some(&s));
        *s = fresh;
    }
    Ok(serde_json::json!({ "kind": kind, "match": pattern, "level": level, "ok": true }))
}

/// The file's text for `entries` (TOML, one `[[trust]]` table each).
fn render(entries: &[Entry]) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let mut out = String::from("# The owner's trust list (D-067): `zen trust` or edit by hand. Levels: trusted, known, partly-open, blocked.\n");
    for e in entries {
        let kind = serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        let level = serde_json::to_value(e.level).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        out.push_str(&format!("\n[[trust]]\nkind = {}\nmatch = {}\nlevel = {}\n", q(&kind), q(&e.pattern), q(&level)));
        if !e.note.is_empty() {
            out.push_str(&format!("note = {}\n", q(&e.note)));
        }
    }
    out
}

/// `GET /api/trust`.
#[derive(Deserialize)]
pub(crate) struct ListQuery { #[serde(default)] suggest: bool }

pub(crate) async fn api_list(axum::extract::State(app): axum::extract::State<crate::AppState>, axum::extract::Query(query): axum::extract::Query<ListQuery>) -> crate::api::ApiResult<axum::Json<serde_json::Value>> {
    if !query.suggest { return Ok(axum::Json(list())); }
    use sqlx::Row;
    // Observations are evidence for review, never automatic authority. Ignore failed calls and
    // sessions with an active denial; the owner still chooses the entry's level explicitly.
    let rows = sqlx::query("SELECT host,sum(reads)::bigint AS reads FROM (
        SELECT e.host,e.count AS reads FROM egress e JOIN sessions s ON s.id=e.session_id
        WHERE e.outcome='allow' AND e.rating<5 AND NOT zen_session_contaminated(s.id)
        UNION ALL
        SELECT split_part(split_part(t.args->>'url','://',2),'/',1) AS host,1::bigint AS reads
        FROM tool_calls t JOIN sessions s ON s.id=t.session_id
        WHERE t.name='web_fetch' AND NOT t.is_error AND NOT zen_session_contaminated(s.id)
        ) observations GROUP BY host HAVING sum(reads)>=2 ORDER BY reads DESC,host LIMIT 50")
        .fetch_all(&app.db).await?;
    let entries: Vec<_> = rows.iter().filter_map(|row| {
        let host: String = row.get("host");
        let parsed = reqwest::Url::parse(&format!("https://{host}/")).ok()?;
        if parsed.host_str()? != host || host_level(&host).is_some() { return None; }
        Some(serde_json::json!({"kind":"url","match":host,"level":"known","note":format!("{} clean observations; review before adding",row.get::<i64,_>("reads"))}))
    }).collect();
    Ok(axum::Json(serde_json::json!({"entries":entries,"built_in":[],"suggestions":true})))
}

#[derive(Deserialize)]
pub(crate) struct Change {
    kind: Kind,
    #[serde(rename = "match")]
    pattern: String,
    #[serde(default)]
    level: Option<Level>,
    #[serde(default)]
    note: String,
    #[serde(default)]
    remove: bool,
}

/// `POST /api/trust`: the owner adds, changes or removes an entry.
pub(crate) async fn api_change(axum::Json(c): axum::Json<Change>) -> crate::api::ApiResult<axum::Json<serde_json::Value>> {
    let level = if c.remove { None } else { Some(c.level.unwrap_or(Level::Trusted)) };
    change(c.kind, &c.pattern, level, &c.note)
        .map(axum::Json)
        .map_err(|e| crate::api::ApiError(axum::http::StatusCode::BAD_REQUEST, format!("{e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_normalize_and_match_any_clone() {
        assert_eq!(normalize_remote("https://github.com/Zegoverno/zenbot.git").as_deref(), Some("github.com/Zegoverno/zenbot"));
        assert_eq!(normalize_remote("git@github.com:Zegoverno/zenbot.git").as_deref(), Some("github.com/Zegoverno/zenbot"));
        assert_eq!(normalize_remote("ssh://git@GitHub.com/Zegoverno/zenbot/").as_deref(), Some("github.com/Zegoverno/zenbot"));
        assert_eq!(normalize_remote("/home/me/zenbot"), None, "a local path is no remote");
        assert!(repo_matches("github.com/Zegoverno/*", "github.com/Zegoverno/zenbot"));
        assert!(!repo_matches("github.com/Zegoverno/*", "github.com/Zegoverno-evil/zenbot"));
        assert!(!repo_matches("github.com/Zegoverno/zenbot", "github.com/zegoverno/zenbot"), "owner and name match exactly");
    }

    #[test]
    fn a_worktree_and_a_clone_report_their_remote() {
        let dir = crate::test_util::TestDir::new("trust-remote");
        let git = |args: &[&str], at: &Path| assert!(std::process::Command::new("git").args(args).current_dir(at).output().unwrap().status.success(), "git {args:?}");
        std::fs::create_dir_all(dir.join("main")).unwrap();
        git(&["init", "-q"], &dir.join("main"));
        git(&["remote", "add", "origin", "git@github.com:Zegoverno/zenbot.git"], &dir.join("main"));
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"], &dir.join("main"));
        git(&["worktree", "add", "-q", "../wt"], &dir.join("main"));
        assert_eq!(remote_of(&dir.join("main/src/a.rs")).as_deref(), Some("github.com/Zegoverno/zenbot"));
        assert_eq!(remote_of(&dir.join("wt")).as_deref(), Some("github.com/Zegoverno/zenbot"));
    }

    #[test]
    fn what_zen_trust_writes_reads_back() {
        let entries = vec![
            Entry { kind: Kind::Url, pattern: "docs.rs".into(), level: Level::Trusted, note: "Rust \"docs\"".into() },
            Entry { kind: Kind::Repo, pattern: "github.com/Zegoverno/*".into(), level: Level::Trusted, note: String::new() },
        ];
        assert_eq!(parse(&render(&entries)).unwrap(), entries);
    }

    #[test]
    fn the_file_is_checked_with_its_line() {
        assert!(parse("[[trust]]\nkind = \"url\"\nmatch = \"docs.rs\"\nlevel = \"trusted\"\n").is_ok());
        let e = parse("[[trust]]\nkind = \"url\"\nmatch = \"docs.rs\"\nlevell = \"trusted\"\n").unwrap_err();
        assert!(e.starts_with("4:1:") && e.contains("did you mean `level`"), "{e}");
        assert!(parse("[[trust]]\nkind = \"repo\"\nmatch = \"https://github.com/a/b\"\nlevel = \"trusted\"\n").unwrap_err().contains("host/owner/name"));
        assert!(parse("[[trust]]\nkind = \"url\"\nmatch = \"http://x.com\"\nlevel = \"known\"\n").is_err());
    }
}
