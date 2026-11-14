//! The `prune` system job: old saved tool outputs, prompt-file backups, database dumps, eval runs
//! and engine session files under the zen home, each kept for its own time.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};

/// Days a prompt file's backup is kept in `<zen home>/backups/prompt-files`.
const BACKUP_DAYS: u64 = 90;

/// Days a saved tool output (cut bash/MCP output, fetched PDFs) is kept in `<zen home>/outputs`.
const OUTPUT_DAYS: u64 = 30;

/// The `prune` job: old saved outputs, prompt-file backups, database dumps and eval runs under the
/// zen home `home`.
pub(super) fn prune_files(home: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    let backups = prune_backups(&home.join(crate::memory::BACKUPS), BACKUP_DAYS);
    if backups > 0 {
        notes.push(format!("backups: removed {backups} prompt-file backup(s) older than {BACKUP_DAYS} days"));
    }
    let outputs = prune_outputs(&home.join("outputs"), OUTPUT_DAYS);
    if outputs > 0 {
        notes.push(format!("outputs: removed {outputs} saved tool output file(s) expired after {OUTPUT_DAYS} days or over the 256 MiB storage budget"));
    }
    let dumps = prune_dumps(&home.join("backups"), DUMP_KEEP);
    if dumps > 0 {
        notes.push(format!("backups: removed {dumps} database dump(s), keeping the newest {DUMP_KEEP}"));
    }
    let evals = prune_eval_runs(&home.join("evals"), EVAL_DAYS, Utc::now());
    if evals > 0 {
        notes.push(format!("evals: removed {evals} eval run(s) older than {EVAL_DAYS} days"));
    }
    let user = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let engines = prune_engine_sessions(home, &user.join(".claude/projects"), ENGINE_DAYS, EVAL_DAYS as u64);
    if engines > 0 {
        notes.push(format!("engines: removed {engines} engine session file(s) or folder(s) unused for {ENGINE_DAYS} days"));
    }
    notes
}

/// Days an engine's own session state is kept: Codex rollouts in zenbot's private Codex
/// home and Claude Code's session files for zenbot's engine folder. A turn whose session is gone
/// starts it again from the kernel's history.
const ENGINE_DAYS: u64 = 14;

/// Claude Code's folder name for a working directory: every `/` and `.` a `-`.
fn claude_project(dir: &Path) -> String {
    dir.to_string_lossy().replace(['/', '.'], "-")
}

/// Remove what's directly in `dir` (with `deep`, files anywhere below it, then emptied folders) whose
/// modification time is over `days` old; returns how many. Symlinks are never followed.
fn remove_older(dir: &Path, days: u64, deep: bool, keep: &dyn Fn(&Path) -> bool) -> usize {
    let limit = Duration::from_secs(days * 86_400);
    let old = |m: &std::fs::Metadata| m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > limit);
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if keep(&path) || meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() && deep {
            n += remove_older(&path, days, true, keep);
            if std::fs::read_dir(&path).is_ok_and(|mut d| d.next().is_none()) && std::fs::remove_dir(&path).is_ok() {
                n += 1;
            }
        } else if old(&meta) && (if meta.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }).is_ok() {
            n += 1;
        }
    }
    n
}

/// zenbot's engine session state: Codex rollouts (`engine/codex/home/sessions`), Claude
/// Code's files for the engine's folder (`projects`, only zenbot's own, never the owner's) after
/// `days`; eval kernels' Claude folders after `eval_days`; turn socket folders after a day.
fn prune_engine_sessions(home: &Path, projects: &Path, days: u64, eval_days: u64) -> usize {
    let none = |_: &Path| false;
    let mut n = remove_older(&home.join("engine/codex/home/sessions"), days, true, &none);
    n += remove_older(&projects.join(claude_project(&home.join("engine/claude"))), days, false, &none);
    n += remove_older(&home.join("engine/sockets"), 1, false, &none);
    // Eval kernels run with their own zen home under an eval run's folder (scripts/eval.sh).
    let not_eval = |p: &Path| !p.file_name().is_some_and(|f| f.to_string_lossy().contains("-zenbot-evals-"));
    n += remove_older(projects, eval_days, false, &not_eval);
    n
}

