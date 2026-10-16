//! The workshop: the agent improves its own skills and makes its own tools, without sprawl
//! (DESIGN.md, "Skills and tools that improve themselves"; D-029, D-037; Phase 5).
//!
//! Hermes sprawls because creating a skill is cheaper than finding and improving one, a fork after
//! busy turns is told to save something, and nothing is ever retired (docs/archive/research/
//! hermes-openclaw.md). Here, `save_skill` is the way to create or change a skill and the kernel
//! enforces the rules:
//! - the agentskills.io format, a SKILL.md under 10,000 characters, and a reason (the evidence);
//! - edit before create: a new skill that does the same job as one in its domain is refused with
//!   "extend X instead" (System One judges; without it, word overlap);
//! - a new skill is a draft (`skills/_proposed/<domain>/<name>`): found and loaded marked as a draft,
//!   active once the owner accepts it (`zen skills accept`) or a session that loaded it gets the
//!   owner's `accept` verdict; a new domain always needs the owner; a draft a session that read
//!   untrusted content wrote (by `save_skill` or `edit`/`write`) is marked (`draft_taint`), read
//!   as untrusted content, and activated only by the owner's own accept (D-053);
//! - changes to an active skill apply at once; every change is a commit in the skills repository;
//! - the nightly sleep flags skills unused for 30 days and archives (never deletes) those unused
//!   for 90 (`skills/_archived`).
//!
//! `save_tool` makes a tool: a manifest (name, description, JSON-schema parameters, command) and its
//! files in `<zen home>/global/tools/<name>/`. It is found with `find_tools` (as `made_<name>`) and run with
//! `call_tool`: the arguments arrive as JSON on stdin. Until the owner approves it (`made_tools`, in
//! the database, out of the agent's reach) it runs in bubblewrap with no network and a read-only
//! filesystem; approved, it runs with the network. It never gets the kernel's environment (tokens,
//! keys), only the basics.
//!
//! The owner's decision covers what they reviewed: `zen skills` shows each draft's and tool's
//! fingerprint, and accepting or rejecting one sends it back; the kernel refuses when the content
//! changed since. A tool runs from a fresh copy of its folder, fingerprinted as it runs, so it
//! can't change between the check and the run, and what it writes (a `__pycache__`) never lands in
//! its folder and never costs it its approval.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::skills::{self, Skill};
use crate::{tools, App};

const MAX_SKILL: usize = 10_000;

fn proposed_root() -> PathBuf {
    skills::root().join("_proposed")
}

fn archived_root() -> PathBuf {
    skills::root().join("_archived")
}

/// Where the agent's tools live: ZEN_TOOLS_DIR, else `~/.zenbot/global/tools` (shared by every agent).
pub fn tools_root() -> PathBuf {
    std::env::var("ZEN_TOOLS_DIR").map(PathBuf::from).unwrap_or_else(|_| crate::layout::global_dir().join("tools"))
}

/// A skill's, domain's or tool's name (agentskills.io: at most 64 characters).
fn valid_name(n: &str) -> bool {
    crate::valid_name(n, 64)
}

fn words(t: &str) -> std::collections::BTreeSet<String> {
    t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 4).map(String::from).collect()
}

/// Word overlap (Jaccard) between two texts: the fallback duplicate check without System One.
pub fn overlap(a: &str, b: &str) -> f64 {
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(&b).count() as f64 / a.union(&b).count() as f64
}

/// The existing skill in the domain a new one would duplicate, if any.
async fn duplicate_of(app: &App, domain: &str, name: &str, description: &str, body: &str, existing: &[Skill]) -> Option<String> {
    let same: Vec<&Skill> = existing.iter().filter(|s| s.domain == domain && s.name != name).collect();
    if same.is_empty() {
        return None;
    }
    if crate::score::scorer().is_some() && crate::score::private_ok() {
        let mut criteria = serde_json::Map::new();
        for s in &same {
            criteria.insert(s.name.clone(), json!(s.description));
        }
        criteria.insert("none".into(), json!("None of them: the new skill does a different job"));
        let questions = json!({ "same": { "type": "choice",
            "instructions": "Does the new skill do the same job as one of the existing skills (so it should extend that one instead)?", "criteria": criteria } });
        let state = json!({ "new_skill": { "name": name, "description": description, "instructions": zen_proto::head(body, 3000) } });
        if let Ok(res) = crate::score::decide(app, &state, &questions).await {
            if res["error"].is_null() {
                let a = &res["answers"]["same"];
                let choice = a["choice"].as_str().unwrap_or("none").to_string();
                let p = a["probabilities"][&choice].as_f64().unwrap_or(0.0);
                crate::agent::log_decision(&app.db, None, "skill_duplicate", &json!({ "skill": format!("{domain}/{name}") }), a, Some(&choice), Some(p), true, None).await;
                return (choice != "none" && p >= 0.8).then_some(choice);
            }
        }
    }
    same.iter().find(|s| overlap(&format!("{} {}", s.name, s.description), &format!("{name} {description}")) >= 0.6).map(|s| s.name.clone())
}

fn skill_md(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {}\n---\n\n{}\n", description.replace('\n', " "), body.trim())
}

/// Write a skill's folder: SKILL.md and extra files (relative paths inside it only).
fn write_skill(dir: &Path, md: &str, files: &Value) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("SKILL.md"), md)?;
    for (rel, content) in files.as_object().into_iter().flatten() {
        let rel = Path::new(rel);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) || rel == Path::new("SKILL.md") {
            bail!("file `{}` must be a relative path inside the skill (and not SKILL.md)", rel.display());
        }
        let path = dir.join(rel);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&path, content.as_str().unwrap_or(""))?;
    }
    Ok(())
}

