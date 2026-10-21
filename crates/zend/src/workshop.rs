//! The workshop: the agent improves its own skills, without sprawl
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
//!   active only after owner fingerprint review (`zen skills accept`); a new domain always needs
//!   the owner; a draft a session that read
//!   untrusted content wrote (by `save_skill` or `edit`/`write`) is marked (`draft_taint`), read
//!   as untrusted content, and activated only by the owner's own accept (D-053);
//! - changes to an active skill stay drafts until owner review; every change is a skills commit;
//! - the nightly sleep flags skills unused for 30 days and archives (never deletes) those unused
//!   for 90 (`skills/_archived`).
//!
//! Made-tool execution and authoring are retired (D-070). Existing files and fingerprints stay
//! available for owner inspection; old approvals cannot activate them. Repeatable procedures
//! belong in reviewed skills and their supporting scripts.

use std::path::{Path, PathBuf};

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
    let mut options = serde_json::Map::new();
    for s in &same {
        options.insert(s.name.clone(), json!(s.description));
    }
    options.insert("none".into(), json!("None of them: the new skill does a different job"));
    // s1/skill_duplicate.toml; without an answer, word overlap decides.
    let facts = crate::s1::Facts::new(None)
        .with("new_skill", json!({ "name": name, "description": description, "instructions": zen_proto::head(body, 3000) }))
        .with("skills", Value::Object(options));
    let d = crate::s1::decide(&app.db, "skill_duplicate", facts).await;
    if d.answered {
        return d.is("duplicate").then(|| d.choice("same").unwrap_or_default().to_string()).filter(|c| !c.is_empty());
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
        (Some(_), _) => (proposed_root().join(&domain).join(&name), "Saved a revision draft"),
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
        format!("The active version is unchanged; owner reviews this revision with zen skills accept {domain}/{name}.")
    } else if tainted {
        format!("This session read untrusted content, so the draft is marked untrusted: only the owner can make it active (zen skills accept {domain}/{name}). Say so in your report.")
    } else if new_domain {
        format!("`{domain}` is a new domain: the owner decides whether it becomes active (zen skills accept {domain}/{name}). Say so in your report.")
    } else {
        format!("It can be found and loaded as a draft now; it becomes active when the owner accepts it (zen skills accept {domain}/{name}) after explicit owner review.")
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
    let mut replaced = None;
    if to.exists() {
        anyhow::ensure!(accept && reviewed.is_some(),"replacement requires owner fingerprint review");
        let previous=archived_root().join(&draft.domain).join(format!("{}-{}",draft.name,Uuid::new_v4()));
        std::fs::create_dir_all(previous.parent().context("missing parent")?)?;
        std::fs::rename(&to,&previous)?;
        replaced = Some(previous);
    }
    std::fs::create_dir_all(to.parent().context("no parent")?)?;
    if let Err(e) = std::fs::rename(&draft.dir, &to) {
        if let Some(previous) = replaced {
            std::fs::rename(&previous, &to).context("restoring previous active skill after failed activation")?;
        }
        return Err(e.into());
    }
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
    // High-risk file tools cannot write outside their workspace. A refused write must not
    // mark an unchanged draft as contaminated before the file tool checks its anchored path.
    let target = tools::resolve(workspace, args["path"].as_str().unwrap_or(""));
    if !crate::agent::real(&target).starts_with(crate::agent::real(workspace)) { return None; }
    mark_draft(&app.db, &skill, session).await.err().map(|e| format!("not written: the draft {skill} couldn't be marked as written by a session that read untrusted content ({e:#})"))
}

/// After the owner accepts a session's work: drafts it loaded (in domains that exist) become
/// active, except drafts marked untrusted (only the owner's own `zen skills accept` activates those).
pub async fn on_accept(app: &App, session: Uuid) {
    if let Err(e)=sqlx::query("UPDATE brain_skill_uses SET outcome='session accepted by owner' WHERE session_id=$1").bind(session).execute(&app.db).await {tracing::warn!("recording skill outcome: {e:#}");}
}

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
        tools.push(json!({ "tool": t.name, "approved": approved, "retired": true, "description": t.description, "sha": sha }));
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
        out.push(crate::mcp::ToolEntry { server: "made".into(), name, description: v["description"].as_str().unwrap_or("").to_string(), schema: v["parameters"].clone(), untrusted: false, manifest: Some(m), annotations: Value::Null });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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

/// Compatibility endpoint: old approvals cannot reactivate retired made tools. Files remain on disk.
pub async fn decide_tool(_db: &sqlx::PgPool, _name: &str, _accept: bool, _reviewed: &str) -> Result<String> {
    bail!("Made tools are retired; migrate the procedure into a reviewed skill with supporting scripts.")
}

/// The origin rating of a made tool's (or a draft skill's) folder (D-062): the highest of its files' ratings by exact
/// bytes (`context::rate`), and their writers. Non-text files and links cannot be rated by
/// this text provenance path and are rejected, not executed with an assumed rating.
/// Errors fail closed: the tool doesn't run.
#[cfg(test)]
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

// ---------- the tools ----------

pub fn save_skill_spec() -> Value {
    json!({
        "name": "save_skill",
        "description": "Create or improve an evidence-based reusable skill, not a one-off. find_skills first; update an existing domain/name instead of duplicating. Changes remain drafts until the owner accepts their fingerprint; new domains need the owner. Keep instructions short and verifiable; put detail in references/ and repeatable steps in scripts/ via files.",
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

/// Run `save_skill`. None for other tools.
pub async fn run_tool(app: &App, session: Uuid, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    let res = match name {
        "save_skill" => save_skill(app, session, args).await,
        _ => return None,
    };
    Some(match res {
        Ok(content) => tools::ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), rating: None, content, is_error: false },
        Err(e) => tools::ToolOutput { content_blocks: Vec::new(), saved_outputs: Vec::new(), rating: None, content: format!("{e:#}"), is_error: true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unapproved tool can't reach a host Unix socket (the Docker socket is root): `/run` is
    /// emptied and connect is refused on any socket, here one inside the tool's own folder, the
    /// one place the sandbox still shows.
    #[test]
    fn names_and_overlap() {
        assert!(valid_name("rust-release") && !valid_name("Rust") && !valid_name("a--b") && !valid_name("-a"));
        assert!(overlap("verify work before reporting it done", "verify the work before reporting done") > 0.6);
        assert!(overlap("verify work before reporting", "deploy a rust binary to the server") < 0.2);
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
    fn a_review_names_the_content() {
        let now = "0123456789abcdef";
        assert!(same_content(now, "0123456789AB").is_ok() && same_content(now, now).is_ok());
        assert!(same_content(now, "01234").is_err(), "too short to mean anything");
        assert!(same_content(now, "fedcba98").unwrap_err().to_string().contains("changed since you reviewed it"));
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