/// Database dumps (`<zen home>/backups/<UTC time>-<label>.dump`, scripts/db.sh `db_backup`) kept.
/// The install takes one right before applying migrations, and a failed install's rollback names
/// that one (the newest) in its restore hint, so the newest few always stay.
const DUMP_KEEP: usize = 3;

/// Days an eval run (`<zen home>/evals/<UTC time>/`, scripts/eval.sh) is kept. The base results
/// it reuses live in `evals/cache` and the base builds in `evals/builds`; eval.sh prunes those.
const EVAL_DAYS: i64 = 7;

/// Whether `name` starts with the UTC time stamp the scripts name things with (`20261013T203126Z`),
/// which is returned.
fn stamp(name: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(name.get(..16)?, "%Y%m%dT%H%M%SZ").ok()
}

/// Remove all but the newest `keep` database dumps directly in `dir` (by the time stamp in their
/// name); returns how many. Only `<stamp>-<label>.dump` regular files count; anything else stays.
fn prune_dumps(dir: &Path, keep: usize) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let mut dumps: Vec<(NaiveDateTime, PathBuf)> = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let at = stamp(&name).filter(|_| name.as_bytes().get(16) == Some(&b'-') && name.ends_with(".dump"))?;
            Some((at, e.path()))
        })
        .collect();
    dumps.sort_by(|a, b| b.cmp(a));
    dumps.into_iter().skip(keep.max(1)).filter(|(_, p)| std::fs::remove_file(p).is_ok()).count()
}

/// Remove eval run directories directly in `dir` named exactly by a UTC time stamp more than `days`
/// days before `now`; returns how many. `builds`, `cache` and anything else stay.
fn prune_eval_runs(dir: &Path, days: i64, now: DateTime<Utc>) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let limit = now.naive_utc() - chrono::Duration::days(days);
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.len() == 16 && stamp(&name).is_some_and(|at| at < limit)
        })
        .filter(|e| std::fs::remove_dir_all(e.path()).is_ok())
        .count()
}

/// Remove regular files directly in `dir` whose name `ours` accepts and that weren't modified for
/// `days` days; returns how many. Anything else there is left alone.
fn prune(dir: &Path, days: u64, ours: impl Fn(&str) -> bool) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let limit = Duration::from_secs(days * 86_400);
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && ours(&e.file_name().to_string_lossy()))
        .filter(|e| e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > limit))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// Prompt-file backups (`*.md`) older than `days` days.
fn prune_backups(dir: &Path, days: u64) -> usize {
    prune(dir, days, |n| n.ends_with(".md"))
}