async fn save_skill(app: &App, session: Uuid, args: &Value) -> Result<String> {
    let s = |k: &str| args[k].as_str().map(str::trim).unwrap_or("").to_string();
    let (domain, name, description, body, reason) = (s("domain"), s("name"), s("description"), s("body"), s("reason"));
    if !valid_name(&domain) || !valid_name(&name) {
        bail!("domain and name: lowercase letters, digits and single hyphens (e.g. build/rust-release)");
    }
    if reason.is_empty() {
        bail!("give the `reason`: what happened that shows this skill is needed (the evidence), not just what it says");
    }
    if body.len() > MAX_SKILL {
        bail!("SKILL.md must stay under {MAX_SKILL} characters: move detail to files in references/ (the `files` argument)");
    }
    let md = skill_md(&name, &description, &body);
    let active = skills::scan(&skills::root());
    let drafts = skills::drafts();
    let root = skills::root();
    let current = active.iter().find(|x| x.domain == domain && x.name == name).cloned();
    let draft = drafts.iter().find(|x| x.domain == domain && x.name == name).cloned();
    // An active skill's description reaches every session's instructions: text from a web page must
    // not rewrite it without the owner. Drafts and new skills still go through review, and one a
    // tainted session writes is marked untrusted (`draft_taint`) before it's written.
    let tainted = crate::taint::limited(&app.db, session, crate::taint::Limit::Steer).await;
    if current.is_some() && tainted {
        bail!("this session read untrusted content, so it can't change an active skill; tell the owner what you'd change and why");
    }
    let (dir, what) = match (&current, &draft) {
        (Some(c), _) => (c.dir.clone(), "Updated the active skill"),
        (None, Some(d)) => (d.dir.clone(), "Updated the draft"),
        (None, None) => {
            let mut everything = active.clone();
            everything.extend(drafts.iter().cloned());
            if let Some(dup) = duplicate_of(app, &domain, &name, &description, &body, &everything).await {
                bail!("{domain}/{dup} already does this job: extend it instead (save_skill with name `{dup}` and its improved instructions), so skills get better instead of multiplying");
            }
            (proposed_root().join(&domain).join(&name), "Saved a new draft")
        }
    };
    // Validate in a scratch copy first, so a bad save never replaces a good skill.
    let scratch = std::env::temp_dir().join(format!("zen-skill-{}", Uuid::new_v4())).join(&name);
    let written = write_skill(&scratch, &md, &args["files"]);
    let errs = if written.is_ok() { skills::validate(&scratch) } else { Vec::new() };
    let _ = std::fs::remove_dir_all(scratch.parent().unwrap_or(&scratch));
    written?;
    if !errs.is_empty() {
        bail!("not saved: {}", errs.join("; "));
    }
    if tainted {
        mark_draft(&app.db, &format!("{domain}/{name}"), session).await.context("not saved: couldn't mark the draft as written by a session that read untrusted content")?;
    }
    crate::sources::record_bytes(&app.db,session,&dir.join("SKILL.md"),&md,None).await?;
    for (rel,content) in args["files"].as_object().into_iter().flatten() {
        crate::sources::record_bytes(&app.db,session,&dir.join(rel),content.as_str().unwrap_or(""),None).await?;
    }
    write_skill(&dir, &md, &args["files"])?;
    let mark = if tainted { "; it read untrusted content" } else { "" };
    crate::git::commit_all(&root, &format!("{what}: {domain}/{name} (session {}{mark})\n\n{reason}", &session.to_string()[..8])).await;
    let new_domain = current.is_none() && draft.is_none() && !active.iter().any(|x| x.domain == domain);
    let next = if current.is_some() {
        "It's in use from the next session that loads it.".to_string()
    } else if tainted {
        format!("This session read untrusted content, so the draft is marked untrusted: only the owner can make it active (zen skills accept {domain}/{name}). Say so in your report.")
    } else if new_domain {
        format!("`{domain}` is a new domain: the owner decides whether it becomes active (zen skills accept {domain}/{name}). Say so in your report.")
    } else {
        format!("It can be found and loaded as a draft now; it becomes active when the owner accepts it (zen skills accept {domain}/{name}) or a session that used it is accepted.")
    };
    Ok(format!("{what} {domain}/{name} ({}). {next}", dir.display()))
}

/// Make a draft skill active (or archive it). `by` is who decided; `reviewed` is the fingerprint
/// of the draft the owner reviewed (`zen skills`), refused when the draft changed since. None only
/// for the kernel's own decisions (`on_accept`).
pub async fn decide_draft(db: &sqlx::PgPool, name: &str, accept: bool, by: &str, reviewed: Option<&str>) -> Result<String> {
    let name = name.trim().trim_end_matches('/');
    let draft = skills::lookup(&skills::drafts(), name).map_err(|e| anyhow::anyhow!(e))?.clone();
    if let Some(r) = reviewed {
        same_content(&fingerprint(db, &draft.dir).await?, r)?;
    }
    let to = if accept { skills::root().join(&draft.domain).join(&draft.name) } else { archived_root().join(&draft.domain).join(&draft.name) };
    if to.exists() {
        bail!("{} already exists", to.display());
    }
    std::fs::create_dir_all(to.parent().context("no parent")?)?;
    std::fs::rename(&draft.dir, &to)?;
    // The owner's decision covers what they reviewed: the mark goes with the draft.
    let key = format!("{}/{}", draft.domain, draft.name);
    if let Err(e) = sqlx::query("DELETE FROM draft_taint WHERE skill = $1").bind(&key).execute(db).await {
        tracing::warn!("clearing {key}'s untrusted mark: {e:#}");
    }
    let verb = if accept { "Activated" } else { "Rejected" };
    crate::git::commit_all(&skills::root(), &format!("{verb} {}/{} ({by})", draft.domain, draft.name)).await;
    Ok(format!("{verb} {}/{}", draft.domain, draft.name))
}

