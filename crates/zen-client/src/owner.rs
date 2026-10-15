//! The owner's commands typed in a chat: `jobs`, `skills`, `tools` and `trust`, as the `zen`
//! terminal app's `/jobs …` and the Matrix bridge's `!jobs …`. They do what `zen jobs`, `zen skills`
//! and `zen tools` do, with the owner's token, so they can approve what only the owner may (resume an
//! agent's paused job, accept a draft skill or a made tool, trust a session that read untrusted
//! content, D-054).
//!
//! The client reads them before anything reaches the agent and calls the kernel itself: the agent
//! never sees or relays them, so text it read can't approve anything. Parsing is pure and tested;
//! [`run`] makes the calls and returns Markdown for the chat.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{json, Value};

use crate::{enc, Client};

/// One owner command.
#[derive(Debug, PartialEq)]
pub enum OwnerCmd {
    Sources,
    SourceAdd { source: String, risk: i16, note: String },
    SourceRm(String),
    /// `jobs`: every job, its schedule and state.
    Jobs,
    /// `jobs run <name>`: run it now, in the background (works on a paused job too).
    Run(String),
    /// `jobs resume <name>`: turn it on (approves an agent's paused job).
    Resume(String),
    /// `jobs pause <name>`.
    Pause(String),
    /// `jobs runs [name]`: recent runs and their reports.
    Runs(Option<String>),
    /// `skills`: skills (drafts with their fingerprint) and made tools.
    Skills,
    /// `skills accept|reject <domain/name> <sha>`.
    Skill { name: String, decision: &'static str, sha: String },
    /// `tools accept|reject <name> <sha>`.
    Tool { name: String, decision: &'static str, sha: String },
    /// `trust`: what marked the chat's session as having read untrusted content; `trust yes`
    /// lifts the mark, as the owner last looked at it.
    Trust { yes: bool },
    /// Inspect held actions or allow the exact action last inspected.
    Allow { yes: bool },
}

/// What a chat client keeps between owner commands: the prefix it reads them by (`/` or `!`) and,
/// per session, the last block the owner looked at with `trust`, which `trust yes` confirms.
#[derive(Debug, Default)]
pub struct Chat {
    pub prefix: &'static str,
    looked: HashMap<String, i64>,
    actions: HashMap<String, i64>,
}

impl Chat {
    pub fn new(prefix: &'static str) -> Self {
        Chat { prefix, looked: HashMap::new(), actions: HashMap::new() }
    }
}

/// How to use the commands, one line each, without the prefix (`/` or `!`) the client adds.
pub const USAGE: &[(&str, &str)] = &[
    ("sources [list|add <host/path> [2|3] [note…]|rm <host/path>]", "manage the owner source allowlist"),
    ("jobs", "every job, its schedule and state"),
    ("jobs run <name>", "run a job now (even a paused one)"),
    ("jobs resume <name>", "turn a job on (approves one zenbot created paused)"),
    ("jobs pause <name>", "pause a job"),
    ("jobs runs [name]", "recent runs and their reports"),
    ("skills", "skills and made tools, with the fingerprint to review a draft"),
    ("skills accept|reject <domain/name> <sha>", "activate or archive a draft skill"),
    ("tools accept|reject <name> <sha>", "approve a made tool (network, no sandbox) or reject it"),
    ("allow", "inspect the latest held action"),
    ("allow yes", "allow that exact action once, for ten minutes"),
    ("trust", "what made this session risky or tainted"),
    ("trust yes", "trust it again, as you just looked at it"),
];

/// The command in `text` (after the client's prefix), or None when its first word isn't one of
/// these. `Some(Err(usage))` when the word is ours but the rest doesn't fit.
pub fn parse(text: &str) -> Option<Result<OwnerCmd, String>> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let first = words.first()?.to_ascii_lowercase();
    let usage = |u: &str| Err(format!("usage: {u}"));
    let decision = |w: &str| match w.to_ascii_lowercase().as_str() {
        "accept" => Some("accept"),
        "reject" => Some("reject"),
        _ => None,
    };
    let parsed = match first.as_str() {
        "jobs" => match (words.get(1).map(|w| w.to_ascii_lowercase()).as_deref(), &words[2.min(words.len())..]) {
            (None | Some("list"), []) => Ok(OwnerCmd::Jobs),
            (Some("run"), [n]) => Ok(OwnerCmd::Run(n.to_string())),
            (Some("resume"), [n]) => Ok(OwnerCmd::Resume(n.to_string())),
            (Some("pause"), [n]) => Ok(OwnerCmd::Pause(n.to_string())),
            (Some("runs"), []) => Ok(OwnerCmd::Runs(None)),
            (Some("runs"), [n]) => Ok(OwnerCmd::Runs(Some(n.to_string()))),
            _ => usage("jobs [run|resume|pause <name> | runs [name]]"),
        },
        "sources" => match &words[1..] {
            [] | ["list"] => Ok(OwnerCmd::Sources),
            ["rm", source] => Ok(OwnerCmd::SourceRm(source.to_string())),
            ["add", source] => Ok(OwnerCmd::SourceAdd { source: source.to_string(), risk: 2, note: String::new() }),
            ["add", source, level, note @ ..] if matches!(*level, "2" | "3") =>
                Ok(OwnerCmd::SourceAdd { source: source.to_string(), risk: level.parse().unwrap_or(2), note: note.join(" ") }),
            _ => usage("sources [list|add <host/path> [2|3] [note…]|rm <host/path>]"),
        },
        "skills" => match &words[1..] {
            [] => Ok(OwnerCmd::Skills),
            [d, name, sha] if decision(d).is_some() => Ok(OwnerCmd::Skill { name: name.to_string(), decision: decision(d).unwrap_or("reject"), sha: sha.to_string() }),
            _ => usage("skills [accept|reject <domain/name> <sha>]"),
        },
        "tools" => match &words[1..] {
            [] => Ok(OwnerCmd::Skills),
            [d, name, sha] if decision(d).is_some() => {
                Ok(OwnerCmd::Tool { name: name.trim_start_matches("made_").to_string(), decision: decision(d).unwrap_or("reject"), sha: sha.to_string() })
            }
            _ => usage("tools [accept|reject <name> <sha>]"),
        },
        "allow" => match &words[1..] {
            [] => Ok(OwnerCmd::Allow { yes: false }),
            [v] if v.eq_ignore_ascii_case("yes") => Ok(OwnerCmd::Allow { yes: true }),
            _ => usage("allow [yes]"),
        },
        "trust" => match words.get(1).map(|w| w.to_ascii_lowercase()).as_deref() {
            None if words.len() == 1 => Ok(OwnerCmd::Trust { yes: false }),
            Some("yes") if words.len() == 2 => Ok(OwnerCmd::Trust { yes: true }),
            _ => usage("trust [yes]"),
        },
        _ => return None,
    };
    Some(parsed)
}

/// Run `cmd` against the kernel for the chat's `session` (if it has one yet); the result as
/// Markdown for the chat.
pub async fn run(c: &Client, cmd: &OwnerCmd, session: Option<&str>, chat: &mut Chat) -> Result<String> {
    Ok(match cmd {
        OwnerCmd::Sources => {
            let v = c.get("/api/sources").await?;
            let lines: Vec<String> = v.as_array().into_iter().flatten()
                .map(|r| format!("- `{}` — risk {}: {}", r["source"].as_str().unwrap_or(""), r["risk"], r["note"].as_str().unwrap_or(""))).collect();
            if lines.is_empty() { "No allowlisted sources.".into() } else { lines.join("\n") }
        }
        OwnerCmd::SourceAdd { source, risk, note } => {
            c.post("/api/sources", json!({"source":source,"risk":risk,"note":note})).await?;
            format!("Allowlisted `{source}` at risk {risk}.")
        }
        OwnerCmd::SourceRm(source) => {
            c.post("/api/sources", json!({"source":source,"remove":true})).await?;
            format!("Removed `{source}`.")
        }
        OwnerCmd::Jobs => {
            let r = c.get("/api/jobs").await?;
            let mut out: Vec<String> = r["jobs"].as_array().into_iter().flatten().map(job_line).collect();
            if out.is_empty() {
                out.push("No jobs.".into());
            }
            if r["scheduler"] == false {
                out.insert(0, "The scheduler is off in this kernel (ZEN_JOBS=0).".into());
            }
            out.join("\n")
        }
        OwnerCmd::Run(name) => {
            let r = c.post(&format!("/api/jobs/{}/run", enc(name)), json!({})).await?;
            format!("Started run {} of `{name}`: its report arrives when it ends (`jobs runs {name}`).", r["run"])
        }
        OwnerCmd::Resume(name) => job_line(&c.patch(&format!("/api/jobs/{}", enc(name)), json!({ "enabled": true })).await?),
        OwnerCmd::Pause(name) => job_line(&c.patch(&format!("/api/jobs/{}", enc(name)), json!({ "enabled": false })).await?),
        OwnerCmd::Runs(name) => {
            let q = name.as_deref().map(|n| format!("&job={}", enc(n))).unwrap_or_default();
            let r = c.get(&format!("/api/jobs/runs?limit=5{q}")).await?;
            let runs = r.as_array().cloned().unwrap_or_default();
            if runs.is_empty() { "No runs yet.".into() } else { runs.iter().map(run_block).collect::<Vec<_>>().join("\n\n") }
        }
        OwnerCmd::Skills => skills_list(&c.get("/api/skills").await?),
        OwnerCmd::Skill { name, decision, sha } => {
            let r = c.post("/api/skills/review", json!({ "name": name, "decision": decision, "sha": sha })).await?;
            r["result"].as_str().unwrap_or("done").to_string()
        }
        OwnerCmd::Tool { name, decision, sha } => {
            let r = c.post(&format!("/api/tools/{}/review", enc(name)), json!({ "decision": decision, "sha": sha })).await?;
            r["result"].as_str().unwrap_or("done").to_string()
        }
        OwnerCmd::Allow { yes } => {
            let Some(id) = session else { return Ok("No session here yet.".into()) };
            let p = chat.prefix;
            if !yes {
                let r = c.get(&format!("/api/sessions/{}/allow", enc(id))).await?;
                chat.actions.remove(id);
                let Some(a) = r["actions"].as_array().and_then(|v| v.first()) else {
                    return Ok("No held actions waiting.".into());
                };
                let gate = a["id"].as_i64().unwrap_or(0);
                chat.actions.insert(id.to_string(), gate);
                return Ok(format!("**Held action #{gate}**: {}\n\n```json\n{}\n```\n\nThis allows the exact workspace, tool and arguments once, for ten minutes. It does not trust the session. Review any scripts/files it will run too.\n\n`{p}allow yes` to allow it.",
                    a["reason"].as_str().unwrap_or(""), a["action"].as_str().unwrap_or("")));
            }
            let Some(gate) = chat.actions.remove(id) else { return Ok(format!("Look first: `{p}allow`.")) };
            let r = c.post(&format!("/api/sessions/{}/allow", enc(id)), json!({"id":gate})).await?;
            if r["allowed"] == true { "Allowed once: ask the agent to retry unchanged.".into() }
            else { "That action is no longer waiting: look again.".into() }
        }
        OwnerCmd::Trust { yes } => {
            let Some(id) = session else { return Ok("No session here yet: nothing to trust.".into()) };
            let p = chat.prefix;
            if !yes {
                let r = c.get(&format!("/api/sessions/{}/taint", enc(id))).await?;
                if r["tainted_at"].is_null() && r["risky_at"].is_null() {
                    chat.looked.remove(id);
                    return Ok("This session hasn't read untrusted content: nothing to trust.".into());
                }
                chat.looked.insert(id.to_string(), r["checked"].as_i64().unwrap_or(0));
                return Ok(taint_report(&r, p));
            }
            let Some(seen) = chat.looked.get(id).copied() else {
                return Ok(format!("Look first: `{p}trust` shows what made this session untrusted."));
            };
            let r = c.post(&format!("/api/sessions/{}/trust", enc(id)), json!({ "seen": seen })).await;
            chat.looked.remove(id);
            let r = r?;
            match r["seq"].as_i64() {
                Some(seq) => format!("Trusted from #{seq} on: the risky and tainted marks are lifted. The next outside read makes it risky again."),
                None => r["result"].as_str().unwrap_or("done").to_string(),
            }
        }
    })
}

/// What made a session untrusted (`GET /api/sessions/{id}/taint`), and what trusting it means.
pub fn taint_report(r: &Value, prefix: &str) -> String {
    let cut = |v: &Value| {
        let s: String = v.as_str().unwrap_or("").chars().take(100).collect();
        if s.is_empty() { String::new() } else { format!(": {s}") }
    };
    let mut out = vec![
        format!(
            "**This session read untrusted content** (since {} UTC), so it runs limited: its shell has no network, \
             and it can't change the prompt files, skills or tools, vouch for a memory or start a job.",
            minute(if r["tainted_at"].is_null() { &r["risky_at"] } else { &r["tainted_at"] })
        ),
        String::new(),
    ];
    if r["tainted_at"].is_null() {
        out[0] = "**This session is risky, not tainted.** It read outside content. Safe reads run normally; programmable work runs sandboxed; dangerous actions wait for approval.".into();
    }
    let taints = r["taints"].as_array().cloned().unwrap_or_default();
    if !taints.is_empty() {
        out.push("What marked it:".into());
        for t in &taints {
            out.push(format!("- #{} {}{}", t["seq"], t["source"].as_str().unwrap_or("?"), cut(&t["about"])));
        }
    }
    let sources = r["sources"].as_array().cloned().unwrap_or_default();
    if !sources.is_empty() {
        out.push("What it was handed as untrusted:".into());
        for x in &sources {
            let n = x["count"].as_i64().unwrap_or(1);
            let times = if n > 1 { format!(" ({n}×)") } else { String::new() };
            out.push(format!("- #{} {}{times}{}", x["first"], x["source"].as_str().unwrap_or("?"), cut(&x["about"])));
        }
    }
    out.push(String::new());
    out.push(
        "Trusting it lifts the limits from now on. What it read stays in its context, so trust it only if you trust that; \
         the next outside read makes it risky again, and what it saved meanwhile stays marked."
            .into(),
    );
    if r["busy"] == true {
        out.push("A turn is running: wait for it to end, then look again.".into());
    } else {
        out.push(format!("`{prefix}trust yes` to trust it."));
    }
    out.join("\n")
}

/// A UTC timestamp's date and minutes: "2026-10-13 09:30".
fn minute(v: &Value) -> String {
    v.as_str().map(|t| t.get(..16).unwrap_or(t).replace('T', " ")).unwrap_or_default()
}

/// One job as a list item: name, schedule, and when it runs next or why it's paused.
pub fn job_line(j: &Value) -> String {
    let state = if j["enabled"] == true {
        match j["next_run_at"].as_str() {
            Some(_) => format!("next {} UTC", minute(&j["next_run_at"])),
            None => "nothing ahead".into(),
        }
    } else {
        format!("paused: {}", j["paused_reason"].as_str().unwrap_or("paused"))
    };
    let last = j["last_status"].as_str().map(|s| format!("; last {s} {} UTC", minute(&j["last_run_at"]))).unwrap_or_default();
    format!(
        "- `{}` {} ({}): {state}{last}",
        j["name"].as_str().unwrap_or(""),
        j["schedule"].as_str().unwrap_or(""),
        j["tz"].as_str().unwrap_or("")
    )
}

/// One run: its header, then the start of its report or error.
fn run_block(x: &Value) -> String {
    let mut s = format!(
        "**{}** {} UTC: {} ({})",
        x["job"].as_str().unwrap_or(""),
        minute(&x["started_at"]),
        x["status"].as_str().unwrap_or(""),
        x["trigger"].as_str().unwrap_or("")
    );
    if let Some(body) = x["output"].as_str().or(x["error"].as_str()) {
        for line in body.lines().take(12) {
            s.push_str(&format!("\n> {line}"));
        }
    }
    s
}

/// Skills (a draft with the fingerprint to review it) and made tools.
pub fn skills_list(s: &Value) -> String {
    let mut out = Vec::new();
    for x in s["skills"].as_array().into_iter().flatten() {
        let draft = if x["status"] == "draft" {
            let mark = if x["untrusted"] == true { ", untrusted" } else { "" };
            format!(" (draft {}{mark})", sha12(x))
        } else {
            String::new()
        };
        out.push(format!("- `{}`{draft}", x["skill"].as_str().unwrap_or("")));
    }
    for t in s["tools"].as_array().into_iter().flatten() {
        let state = if t["approved"] == true { "approved" } else { "sandboxed until approved" };
        out.push(format!("- tool `made_{}` {}: {state}", t["tool"].as_str().unwrap_or(""), sha12(t)));
    }
    if out.is_empty() { "No skills or tools.".into() } else { out.join("\n") }
}

/// The fingerprint as `zen skills` shows it (12 characters), typed back to review.
fn sha12(v: &Value) -> String {
    v["sha"].as_str().map(|s| s[..12.min(s.len())].to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_commands_and_leaves_anything_else_alone() {
        assert_eq!(parse("jobs"), Some(Ok(OwnerCmd::Jobs)));
        assert_eq!(parse("JOBS list"), Some(Ok(OwnerCmd::Jobs)));
        assert_eq!(parse("jobs run daily-readings"), Some(Ok(OwnerCmd::Run("daily-readings".into()))));
        assert_eq!(parse("jobs  Resume  daily-readings "), Some(Ok(OwnerCmd::Resume("daily-readings".into()))));
        assert_eq!(parse("jobs pause x"), Some(Ok(OwnerCmd::Pause("x".into()))));
        assert_eq!(parse("jobs runs"), Some(Ok(OwnerCmd::Runs(None))));
        assert_eq!(parse("jobs runs x"), Some(Ok(OwnerCmd::Runs(Some("x".into())))));
        assert_eq!(parse("sources"), Some(Ok(OwnerCmd::Sources)));
        assert_eq!(parse("sources add x.com/paulg 3 public comments"), Some(Ok(OwnerCmd::SourceAdd { source: "x.com/paulg".into(), risk: 3, note: "public comments".into() })));
        assert!(parse("sources add x.com 0").unwrap().is_err());
        assert_eq!(parse("sources rm x.com"), Some(Ok(OwnerCmd::SourceRm("x.com".into()))));
        assert_eq!(parse("skills"), Some(Ok(OwnerCmd::Skills)));
        assert_eq!(parse("tools"), Some(Ok(OwnerCmd::Skills)));
        assert_eq!(
            parse("skills accept work/brief abc123def456"),
            Some(Ok(OwnerCmd::Skill { name: "work/brief".into(), decision: "accept", sha: "abc123def456".into() }))
        );
        assert_eq!(parse("tools reject made_fetch 0123abcd"), Some(Ok(OwnerCmd::Tool { name: "fetch".into(), decision: "reject", sha: "0123abcd".into() })));
        assert_eq!(parse("allow"), Some(Ok(OwnerCmd::Allow { yes: false })));
        assert_eq!(parse("ALLOW YES"), Some(Ok(OwnerCmd::Allow { yes: true })));
        assert!(matches!(parse("allow everything"), Some(Err(_))));
        assert_eq!(parse("trust"), Some(Ok(OwnerCmd::Trust { yes: false })));
        assert_eq!(parse("Trust YES"), Some(Ok(OwnerCmd::Trust { yes: true })));
        assert_eq!(parse("hello"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn a_wrong_shape_is_usage_never_a_guess() {
        assert!(matches!(parse("jobs run"), Some(Err(u)) if u.starts_with("usage: jobs")));
        assert!(matches!(parse("jobs run a b"), Some(Err(_))));
        assert!(matches!(parse("jobs delete x"), Some(Err(_))), "removing a job stays in the terminal (`zen jobs rm`)");
        assert!(matches!(parse("skills accept work/brief"), Some(Err(_))), "a review always names the fingerprint reviewed");
        assert!(matches!(parse("tools approve x abc"), Some(Err(_))));
        assert!(matches!(parse("trust me"), Some(Err(u)) if u == "usage: trust [yes]"));
        assert!(matches!(parse("trust yes please"), Some(Err(_))));
    }

    #[test]
    fn lines_for_the_chat() {
        let j = json!({ "name": "daily-readings", "schedule": "30 6 * * *", "tz": "America/Sao_Paulo", "enabled": false, "paused_reason": "awaiting the owner's approval" });
        assert_eq!(job_line(&j), "- `daily-readings` 30 6 * * * (America/Sao_Paulo): paused: awaiting the owner's approval");
        let on = json!({ "name": "n", "schedule": "every 2h", "tz": "UTC", "enabled": true, "next_run_at": "2026-10-14T09:30:00Z", "last_status": "ok", "last_run_at": "2026-10-13T09:30:05Z" });
        assert_eq!(job_line(&on), "- `n` every 2h (UTC): next 2026-10-14 09:30 UTC; last ok 2026-10-13 09:30 UTC");
        let s = json!({ "skills": [{ "skill": "work/brief", "status": "active" }, { "skill": "ops/x", "status": "draft", "sha": "abcdef0123456789", "untrusted": true }],
            "tools": [{ "tool": "fetch", "approved": false, "sha": "0011223344556677" }] });
        assert_eq!(skills_list(&s), "- `work/brief`\n- `ops/x` (draft abcdef012345, untrusted)\n- tool `made_fetch` 001122334455: sandboxed until approved");
        let r = json!({ "job": "n", "started_at": "2026-10-13T09:30:00Z", "status": "ok", "trigger": "manual", "output": "line 1\nline 2" });
        assert_eq!(run_block(&r), "**n** 2026-10-13 09:30 UTC: ok (manual)\n> line 1\n> line 2");
    }

    #[test]
    fn the_taint_report_says_what_and_what_trusting_means() {
        let r = json!({ "tainted_at": "2026-10-13T09:30:12Z", "checked": 40, "busy": false,
            "taints": [{ "seq": 12, "source": "web_search", "about": "harbour" }],
            "sources": [{ "first": 12, "source": "web", "about": "harbour", "count": 3 }, { "first": 30, "source": "subagent", "about": "", "count": 1 }] });
        let t = taint_report(&r, "!");
        assert!(t.starts_with("**This session read untrusted content** (since 2026-10-13 09:30 UTC)"), "{t}");
        assert!(t.contains("What marked it:\n- #12 web_search: harbour\n"), "{t}");
        assert!(t.contains("- #12 web (3×): harbour\n- #30 subagent\n"), "{t}");
        assert!(t.ends_with("`!trust yes` to trust it."), "{t}");
        let busy = taint_report(&json!({ "tainted_at": "2026-10-13T09:30:12Z", "busy": true }), "/");
        assert!(busy.ends_with("wait for it to end, then look again.") && !busy.contains("trust yes"), "{busy}");
    }

    #[tokio::test]
    async fn trust_yes_needs_a_look_first() {
        let c = Client::new("http://127.0.0.1:9", Some("t".into())).unwrap();
        let mut chat = Chat::new("/");
        let out = run(&c, &OwnerCmd::Trust { yes: true }, Some("s1"), &mut chat).await.unwrap();
        assert_eq!(out, "Look first: `/trust` shows what made this session untrusted.");
        let out = run(&c, &OwnerCmd::Trust { yes: false }, None, &mut chat).await.unwrap();
        assert!(out.starts_with("No session here yet"), "{out}");
    }
}