/// Saved outputs expire after `days` days, with oldest files evicted above the storage budget.
fn prune_outputs(dir: &Path, days: u64) -> usize {
    crate::output::prune_saved(dir, days, crate::output::TOTAL_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn engine_sessions_are_pruned_and_the_owners_claude_projects_kept() {
        let root = crate::test_util::TestDir::new("engine-prune");
        let (home, projects) = (root.join("zh"), root.join("projects"));
        let old = std::time::SystemTime::now() - Duration::from_secs(20 * 86_400);
        let touch = |p: &Path, aged: bool| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
            if aged { std::fs::File::options().write(true).open(p).unwrap().set_modified(old).unwrap(); }
        };
        let rollout = home.join("engine/codex/home/sessions/2026/10/01/rollout-a.jsonl");
        touch(&rollout, true);
        touch(&home.join("engine/codex/home/sessions/2026/10/21/rollout-b.jsonl"), false);
        let engine = projects.join(claude_project(&home.join("engine/claude")));
        touch(&engine.join("old.jsonl"), true);
        touch(&engine.join("new.jsonl"), false);
        let owners = projects.join("-home-me-work");
        touch(&owners.join("old.jsonl"), true);
        let eval = projects.join("-home-me--zenbot-evals-20261001T000000Z-work-x--zenbot-engine-claude");
        touch(&eval.join("s.jsonl"), true);
        std::fs::File::open(&eval).unwrap().set_modified(old).unwrap();
        let n = prune_engine_sessions(&home, &projects, 14, 7);
        assert!(!rollout.exists() && !rollout.parent().unwrap().exists(), "an old rollout and its emptied day folder go");
        assert!(home.join("engine/codex/home/sessions/2026/10/21/rollout-b.jsonl").exists(), "a recent one stays");
        assert!(!engine.join("old.jsonl").exists() && engine.join("new.jsonl").exists());
        assert!(owners.join("old.jsonl").exists(), "the owner's own Claude projects are never touched");
        assert!(!eval.exists(), "an old eval kernel's Claude folder goes");
        assert!(n >= 4, "{n}");
    }



    #[test]
    fn old_saved_outputs_are_pruned_and_others_kept() {
        let dir = crate::test_util::TestDir::new("outputs");
        let old = std::time::SystemTime::now() - Duration::from_secs(40 * 86_400);
        for name in ["1.log", "mcp-a.txt", "web-a.pdf", "notes.md", "fresh.log"] {
            std::fs::write(dir.join(name), "x").unwrap();
            if name != "fresh.log" {
                std::fs::File::options().write(true).open(dir.join(name)).unwrap().set_modified(old).unwrap();
            }
        }
        assert_eq!(prune_outputs(&dir, 30), 3);
        assert!(dir.join("notes.md").exists() && dir.join("fresh.log").exists());
    }

    #[test]
    fn only_the_newest_database_dumps_are_kept() {
        let dir = crate::test_util::TestDir::new("dumps");
        let names = [
            "20261007T133742Z-c9945db.dump",
            "20261009T164854Z-b78e535.dump",
            "20261013T004556Z-26f9477.dump",
            "20261013T203736Z-282d1b3.dump",
            "20261013T221423Z-manual.dump",
            "old.dump",
            "20261001T000000Z-x.tgz",
            "env.bak",
        ];
        for name in names {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        std::fs::create_dir(dir.join("20261001T000000Z-dir.dump")).unwrap();
        assert_eq!(prune_dumps(&dir, 3), 2);
        assert!(!dir.join("20261007T133742Z-c9945db.dump").exists() && !dir.join("20261009T164854Z-b78e535.dump").exists());
        for kept in &names[2..] {
            assert!(dir.join(kept).exists(), "{kept}");
        }
        assert!(dir.join("20261001T000000Z-dir.dump").is_dir());
        // The newest always stays, even when asked to keep none.
        assert_eq!(prune_dumps(&dir, 0), 2);
        assert!(dir.join("20261013T221423Z-manual.dump").exists());
    }

    #[test]
    fn old_eval_runs_are_pruned_and_builds_kept() {
        let dir = crate::test_util::TestDir::new("evals");
        for name in ["20261001T000000Z", "20261006T235959Z", "20261008T000001Z", "builds", "cache", "20261001T000000Z-x"] {
            std::fs::create_dir_all(dir.join(name).join("work")).unwrap();
        }
        std::fs::write(dir.join("20261002T000000Z"), "a file, not a run").unwrap();
        assert_eq!(prune_eval_runs(&dir, 7, t("2026-10-14T00:00:00Z")), 2);
        assert!(!dir.join("20261001T000000Z").exists() && !dir.join("20261006T235959Z").exists());
        for kept in ["20261008T000001Z", "builds", "cache", "20261001T000000Z-x", "20261002T000000Z"] {
            assert!(dir.join(kept).exists(), "{kept}");
        }
    }
}