/// Draft skills a session that read untrusted content wrote (domain/name). When that can't be
/// read, every draft counts (fail closed).
pub async fn tainted_drafts(db: &sqlx::PgPool) -> Vec<String> {
    match sqlx::query_scalar("SELECT skill FROM draft_taint").fetch_all(db).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("reading untrusted drafts: {e:#}");
            skills::drafts().into_iter().map(|s| format!("{}/{}", s.domain, s.name)).collect()
        }
    }
}

/// Mark a draft as written by a session that read untrusted content.
async fn mark_draft(db: &sqlx::PgPool, skill: &str, session: Uuid) -> Result<()> {
    sqlx::query("INSERT INTO draft_taint (skill, session_id) VALUES ($1, $2) ON CONFLICT (skill) DO NOTHING").bind(skill).bind(session).execute(db).await?;
    Ok(())
}

/// Before an `edit` or `write` into a draft skill from a session that read untrusted content: mark
/// the draft. Why the call is refused, when the mark can't be recorded.
pub async fn mark_draft_write(app: &App, session: Uuid, workspace: &Path, name: &str, args: &Value) -> Option<String> {
    if !matches!(name, "edit" | "write") {
        return None;
    }
    let skill = crate::agent::draft_target(workspace, name, args)?;
    if !crate::taint::limited(&app.db, session, crate::taint::Limit::Steer).await {
        return None;
    }
    mark_draft(&app.db, &skill, session).await.err().map(|e| format!("not written: the draft {skill} couldn't be marked as written by a session that read untrusted content ({e:#})"))
}

/// After the owner accepts a session's work: drafts it loaded (in domains that exist) become
/// active, except drafts marked untrusted (only the owner's own `zen skills accept` activates those).
pub async fn on_accept(app: &App, session: Uuid) {
    let loaded: Vec<String> = sqlx::query_scalar("SELECT DISTINCT args->>'name' FROM tool_calls WHERE session_id = $1 AND name = 'load_skill' AND NOT is_error")
        .bind(session)
        .fetch_all(&app.db)
        .await
        .unwrap_or_default();
    let domains: Vec<String> = skills::scan(&skills::root()).into_iter().map(|s| s.domain).collect();
    let untrusted = tainted_drafts(&app.db).await;
    for name in loaded {
        let Ok(d) = skills::lookup(&skills::drafts(), &name).cloned() else { continue };
        if untrusted.contains(&format!("{}/{}", d.domain, d.name)) {
            tracing::info!("not activating {}/{}: a session that read untrusted content wrote it (the owner accepts it with zen skills)", d.domain, d.name);
            continue;
        }
        // By origin too (D-062): a draft whose files are rated 4 or more, or written by a session
        // since stopped, is activated only by the owner's own accept. Unknown means no.
        match folder_rating(&app.db, &d.dir).await {
            Ok((risk, _)) if risk <= 3 => {}
            other => {
                tracing::info!("not activating {}/{}: its files are rated {:?} by origin (the owner accepts it with zen skills)", d.domain, d.name, other.map(|r| r.0).map_err(|e| e.to_string()));
                continue;
            }
        }
        if domains.contains(&d.domain) {
            match decide_draft(&app.db, &format!("{}/{}", d.domain, d.name), true, "a session that used it was accepted", None).await {
                Ok(m) => tracing::info!("{m}"),
                Err(e) => tracing::warn!("activating {name}: {e:#}"),
            }
        }
    }
}

/// Each skill's use: loads, the last one, and the owner's verdicts on sessions that loaded it.
pub async fn stats(db: &sqlx::PgPool) -> Result<Value> {
    let rows = sqlx::query(
        "WITH loads AS (SELECT args->>'name' AS name, session_id, created_at FROM tool_calls WHERE name = 'load_skill' AND NOT is_error),
              verdicts AS (SELECT DISTINCT ON (session_id) session_id, decision FROM session_decisions ORDER BY session_id, created_at DESC)
         SELECT l.name, count(*) AS loads, max(l.created_at) AS last,
                count(*) FILTER (WHERE v.decision = 'accept') AS accepted, count(v.decision) AS judged
         FROM loads l LEFT JOIN verdicts v USING (session_id) GROUP BY l.name",
    )
    .fetch_all(db)
    .await?;
    let used = |s: &Skill| rows.iter().find(|r| {
        let n: Option<String> = r.get("name");
        n.as_deref().is_some_and(|n| n == format!("{}/{}", s.domain, s.name) || n == s.name)
    });
    let untrusted = tainted_drafts(db).await;
    let mut out = Vec::new();
    for (status, list) in [("active", skills::scan(&skills::root())), ("draft", skills::drafts())] {
        for s in list {
            let r = used(&s);
            // What the owner reviews a draft by (and sends back to accept or reject it).
            let sha = if status == "draft" { fingerprint(db, &s.dir).await.ok() } else { None };
            out.push(json!({
                "sha": sha,
                "skill": format!("{}/{}", s.domain, s.name), "status": status, "description": s.description,
                "untrusted": status == "draft" && untrusted.contains(&format!("{}/{}", s.domain, s.name)),
                "loads": r.map(|r| r.get::<i64, _>("loads")).unwrap_or(0),
                "last_load": r.map(|r| r.get::<chrono::DateTime<chrono::Utc>, _>("last")),
                "sessions_accepted": r.map(|r| r.get::<i64, _>("accepted")).unwrap_or(0),
                "sessions_judged": r.map(|r| r.get::<i64, _>("judged")).unwrap_or(0),
            }));
        }
    }
    let mut tools = Vec::new();
    for (t, approved) in made_tools(db).await {
        let sha = fingerprint(db, &tools_root().join(&t.name)).await.ok();
        tools.push(json!({ "tool": t.name, "approved": approved, "description": t.description, "sha": sha }));
    }
    Ok(json!({ "skills": out, "tools": tools }))
}

