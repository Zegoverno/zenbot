//! The owner's commands, the same everywhere: `/queue` in the `zen` terminal app, `!queue` in
//! Matrix, `zen queue` on the command line. Four answer what zenbot holds for the owner: `queue`
//! (what waits), `approve [N]`, `deny [N]` (no number: everything waiting) and `trust [add|rm]
//! <url|repo>` (say yes in advance); `jobs`, `skills` and `tools` manage the rest. They run with the
//! owner's token, so they can approve what only the owner may.
//!
//! The client reads them before anything reaches the agent and calls the kernel itself: the agent
//! never sees or relays them, so text it read can't approve anything. Parsing is pure and tested;
//! [`run`] makes the calls and returns Markdown for the chat.

use anyhow::Result;
use serde_json::{json, Value};

use crate::{enc, Client};

/// What to do with an MCP sign-in the kernel started (`POST /api/mcp/login`): the sign-in
/// server's host first (check it's the service you expect), then the link, then what to paste.
pub fn login_instructions(start: &Value) -> String {
    format!(
        "Sign-in at {host}\n\n{url}\n\nOpen the link in your browser and approve. Your browser then fails to load {redirect}: that's expected.\nCopy that page's whole address and paste it here.",
        host = start["auth_server"].as_str().unwrap_or("?"),
        url = start["authorize_url"].as_str().unwrap_or(""),
        redirect = start["redirect"].as_str().unwrap_or("127.0.0.1"),
    )
}

/// The kernel's answer to a finished sign-in.
pub fn login_done(done: &Value) -> String {
    let server = done["server"].as_str().unwrap_or("");
    if done["renews"] == true {
        format!("Signed in to `{server}`; zenbot renews the sign-in itself.")
    } else {
        format!("Signed in to `{server}`. The server gave no way to renew it: sign in again when it expires (/mcp login {server}).")
    }
}

/// One owner command.
#[derive(Debug, PartialEq)]
pub enum OwnerCmd {
    /// `queue`: everything waiting for the owner, numbered.
    Queue,
    /// `approve [N]`: run one held item once (or every one).
    Approve(Option<Item>),
    /// `deny [N]`: refuse one held item for good (or every one).
    Deny(Option<Item>),
    /// `trust`: the trust list.
    TrustList,
    /// `trust add [url|api|mcp|repo] <match> [trusted|known|partly-open|blocked]`.
    TrustAdd { kind: String, pattern: String, level: String },
    /// `trust rm [url|api|mcp|repo] <match>`.
    TrustRm { kind: String, pattern: String },
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
}

/// A queue item as the owner types it: a held action by its number (`12`), a source proposal
/// with an `s` (`s7`).
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Item {
    Action(i64),
    Source(i64),
}

impl Item {
    fn parse(w: &str) -> Option<Item> {
        match w.strip_prefix(['s', 'S']) {
            Some(n) => n.parse().ok().map(Item::Source),
            None => w.trim_start_matches('#').parse().ok().map(Item::Action),
        }
    }
    pub fn of(v: &Value) -> Option<Item> {
        let id = v["id"].as_i64()?;
        Some(if v["kind"] == "source" { Item::Source(id) } else { Item::Action(id) })
    }
    pub fn label(self) -> String {
        match self {
            Item::Action(n) => n.to_string(),
            Item::Source(n) => format!("s{n}"),
        }
    }
    fn path(self) -> String {
        match self {
            Item::Action(n) => format!("/api/queue/action/{n}"),
            Item::Source(n) => format!("/api/queue/source/{n}"),
        }
    }
}

/// What a chat client keeps between owner commands: the prefix it reads them by (`/`, `!` or `zen `).
#[derive(Debug, Default)]
pub struct Chat {
    pub prefix: &'static str,
}

impl Chat {
    pub fn new(prefix: &'static str) -> Self {
        Chat { prefix }
    }
}

