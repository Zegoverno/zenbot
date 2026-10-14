//! The machine zen runs on: the installer's `env` file, the engines' state and signing in to
//! them (`zen login`), and git's identity and the root filesystem's use for `zen status`.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use zen_client::parse_env;

use crate::client::zen_home;

/// Values from `<zen home>/env` (written by the installer), read once.
pub(crate) fn zen_env() -> &'static BTreeMap<String, String> {
    static ENV: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| parse_env(&std::fs::read_to_string(zen_home().join("env")).unwrap_or_default()))
}

pub(crate) fn zen_path() -> String {
    format!("{}:{}", zen_env().get("PATH").cloned().unwrap_or_default(), std::env::var("PATH").unwrap_or_default())
}

/// "Name <email>" that commits in the zenbot checkout will carry, if git has one configured.
pub(crate) fn git_identity() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let repo = zen_env().get("ZEN_REPO").cloned().unwrap_or(format!("{home}/zenbot"));
    let get = |key: &str| {
        let out = std::process::Command::new("git").args(["-C", &repo, "config", key]).output().ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!v.is_empty()).then_some(v)
    };
    Some(format!("{} <{}>", get("user.name")?, get("user.email")?))
}

/// The engines' state as `scripts/update-engines.sh` last left it (`<zen home>/engines.json`).
pub(crate) fn engines_state() -> Value {
    std::fs::read_to_string(zen_home().join("engines.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

/// The `zen status` line for the engines: "claude 2.1.291, codex 0.160.1 (checked …)",
/// with any engine not left up to date (rolled back, skipped, failed) named with its status.
pub(crate) fn engines_line(state: &Value) -> Option<String> {
    let engines = state["engines"].as_object().filter(|e| !e.is_empty())?;
    let mut names: Vec<&String> = engines.keys().collect();
    names.sort_by_key(|n| (["claude", "codex"].iter().position(|k| k == n).unwrap_or(9), n.to_string()));
    let versions: Vec<String> = names.iter().map(|n| format!("{n} {}", engines[*n]["version"].as_str().filter(|v| !v.is_empty()).unwrap_or("?"))).collect();
    let problems: Vec<String> = names
        .iter()
        .filter_map(|n| {
            let s = engines[*n]["status"].as_str().unwrap_or("");
            (!(s == "up to date" || s.starts_with("updated"))).then(|| format!("{n}: {s}"))
        })
        .collect();
    let checked = state["checked"].as_str().map(|c| c.replacen('T', " ", 1).trim_end_matches('Z').to_string() + " UTC").unwrap_or_default();
    let mut line = format!("{}  (checked {checked}", versions.join(", "));
    if !problems.is_empty() {
        line += &format!("; {}", problems.join("; "));
    }
    Some(line + ")")
}

/// Share of the root filesystem above which `zen status` warns (the VM's disk once filled up).
pub(crate) const DISK_WARN_PERCENT: u64 = 80;

/// The root filesystem's use from `df -Pk /`: `{ total, used, avail }` in bytes and `percent` as
/// df counts it (used of what non-root users can use); null when df isn't there or says nothing.
pub(crate) fn root_disk() -> Value {
    std::process::Command::new("df")
        .args(["-Pk", "/"])
        .output()
        .ok()
        .and_then(|o| parse_df(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(Value::Null)
}

/// The figures of `df -P` output's first filesystem (sizes in 1024-byte blocks).
fn parse_df(out: &str) -> Option<Value> {
    let f: Vec<&str> = out.lines().nth(1)?.split_whitespace().collect();
    let n = |i: usize| f.get(i)?.parse::<u64>().ok();
    let (total, used, avail) = (n(1)?, n(2)?, n(3)?);
    let percent: u64 = f.get(4)?.trim_end_matches('%').parse().ok()?;
    Some(serde_json::json!({ "total": total * 1024, "used": used * 1024, "avail": avail * 1024, "percent": percent }))
}

/// The `zen status` line for the root filesystem: "12.3G of 24.5G used (51%)", with a warning
/// above [`DISK_WARN_PERCENT`].
pub(crate) fn disk_line(disk: &Value) -> Option<String> {
    let gib = |k: &str| disk[k].as_u64().map(|b| format!("{:.1}G", b as f64 / (1u64 << 30) as f64));
    let percent = disk["percent"].as_u64()?;
    let mut line = format!("{} of {} used ({percent}%)", gib("used")?, gib("total")?);
    if percent > DISK_WARN_PERCENT {
        line += &format!("  WARNING: over {DISK_WARN_PERCENT}% full; free space (the `prune` job and scripts/upgrade.sh clean zen's own leftovers)");
    }
    Some(line)
}

/// Whether a model engine (Claude Code, Codex) is signed in, from `/api/models`. OpenRouter is
/// listed there too but only serves System One and embeddings, so it can't run a turn.
pub(crate) fn engine_signed_in(models: &Value) -> bool {
    models["authenticated"].as_object().is_some_and(|a| a.iter().any(|(n, v)| n != "openrouter" && *v == true))
}

pub(crate) fn signed_in(engine: &str) -> bool {
    let run = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).env("PATH", zen_path()).output().ok();
    match engine {
        "claude" => run("claude", &["auth", "status"]).is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("\"loggedIn\": true")),
        "codex" => run("codex", &["login", "status"]).is_some_and(|o| o.status.success()),
        _ => false,
    }
}

pub(crate) fn run_login(cmd: &str, args: &[&str]) -> Result<()> {
    let mut c = std::process::Command::new(cmd);
    c.args(args).env("PATH", zen_path());
    let status = c.status().with_context(|| format!("running {cmd} (is it installed?)"))?;
    if !status.success() {
        bail!("{cmd} sign-in failed");
    }
    Ok(())
}

pub(crate) fn login(which: Option<&str>) -> Result<()> {
    let targets: Vec<&str> = match which {
        Some(w) => vec![w],
        None => ["claude", "codex"].into_iter().filter(|e| !signed_in(e)).collect(),
    };
    if targets.is_empty() {
        eprintln!("Claude Code and Codex are both signed in.");
        return Ok(());
    }
    for t in targets {
        match t {
            "claude" => {
                eprintln!("\n== Claude (your Claude subscription)\nOpen the link, approve, and paste the code back here if asked.\n");
                run_login("claude", &["auth", "login", "--claudeai"])?;
            }
            "codex" => {
                eprintln!("\n== Codex (your ChatGPT subscription)\nOpen the link and enter the code shown.\n");
                run_login("codex", &["login", "--device-auth"])?;
            }
            other => bail!("unknown engine `{other}`; use claude or codex"),
        }
    }
    eprintln!("\nSigned in. Run `zen` to start.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn an_openrouter_key_alone_is_not_a_signed_in_engine() {
        assert!(!engine_signed_in(&json!({ "authenticated": { "openrouter": true, "claude": false } })));
        assert!(engine_signed_in(&json!({ "authenticated": { "openrouter": false, "codex": true } })));
        assert!(!engine_signed_in(&json!({})));
    }

    #[test]
    fn engines_line_lists_versions_and_flags_problems() {
        let state = json!({ "checked": "2026-10-06T14:32:34Z", "engines": {
            "codex": { "version": "0.160.1", "status": "updated from 0.159.0" },
            "claude": { "version": "2.1.284", "status": "rolled back from 2.1.291" },
        }});
        assert_eq!(
            engines_line(&state).unwrap(),
            "claude 2.1.284, codex 0.160.1  (checked 2026-10-06 14:32:34 UTC; claude: rolled back from 2.1.291)"
        );
        assert_eq!(engines_line(&Value::Null), None);
        assert_eq!(engines_line(&json!({ "engines": {} })), None);
    }

    #[test]
    fn disk_line_shows_use_and_warns_over_80_percent() {
        let df = "Filesystem     1024-blocks     Used Available Capacity Mounted on\n/dev/root        25671908 12345678  13309846      49% /\n";
        let disk = parse_df(df).unwrap();
        assert_eq!(disk["used"], 12345678u64 * 1024);
        assert_eq!(disk_line(&disk).unwrap(), "11.8G of 24.5G used (49%)");
        let full = parse_df("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/root 25671908 21823122 3848786 85% /").unwrap();
        assert!(disk_line(&full).unwrap().starts_with("20.8G of 24.5G used (85%)  WARNING: over 80% full"));
        assert_eq!(parse_df("df: /: No such file or directory\n"), None);
        assert_eq!(disk_line(&Value::Null), None);
    }
}