/// The nightly `skills` job (jobs.rs): active skills unused for 30 days are flagged, for 90 archived.
/// A skill's age counts from when it was written (its SKILL.md), so new skills aren't flagged.
pub async fn tend(db: &sqlx::PgPool) -> Vec<String> {
    let mut notes = Vec::new();
    let Ok(st) = stats(db).await else { return notes };
    let now = chrono::Utc::now();
    for s in skills::scan(&skills::root()) {
        let key = format!("{}/{}", s.domain, s.name);
        let last_load = st["skills"].as_array().into_iter().flatten().find(|x| x["skill"] == key.as_str()).and_then(|x| x["last_load"].as_str().and_then(|t| t.parse::<chrono::DateTime<chrono::Utc>>().ok()));
        let written = std::fs::metadata(s.dir.join("SKILL.md")).and_then(|m| m.modified()).map(chrono::DateTime::<chrono::Utc>::from).unwrap_or(now);
        let since = last_load.unwrap_or(written).max(written);
        let days = (now - since).num_days();
        if days >= 90 {
            let to = archived_root().join(&s.domain).join(&s.name);
            if !to.exists() && std::fs::create_dir_all(to.parent().unwrap_or(&to)).is_ok() && std::fs::rename(&s.dir, &to).is_ok() {
                notes.push(format!("skills: archived {key} (unused for {days} days)"));
            }
        } else if days >= 30 {
            notes.push(format!("skills: {key} unused for {days} days (archived at 90)"));
        }
    }
    if !notes.is_empty() {
        crate::git::commit_all(&skills::root(), "skills job: skills tended").await;
    }
    notes
}

// ---------- tools the agent makes ----------

/// The agent's tools and whether the owner approved each.
pub async fn made_tools(db: &sqlx::PgPool) -> Vec<(crate::mcp::ToolEntry, bool)> {
    let approved: Vec<String> = sqlx::query_scalar("SELECT name FROM made_tools WHERE approved_at IS NOT NULL AND rejected_at IS NULL").fetch_all(db).await.unwrap_or_default();
    made_entries().into_iter().map(|t| {
        let ok = approved.contains(&t.name);
        (t, ok)
    }).collect()
}

/// The agent's tools from their manifests (`<zen home>/global/tools/<name>/tool.json`), as catalog entries
/// (server `made`).
pub fn made_entries() -> Vec<crate::mcp::ToolEntry> {
    let root = tools_root();
    let mut out = Vec::new();
    for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let Ok(m) = std::fs::read_to_string(e.path().join("tool.json")) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&m) else { continue };
        let name = v["name"].as_str().unwrap_or("").to_string();
        if name.is_empty() || name != e.file_name().to_string_lossy() {
            continue;
        }
        out.push(crate::mcp::ToolEntry { server: "made".into(), name, description: v["description"].as_str().unwrap_or("").to_string(), schema: v["parameters"].clone(), untrusted: false, manifest: Some(m) });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

async fn save_tool(app: &App, session: Uuid, args: &Value) -> Result<String> {
    let name = args["name"].as_str().map(str::trim).unwrap_or("");
    if !valid_name(name) {
        bail!("name: lowercase letters, digits and single hyphens");
    }
    let description = args["description"].as_str().map(str::trim).filter(|d| !d.is_empty()).context("give a `description`: what it does, when to use it, what it returns")?;
    let command = args["command"].as_str().map(str::trim).filter(|c| !c.is_empty()).context("give the `command` that runs it, e.g. `python3 run.py` (it reads the arguments as JSON on stdin)")?;
    let reason = args["reason"].as_str().map(str::trim).filter(|r| !r.is_empty()).context("give the `reason`: what it saves (a repeated chore, a fragile procedure)")?;
    let params = if args["parameters"].is_object() { args["parameters"].clone() } else { json!({ "type": "object", "properties": {} }) };
    if params["type"] != "object" {
        bail!("`parameters` must be a JSON schema of type object");
    }
    let dir = tools_root().join(name);
    std::fs::create_dir_all(&dir)?;
    for (rel, content) in args["files"].as_object().into_iter().flatten() {
        let rel = Path::new(rel);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) || rel == Path::new("tool.json") {
            bail!("file `{}` must be a relative path inside the tool (and not tool.json)", rel.display());
        }
        let path = dir.join(rel);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        crate::sources::record_bytes(&app.db,session,&path,content.as_str().unwrap_or(""),None).await?;
        std::fs::write(&path, content.as_str().unwrap_or(""))?;
    }
    let manifest = json!({ "name": name, "description": description, "parameters": params, "command": command });
    let manifest = serde_json::to_string_pretty(&manifest)?;
    crate::sources::record_bytes(&app.db,session,&dir.join("tool.json"),&manifest,None).await?;
    std::fs::write(dir.join("tool.json"),manifest)?;
    sqlx::query("INSERT INTO made_tools (name) VALUES ($1) ON CONFLICT (name) DO NOTHING").bind(name).execute(&app.db).await?;
    crate::git::commit_all(&tools_root(), &format!("save_tool: {name} (session {})\n\n{reason}", &session.to_string()[..8])).await;
    let approved: bool = sqlx::query_scalar("SELECT approved_at IS NOT NULL AND rejected_at IS NULL FROM made_tools WHERE name = $1").bind(name).fetch_one(&app.db).await.unwrap_or(false);
    Ok(format!(
        "Saved tool made_{name} ({}). {}",
        dir.display(),
        if approved {
            "It is approved: it runs with the network.".to_string()
        } else {
            format!("Until the owner approves it (zen tools accept {name}) it runs sandboxed: no network, files read-only. Try it with call_tool.")
        }
    ))
}