/// How to use the commands, one line each, without the prefix the client adds.
pub const USAGE: &[(&str, &str)] = &[
    ("queue", "everything waiting for you, numbered"),
    ("approve [N]", "run held item N once; no number: everything waiting"),
    ("deny [N]", "refuse held item N for good; no number: everything waiting"),
    ("trust [add|rm <url|repo> [level]]", "the trust list; add or remove a host, host/path or repo (github.com/owner/*)"),
    ("jobs", "every job, its schedule and state"),
    ("jobs run|resume|pause <name>", "run a job now, turn it on (approves one zenbot made), pause it"),
    ("jobs runs [name]", "recent runs and their reports"),
    ("skills", "skills and made tools, with the fingerprint to review a draft"),
    ("skills accept|reject <domain/name> <sha>", "activate or archive a draft skill"),
    ("tools accept|reject <name> <sha>", "approve a made tool (network, no sandbox) or reject it"),
];

const KINDS: &[&str] = &["url", "api", "mcp", "repo"];
const LEVELS: &[&str] = &["trusted", "known", "partly-open", "blocked"];

/// `host/owner/name` on a git host is a repo; anything else is a URL (host or host/path).
fn kind_of(pattern: &str) -> &'static str {
    let parts: Vec<&str> = pattern.split('/').collect();
    let git = matches!(parts[0], "github.com" | "gitlab.com" | "codeberg.org" | "bitbucket.org");
    if git && parts.len() == 3 { "repo" } else { "url" }
}

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
    let item = |rest: &[&str]| match rest {
        [] => Some(None),
        [w] => Item::parse(w).map(Some),
        _ => None,
    };
    let parsed = match first.as_str() {
        "queue" => match &words[1..] {
            [] => Ok(OwnerCmd::Queue),
            _ => usage("queue"),
        },
        "approve" => item(&words[1..]).map(OwnerCmd::Approve).ok_or_else(|| "usage: approve [N]".to_string()),
        "deny" => item(&words[1..]).map(OwnerCmd::Deny).ok_or_else(|| "usage: deny [N]".to_string()),
        "trust" => {
            let lower: Vec<String> = words[1..].iter().map(|w| w.to_ascii_lowercase()).collect();
            let rest: Vec<&str> = lower.iter().map(String::as_str).collect();
            let split = |args: &[&str], orig: &[&str]| -> Option<(String, String, Vec<String>)> {
                match args.first() {
                    Some(k) if KINDS.contains(k) && args.len() >= 2 => Some((k.to_string(), orig[1].to_string(), args[2..].iter().map(|s| s.to_string()).collect())),
                    Some(_) => Some((kind_of(orig[0]).to_string(), orig[0].to_string(), args[1..].iter().map(|s| s.to_string()).collect())),
                    None => None,
                }
            };
            match rest.first().copied() {
                None | Some("list") if rest.len() <= 1 => Ok(OwnerCmd::TrustList),
                Some("add") => match split(&rest[1..], &words[2..]) {
                    Some((kind, pattern, more)) if more.is_empty() => Ok(OwnerCmd::TrustAdd { kind, pattern, level: "trusted".into() }),
                    Some((kind, pattern, more)) if more.len() == 1 && LEVELS.contains(&more[0].as_str()) => Ok(OwnerCmd::TrustAdd { kind, pattern, level: more[0].clone() }),
                    _ => usage("trust add <url|repo> [trusted|known|partly-open|blocked]"),
                },
                Some("rm") => match split(&rest[1..], &words[2..]) {
                    Some((kind, pattern, more)) if more.is_empty() => Ok(OwnerCmd::TrustRm { kind, pattern }),
                    _ => usage("trust rm <url|repo>"),
                },
                _ => usage("trust [add|rm <url|repo> [level]]"),
            }
        }
        "jobs" => match (words.get(1).map(|w| w.to_ascii_lowercase()).as_deref(), &words[2.min(words.len())..]) {
            (None | Some("list"), []) => Ok(OwnerCmd::Jobs),
            (Some("run"), [n]) => Ok(OwnerCmd::Run(n.to_string())),
            (Some("resume"), [n]) => Ok(OwnerCmd::Resume(n.to_string())),
            (Some("pause"), [n]) => Ok(OwnerCmd::Pause(n.to_string())),
            (Some("runs"), []) => Ok(OwnerCmd::Runs(None)),
            (Some("runs"), [n]) => Ok(OwnerCmd::Runs(Some(n.to_string()))),
            _ => usage("jobs [run|resume|pause <name> | runs [name]]"),
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
        _ => return None,
    };
    Some(parsed)
}