/// A tool's content (its manifest and files, in path order) as bytes, for its fingerprint.
fn content(dir: &Path) -> Vec<u8> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if e.file_name() == ".git" {
                continue;
            }
            let kind = match e.file_type() { Ok(k) => k, Err(_) => continue };
            if kind.is_dir() {
                walk(&p, base, out);
            } else if kind.is_symlink() {
                // Hash the link itself, never follow it outside this tool or into a cycle.
                if let Ok(target) = std::fs::read_link(&p) {
                    out.push((p.strip_prefix(base).unwrap_or(&p).display().to_string(), target.as_os_str().as_encoded_bytes().to_vec()));
                }
            } else if kind.is_file() {
                if let Ok(bytes) = std::fs::read(&p) {
                    out.push((p.strip_prefix(base).unwrap_or(&p).display().to_string(), bytes));
                }
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort();
    let mut all = Vec::new();
    for (path, bytes) in files {
        all.extend_from_slice(path.as_bytes());
        all.push(0);
        all.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        all.extend_from_slice(&bytes);
    }
    all
}

/// The SHA-256 of a folder's content: a tool's or a draft skill's (computed by Postgres: no
/// hashing crate needed).
async fn fingerprint(db: &sqlx::PgPool, dir: &Path) -> Result<String> {
    anyhow::ensure!(dir.is_dir(), "{} doesn't exist", dir.display());
    Ok(sqlx::query_scalar("SELECT encode(sha256($1), 'hex')").bind(content(dir)).fetch_one(db).await?)
}

/// Whether the fingerprint the owner reviewed (all of it, or its first 8 or more characters, as
/// `zen skills` shows it) is the content's now.
fn same_content(now: &str, reviewed: &str) -> Result<()> {
    let r = reviewed.trim().to_lowercase();
    anyhow::ensure!(r.len() >= 8, "give the fingerprint you reviewed (`zen skills` shows it; 8 characters or more)");
    anyhow::ensure!(now.starts_with(&r), "it changed since you reviewed it (now {}): look at it again (`zen skills`)", &now[..12.min(now.len())]);
    Ok(())
}

/// The owner approves (or rejects) a tool the agent made, as reviewed (its fingerprint, `reviewed`).
/// Approval covers the tool as it is now: a later change to its manifest or files puts it back in
/// the sandbox until approved again.
pub async fn decide_tool(db: &sqlx::PgPool, name: &str, accept: bool, reviewed: &str) -> Result<String> {
    let dir = tools_root().join(name);
    anyhow::ensure!(valid_name(name) && dir.join("tool.json").is_file(), "no tool `{name}`");
    let sha = fingerprint(db, &dir).await?;
    same_content(&sha, reviewed)?;
    let n = sqlx::query(if accept {
        "UPDATE made_tools SET approved_at = now(), approved_sha = $2, rejected_at = NULL WHERE name = $1"
    } else {
        "UPDATE made_tools SET rejected_at = now() WHERE name = $1 AND $2 IS NOT NULL"
    })
    .bind(name)
    .bind(&sha)
    .execute(db)
    .await?
    .rows_affected();
    anyhow::ensure!(n == 1, "no tool `{name}`");
    Ok(format!("{} {name}", if accept { "Approved" } else { "Rejected" }))
}

/// How much of a made tool's stdout and stderr is kept (each); the rest is drained and dropped.
const MADE_CAP: usize = 256 * 1024;

async fn drain_capped(mut pipe: impl tokio::io::AsyncRead + Unpin, cap: usize) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    while let Ok(n) = pipe.read(&mut chunk).await {
        if n == 0 { break; }
        kept.extend_from_slice(&chunk[..n.min(cap.saturating_sub(kept.len()))]);
    }
    kept
}

/// Copy a tool's folder (without `.git`; symlinks as symlinks, never followed).
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        if e.file_name() == ".git" {
            continue;
        }
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let kind = e.file_type()?;
        if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else if kind.is_file() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// The origin rating of a made tool's (or a draft skill's) folder (D-062): the highest of its files' ratings by exact
/// bytes (`context::rate`), and their writers. Non-text files and links cannot be rated by
/// this text provenance path and are rejected, not executed with an assumed rating.
/// Errors fail closed: the tool doesn't run.
pub async fn folder_rating(db: &sqlx::PgPool, dir: &Path) -> Result<(i16, Vec<Uuid>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, bool)>) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.file_name() == ".git" {
                continue;
            }
            let kind = e.file_type()?;
            if kind.is_dir() {
                walk(&e.path(), out)?;
            } else {
                out.push((e.path(), kind.is_file()));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, &mut files)?;
    let (mut risk, mut writers) = (1, Vec::new());
    for (path, regular) in files {
        anyhow::ensure!(regular, "tool file {} is not a regular file; its provenance cannot be rated", path.display());
        let text = std::fs::read_to_string(&path).with_context(|| format!("rating exact bytes of {}", path.display()))?;
        let (r, w) = crate::context::rate(db, &text, 1).await?;
        risk = risk.max(r);
        writers.extend(w);
    }
    writers.sort();
    writers.dedup();
    Ok((risk, writers))
}

/// A private copy of a tool's folder, removed when dropped.
struct RunCopy(PathBuf);

impl RunCopy {
    fn of(name: &str) -> Result<Self> {
        Self::from_dir(name, &tools_root().join(name))
    }