/// One queue item for the chat: its number, where it's from, what it wants and why it waits.
pub fn queue_line(v: &Value) -> String {
    let n = Item::of(v).map(Item::label).unwrap_or_default();
    let title = v["title"].as_str().unwrap_or(if v["kind"] == "source" { "source rating" } else { "" });
    format!("**{n}** · {title}
{}
{}", short(v["summary"].as_str().unwrap_or(""), 300), v["reason"].as_str().unwrap_or(""))
}

/// Approve or deny `which` (None: everything waiting); one line per item.
async fn answer(c: &Client, which: Option<Item>, approve: bool) -> Result<String> {
    let items: Vec<Item> = match which {
        Some(i) => vec![i],
        None => c.get("/api/queue").await?.as_array().into_iter().flatten().filter_map(Item::of).collect(),
    };
    if items.is_empty() {
        return Ok("Nothing waiting.".into());
    }
    let (verb, decision) = if approve { ("approved", "approve") } else { ("denied", "reject") };
    let mut out = Vec::new();
    for i in items {
        out.push(match c.post(&i.path(), json!({ "decision": decision })).await {
            Ok(_) => format!("{} {verb}", i.label()),
            Err(e) => format!("{}: {e:#}", i.label()),
        });
    }
    Ok(out.join("\n"))
}

/// Run `cmd` against the kernel for the chat's `session` (if it has one yet); the result as
/// Markdown for the chat.
pub async fn run(c: &Client, cmd: &OwnerCmd, _session: Option<&str>, chat: &mut Chat) -> Result<String> {
    Ok(match cmd {
        OwnerCmd::Queue => {
            let v = c.get("/api/queue").await?;
            let lines: Vec<String> = v.as_array().into_iter().flatten().map(queue_line).collect();
            let p = chat.prefix;
            if lines.is_empty() { "Nothing waiting.".into() } else { format!("{}\n\n`{p}approve N` · `{p}deny N` · no number: all", lines.join("\n\n")) }
        }
        OwnerCmd::Approve(which) => answer(c, *which, true).await?,
        OwnerCmd::Deny(which) => answer(c, *which, false).await?,
        OwnerCmd::TrustList => {
            let v = c.get("/api/trust").await?;
            let mut out: Vec<String> = v["entries"].as_array().into_iter().flatten()
                .map(|e| format!("- {} `{}`: {}{}", e["kind"].as_str().unwrap_or(""), e["match"].as_str().unwrap_or(""), e["level"].as_str().unwrap_or(""),
                    e["note"].as_str().filter(|n| !n.is_empty()).map(|n| format!(" ({n})")).unwrap_or_default())).collect();
            if out.is_empty() { out.push("Nothing on the trust list.".into()); }
            if let Some(p) = v["problem"].as_str() { out.push(format!("problem: {p}")); }
            out.join("\n")
        }
        OwnerCmd::TrustAdd { kind, pattern, level } => {
            c.post("/api/trust", json!({ "kind": kind, "match": pattern, "level": level })).await?;
            format!("Trust list: {kind} `{pattern}` is {level}.")
        }
        OwnerCmd::TrustRm { kind, pattern } => {
            c.post("/api/trust", json!({ "kind": kind, "match": pattern, "remove": true })).await?;
            format!("Trust list: removed {kind} `{pattern}`.")
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
    })
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
    // A script job's exact command: what the owner approves when resuming one zenbot wrote.
    let command = j["command"].as_str().map(|c| format!("\n  `$ {c}`")).unwrap_or_default();
    format!(
        "- `{}` {} ({}): {state}{last}{command}",
        j["name"].as_str().unwrap_or(""),
        j["schedule"].as_str().unwrap_or(""),
        j["tz"].as_str().unwrap_or("")
    )
}

/// One run: its header, then the start of its report or error.
/// The first `max` characters of `s`.
fn short(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn run_block(x: &Value) -> String {
    let mut s = format!(
        "**{}** {} UTC: {} ({})",
        x["job"].as_str().unwrap_or(""),
        minute(&x["started_at"]),
        x["status"].as_str().unwrap_or(""),
        x["trigger"].as_str().unwrap_or("")
    );
    // A script job's run: the exact command it ran and how it exited.
    if let Some(c) = x["command"].as_str() {
        let exit = x["exit_code"].as_i64().map(|e| format!(" (exit {e})")).unwrap_or_default();
        s.push_str(&format!("\n`$ {}`{exit}", short(c, 200)));
    }
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
        assert_eq!(parse("queue"), Some(Ok(OwnerCmd::Queue)));
        assert_eq!(parse("approve"), Some(Ok(OwnerCmd::Approve(None))));
        assert_eq!(parse("approve 12"), Some(Ok(OwnerCmd::Approve(Some(Item::Action(12))))));
        assert_eq!(parse("APPROVE #12"), Some(Ok(OwnerCmd::Approve(Some(Item::Action(12))))));
        assert_eq!(parse("deny s7"), Some(Ok(OwnerCmd::Deny(Some(Item::Source(7))))));
        assert_eq!(parse("deny"), Some(Ok(OwnerCmd::Deny(None))));
        assert!(parse("approve nope").unwrap().is_err());
        assert!(parse("approve 1 2").unwrap().is_err());
        assert_eq!(parse("trust"), Some(Ok(OwnerCmd::TrustList)));
        assert_eq!(parse("trust add docs.rs"), Some(Ok(OwnerCmd::TrustAdd { kind: "url".into(), pattern: "docs.rs".into(), level: "trusted".into() })));
        assert_eq!(parse("trust add x.com/paulg known"), Some(Ok(OwnerCmd::TrustAdd { kind: "url".into(), pattern: "x.com/paulg".into(), level: "known".into() })));
        assert_eq!(parse("trust add github.com/Zegoverno/*"), Some(Ok(OwnerCmd::TrustAdd { kind: "repo".into(), pattern: "github.com/Zegoverno/*".into(), level: "trusted".into() })));
        assert_eq!(parse("trust add mcp linear"), Some(Ok(OwnerCmd::TrustAdd { kind: "mcp".into(), pattern: "linear".into(), level: "trusted".into() })));
        assert_eq!(parse("trust rm github.com/a/b"), Some(Ok(OwnerCmd::TrustRm { kind: "repo".into(), pattern: "github.com/a/b".into() })));
        assert!(parse("trust add docs.rs maybe").unwrap().is_err());
        assert!(parse("trust yes").unwrap().is_err());
        assert_eq!(parse("jobs"), Some(Ok(OwnerCmd::Jobs)));
        assert_eq!(parse("JOBS list"), Some(Ok(OwnerCmd::Jobs)));
        assert_eq!(parse("jobs run daily-readings"), Some(Ok(OwnerCmd::Run("daily-readings".into()))));
        assert_eq!(parse("jobs  Resume  daily-readings "), Some(Ok(OwnerCmd::Resume("daily-readings".into()))));
        assert_eq!(parse("jobs pause x"), Some(Ok(OwnerCmd::Pause("x".into()))));
        assert_eq!(parse("jobs runs"), Some(Ok(OwnerCmd::Runs(None))));
        assert_eq!(parse("jobs runs x"), Some(Ok(OwnerCmd::Runs(Some("x".into())))));
        assert_eq!(parse("skills"), Some(Ok(OwnerCmd::Skills)));
        assert_eq!(parse("tools"), Some(Ok(OwnerCmd::Skills)));
        assert_eq!(
            parse("skills accept work/brief abc123def456"),
            Some(Ok(OwnerCmd::Skill { name: "work/brief".into(), decision: "accept", sha: "abc123def456".into() }))
        );
        assert_eq!(parse("tools reject made_fetch 0123abcd"), Some(Ok(OwnerCmd::Tool { name: "fetch".into(), decision: "reject", sha: "0123abcd".into() })));
        for gone in ["allow", "sources"] { assert_eq!(parse(gone), None, "{gone} is gone"); }
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
        assert!(matches!(parse("queue approve action 1"), Some(Err(_))));
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
    fn queue_lines_number_actions_and_sources() {
        let a = json!({"kind":"action","id":12,"title":"ship it","summary":"git push","reason":"held: off-job"});
        assert_eq!(queue_line(&a), "**12** · ship it\ngit push\nheld: off-job");
        let s = json!({"kind":"source","id":7,"summary":"x.com/paulg","reason":"rating"});
        assert!(queue_line(&s).starts_with("**s7** · source rating"));
    }
}