    fn from_dir(name: &str, source: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().join(format!("zen-tool-{}", Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&root)?;
        let copy = RunCopy(root);
        copy_tree(source, &copy.dir(name)).context("copying the tool")?;
        Ok(copy)
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for RunCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run a tool the agent made: arguments as JSON on stdin, output from stdout and stderr. Sandboxed
/// (no network, read-only files, an empty home folder) until the owner approves it; never with the
/// kernel's environment; its process group is killed after 120 s. It runs from a private copy of
/// its folder, which is what is checked against the approval (nothing can change it in between)
/// and what it writes to (dropped after the run).
/// The copy is rated by its exact bytes before it runs (`folder_rating`), and `session` depends on
/// its writers: a tool a stopped session wrote doesn't run (rating 5, nothing else returned).
/// Returns the output, whether it failed, whether it ran with the network (approved), and the
/// tool's origin rating.
pub async fn run_made(db: &sqlx::PgPool, session: Uuid, name: &str, args: &Value, force_sandbox: bool, owner_allowed: bool) -> Result<(String, bool, bool, i16)> {
    anyhow::ensure!(valid_name(name) && tools_root().join(name).join("tool.json").is_file(), "no such tool");
    let copy = RunCopy::of(name)?;
    run_snapshot(db, session, name, args, force_sandbox, owner_allowed, &copy).await
}

/// The unapproved tool's sandbox: read-only, no network, the home folder replaced by an empty one
/// (so the tool can't read the owner's keys or tokens), only its own folder bound back. A network
/// namespace doesn't isolate pathname Unix sockets (the Docker socket is root on this host), so
/// `/run` is emptied and the offline shell's seccomp filter refuses connect and send on any
/// socket. The returned file must outlive the spawn.
fn sandboxed(home: &str, dir: &Path, command: &str) -> Result<(tokio::process::Command, std::fs::File)> {
    let filter = crate::tools::network_filter().map_err(|e| anyhow::anyhow!(e))?;
    let fd = std::os::fd::AsRawFd::as_raw_fd(&filter);
    let mut c = tokio::process::Command::new("bwrap");
    // SAFETY: fcntl is async-signal-safe; it only clears close-on-exec on the child's copy of the
    // filter fd, which bwrap consumes.
    unsafe {
        c.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    c.arg("--seccomp").arg(fd.to_string());
    c.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--tmpfs", "/run", "--unshare-net", "--unshare-pid", "--new-session", "--die-with-parent"])
        .arg("--tmpfs")
        .arg(home)
        .arg("--ro-bind")
        .arg(dir)
        .arg(dir)
        .arg("--chdir")
        .arg(dir)
        .args(["bash", "-c"])
        .arg(command);
    Ok((c, filter))
}

/// Execute only the private snapshot whose bytes are rated below; never reread the source.
async fn run_snapshot(db: &sqlx::PgPool, session: Uuid, name: &str, args: &Value, force_sandbox: bool, owner_allowed: bool, copy: &RunCopy) -> Result<(String, bool, bool, i16)> {
    let dir = copy.dir(name);
    let (risk, writers) = folder_rating(db, &dir).await?;
    if risk >= 5 {
        return Ok((String::new(), true, false, 5));
    }
    crate::sources::depend(db, session, &writers).await?;
    crate::sources::raise(db, session, risk).await?;
    // Denial and new dependency edges cannot commit between this check and process creation.
    let mut spawn_tx = db.begin().await?;
    crate::sources::lock_provenance(&mut spawn_tx).await?;
    anyhow::ensure!(!crate::taint::stopped(db, session).await?, "tool withheld: a writer was stopped");
    // The code's rating affects capabilities before execution, not just its returned output.
    let session_risk: i16 = sqlx::query_scalar("SELECT risk FROM sessions WHERE id=$1").bind(session).fetch_one(db).await?;
    let force_sandbox = force_sandbox || (session_risk >= 4 && !owner_allowed);
    let m: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("tool.json")).context("no such tool")?)?;
    let command = m["command"].as_str().context("the manifest has no command")?.to_string();
    let row = sqlx::query("SELECT approved_at IS NOT NULL AND rejected_at IS NULL AS ok, rejected_at IS NOT NULL AS rejected, approved_sha FROM made_tools WHERE name = $1")
        .bind(name)
        .fetch_optional(db)
        .await?;
    let (approved, rejected, sha) = row.map(|r| (r.get::<bool, _>("ok"), r.get::<bool, _>("rejected"), r.get::<Option<String>, _>("approved_sha"))).unwrap_or((false, false, None));
    anyhow::ensure!(!rejected, "the owner rejected this tool");
    // Approval covers the content the owner saw: a changed tool runs sandboxed again.
    let changed = approved && sha.as_deref() != Some(fingerprint(db, &dir).await?.as_str());
    let approved = approved && !changed && !force_sandbox;
    let keep = ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TZ"];
    let mut _filter: Option<std::fs::File> = None;
    let mut cmd = if approved {
        // `bash -c`, not a login shell: a profile could export secrets.
        let mut c = tokio::process::Command::new("bash");
        c.arg("-c").arg(&command);
        c
    } else {
        // Read-only, no network, and the home folder replaced by an empty one, so the tool can't
        // read the owner's keys or tokens; only its own folder is bound back. A network namespace
        // doesn't isolate pathname Unix sockets (the Docker socket is root on this host), so
        // `/run` is emptied and the same seccomp filter as the offline shell refuses connect and
        // send on any socket.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
        let (c, filter) = sandboxed(&home, &dir, &command)?;
        _filter = Some(filter);
        c
    };
    let mut child = cmd
        .current_dir(&dir)
        .env_clear()
        .envs(keep.iter().filter_map(|k| std::env::var(k).ok().map(|v| (*k, v))))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("starting the tool")?;
    // Kill the whole group on a timeout or an abort, not just bash.
    let _group = crate::tools::GroupKill(child.id().map(|p| p as i32));
    spawn_tx.commit().await?;
    let input = args.to_string();
    let mut stdin = child.stdin.take();
    let run = async move {
        use tokio::io::AsyncWriteExt;
        // Write the arguments in their own task, so a tool that prints before reading can't deadlock.
        let writer = tokio::spawn(async move {
            if let Some(mut s) = stdin.take() {
                let _ = s.write_all(input.as_bytes()).await;
            }
        });
        let (so, se) = (child.stdout.take(), child.stderr.take());
        let read_out = async { match so { Some(p) => drain_capped(p, MADE_CAP).await, None => Vec::new() } };
        let read_err = async { match se { Some(p) => drain_capped(p, MADE_CAP).await, None => Vec::new() } };
        let (out, err) = tokio::join!(read_out, read_err);
        let status = child.wait().await;
        writer.abort();
        (out, err, status)
    };
    let (out, err, status) = tokio::time::timeout(Duration::from_secs(120), run).await.context("the tool took over 120 s")?;
    let status = status.context("waiting for the tool")?;
    let mut text = String::from_utf8_lossy(&out).into_owned();
    let err = String::from_utf8_lossy(&err);
    if !err.trim().is_empty() {
        text.push_str(&format!("\n[stderr]\n{}", err.trim_end()));
    }
    let text = crate::secrets::mask_off_thread(zen_proto::head(&text, 50_000)).await;
    let note = if approved {
        ""
    } else if changed {
        "\n[ran sandboxed: the tool changed since the owner approved it; it needs approval again]"
    } else {
        "\n[ran sandboxed: no network, files read-only, until the owner approves it]"
    };
    anyhow::ensure!(!crate::taint::stopped(db, session).await?, "tool result withheld: a writer was stopped during execution");
    Ok((format!("{}{note}", text.trim_end()), !status.success(), approved, risk))
}

// ---------- the tools ----------

pub fn save_skill_spec() -> Value {
    json!({
        "name": "save_skill",
        "description": "Create or improve one of your skills (how to do a kind of work well). Save from evidence: after a job taught you a \
procedure that worked (or what failed), not after a one-off. Improve an existing skill rather than adding a near-duplicate: find_skills \
first, then save under its domain/name with the improved instructions. A new skill is a draft until the owner accepts it or a session \
that used it is accepted; a new domain always needs the owner. Keep SKILL.md short and checkable (steps that end in something you can \
verify); put detail in references/ and repeatable steps in scripts/ (`files`).",
        "parameters": { "type": "object", "properties": {
            "domain": { "type": "string", "description": "The kind of work, e.g. build, research, ops, work" },
            "name": { "type": "string", "description": "The skill, 2–4 words with hyphens (e.g. rust-release)" },
            "description": { "type": "string", "description": "What it does and when to use it (the trigger first), under 1024 characters" },
            "body": { "type": "string", "description": "The instructions (markdown)" },
            "files": { "type": "object", "description": "Optional extra files: relative path -> content (references/…, scripts/…)" },
            "reason": { "type": "string", "description": "The evidence: what happened that shows this is needed" } },
          "required": ["domain", "name", "description", "body", "reason"] }
    })
}

pub fn save_tool_spec() -> Value {
    json!({
        "name": "save_tool",
        "description": "Make (or update) a tool of your own when a chore keeps coming back or a procedure is fragile by hand: a command \
that reads its arguments as JSON on stdin and prints its result. It is then found with find_tools (as made_<name>) and run with \
call_tool. Until the owner approves it, it runs sandboxed (no network, files read-only); it never gets your environment's secrets.",
        "parameters": { "type": "object", "properties": {
            "name": { "type": "string", "description": "lowercase-with-hyphens" },
            "description": { "type": "string", "description": "What it does, when to use it, what it returns" },
            "parameters": { "type": "object", "description": "JSON schema (type object) of its arguments" },
            "command": { "type": "string", "description": "How to run it from its folder, e.g. `python3 run.py`" },
            "files": { "type": "object", "description": "Its files: relative path -> content (e.g. run.py)" },
            "reason": { "type": "string", "description": "What it saves" } },
          "required": ["name", "description", "command", "files", "reason"] }
    })
}

/// Run `save_skill` or `save_tool`. None for other tools.
pub async fn run_tool(app: &App, session: Uuid, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    let res = match name {
        "save_skill" => save_skill(app, session, args).await,
        "save_tool" => save_tool(app, session, args).await,
        _ => return None,
    };
    Some(match res {
        Ok(content) => tools::ToolOutput { rating: None, content, is_error: false },
        Err(e) => tools::ToolOutput { rating: None, content: format!("{e:#}"), is_error: true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unapproved tool can't reach a host Unix socket (the Docker socket is root): `/run` is
    /// emptied and connect is refused on any socket, here one inside the tool's own folder, the
    /// one place the sandbox still shows.
    #[tokio::test]
    async fn unapproved_tools_cannot_reach_unix_sockets() {
        if std::process::Command::new("bwrap").arg("--version").output().is_err() {
            eprintln!("skipped: no bwrap");
            return;
        }
        let dir = crate::test_util::TestDir::new("made-tool-socket");
        let sock = dir.join("daemon.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let home = crate::test_util::TestDir::new("made-tool-home");
        let probe = format!(
            "python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); print(\"CONNECTED\")' {} 2>&1; ls /run",
            sock.display()
        );
        let (mut c, _filter) = sandboxed(&home.display().to_string(), &dir, &probe).unwrap();
        let out = c.output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(!text.contains("CONNECTED"), "{text}");
        assert!(text.contains("Operation not permitted"), "{text}");
        assert!(!text.contains("docker.sock"), "/run is emptied: {text}");
    }

    #[test]
    fn names_and_overlap() {
        assert!(valid_name("rust-release") && !valid_name("Rust") && !valid_name("a--b") && !valid_name("-a"));
        assert!(overlap("verify work before reporting it done", "verify the work before reporting done") > 0.6);
        assert!(overlap("verify work before reporting", "deploy a rust binary to the server") < 0.2);
    }

    #[tokio::test]
    async fn made_tool_output_is_drained_after_cap() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(4096);
        let writing = tokio::spawn(async move { writer.write_all(&vec![b'x'; MADE_CAP * 2]).await.unwrap(); });
        let kept = tokio::time::timeout(Duration::from_secs(2), drain_capped(reader, MADE_CAP)).await.unwrap();
        writing.await.unwrap();
        assert_eq!(kept.len(), MADE_CAP);
    }

    #[test]
    fn fingerprint_does_not_follow_symlinks() {
        let dir = crate::test_util::TestDir::new("fingerprint");
        std::fs::write(dir.join("tool.json"), "{}").unwrap();
        std::os::unix::fs::symlink(".", dir.join("cycle")).unwrap();
        let bytes = content(&dir);
        assert!(bytes.windows(5).any(|w| w == b"cycle"));
        assert!(bytes.len() < 100, "a symlink cycle must not expand");

    }

    #[test]
    fn a_copy_has_the_same_content_and_a_review_names_it() {
        let dir = crate::test_util::TestDir::new("copy-tool");
        let tool = dir.join("t");
        std::fs::create_dir_all(tool.join("lib/.git")).unwrap();
        std::fs::write(tool.join("tool.json"), "{}").unwrap();
        std::fs::write(tool.join("lib/run.py"), "print(1)").unwrap();
        std::os::unix::fs::symlink("lib/run.py", tool.join("run.py")).unwrap();
        copy_tree(&tool, &dir.join("copy")).unwrap();
        assert_eq!(content(&tool), content(&dir.join("copy")), "the copy is what gets fingerprinted");
        assert!(std::fs::symlink_metadata(dir.join("copy/run.py")).unwrap().file_type().is_symlink());
        let now = "0123456789abcdef";
        assert!(same_content(now, "0123456789AB").is_ok() && same_content(now, now).is_ok());
        assert!(same_content(now, "01234").is_err(), "too short to mean anything");
        assert!(same_content(now, "fedcba98").unwrap_err().to_string().contains("changed since you reviewed it"));
    }

    /// Execute a private rated snapshot, with no global path/environment overrides.
    #[tokio::test]
    #[ignore]
    async fn made_execution_provenance_database() {
        let db = sqlx::PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap()).await.unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        let app = crate::test_util::app(db.clone());
        let session = |risk: i16| {
            let db = db.clone();
            async move {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO sessions(id,title,model,risk) VALUES($1,'made execution','faux/smoke',$2)")
                    .bind(id).bind(risk).execute(&db).await.unwrap();
                id
            }
        };
        let dir = crate::test_util::TestDir::new("made-execution");
        let name = format!("probe-{}", Uuid::new_v4());
        let tool = dir.join(&name);
        std::fs::create_dir_all(&tool).unwrap();
        let manifest = format!(r#"{{"name":"{name}","command":"sleep 0.2; printf SAFE"}}"#);
        std::fs::write(tool.join("tool.json"), &manifest).unwrap();
        let writer = session(4).await;
        crate::sources::record_bytes(&db, writer, &tool.join("tool.json"), &manifest, None).await.unwrap();
        let copy = RunCopy::from_dir(&name, &tool).unwrap();
        // An owner-approved fingerprint must not override the code's numeric origin.
        let sha = fingerprint(&db, &copy.dir(&name)).await.unwrap();
        sqlx::query("INSERT INTO made_tools(name,approved_at,approved_sha) VALUES($1,now(),$2)")
            .bind(&name).bind(sha).execute(&db).await.unwrap();
        // Change the live source after copying: only the rated private snapshot executes.
        std::fs::write(tool.join("tool.json"), r#"{"command":"printf UNRATED"}"#).unwrap();
        let reader = session(1).await;
        let (text, error, networked, risk) = run_snapshot(&db, reader, &name, &json!({}), false, false, &copy).await.unwrap();
        assert!(!error && !networked && risk == 4 && text.starts_with("SAFE"), "{text}");
        let stored: i16 = sqlx::query_scalar("SELECT risk FROM sessions WHERE id=$1").bind(reader).fetch_one(&db).await.unwrap();
        assert_eq!(stored, 4);
        let granted = session(4).await;
        let (_, error, networked, risk) = run_snapshot(&db, granted, &name, &json!({}), false, true, &copy).await.unwrap();
        assert!(!error && networked && risk == 4, "an exact owner grant changes capabilities, not origin");
        // Denial in flight withholds even a successful result. A denial before start may reject
        // it earlier; neither timing is allowed to return the tool's words.
        let reader2 = session(1).await;
        let args = json!({});
        let (result, denied) = tokio::join!(
            run_snapshot(&db, reader2, &name, &args, false, false, &copy),
            async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                crate::taint::deny(&app, writer, "made execution race").await
            }
        );
        denied.unwrap();
        assert!(result.is_err(), "an in-flight denied tool's output must be withheld");
        assert!(crate::taint::stopped(&db, reader2).await.unwrap());
        let reader3 = session(1).await;
        let (text, error, networked, risk) = run_snapshot(&db, reader3, &name, &args, false, true, &copy).await.unwrap();
        assert!(text.is_empty() && error && !networked && risk == 5, "an exact grant cannot execute a contaminated snapshot");
    }

    #[test]
    fn skill_files_stay_inside_the_skill() {
        let scratch = crate::test_util::TestDir::new("ws");
        let dir = scratch.join("x");
        assert!(write_skill(&dir, &skill_md("x", "Does x.", "Do it."), &json!({ "../escape.md": "no" })).is_err());
        write_skill(&dir, &skill_md("x", "Does x.", "Do it."), &json!({ "references/a.md": "ok" })).unwrap();
        assert!(dir.join("references/a.md").exists() && skills::validate(&dir).is_empty());
    }
}
