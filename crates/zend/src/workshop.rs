//! The workshop: the agent improves its own skills, without sprawl.
//!
//! `save_skill` is the way to create or change a skill, and the kernel enforces the rules:
//! - the agentskills.io format, a SKILL.md under 10,000 characters, and a reason (the evidence);
//! - edit before create: a new skill that does the same job as one in its domain is refused with
//!   "extend X instead" (System One judges; without it, word overlap);
//! - a saved skill is active at once; every change is a commit in the skills folder;
//! - the nightly `skills` job flags skills unused for 30 days and archives (never deletes) those
//!   unused for 90 (`skills/_archived`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::skills::{self, Skill};
use crate::{tools, App};

const MAX_SKILL: usize = 10_000;

fn archived_root() -> PathBuf {
    skills::root().join("_archived")
}

/// A skill's or domain's name (agentskills.io: at most 64 characters).
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

/// Save into agent `who`'s skills: zenbot's are the shared ones, another agent's its own folder
/// (duplicates checked against every skill it sees).
async fn save_skill(app: &App, session: Uuid, who: &crate::agents::Agent, args: &Value) -> Result<String> {
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
    let home = who.skill_home();
    let active = skills::scan_roots(&who.skill_roots());
    let current = skills::scan(&home).iter().any(|x| x.domain == domain && x.name == name);
    if !current {
        if let Some(dup) = duplicate_of(app, &domain, &name, &description, &body, &active).await {
            bail!("{domain}/{dup} already does this job: extend it instead (save_skill with name `{dup}` and its improved instructions), so skills get better instead of multiplying");
        }
    }
    // Validate in a scratch copy first, so a bad save never replaces a good skill.
    let scratch = std::env::temp_dir().join(format!("zen-skill-{}", Uuid::new_v4())).join(&name);
    let written = write_skill(&scratch, &md, &args["files"]);
    let errs = if written.is_ok() { skills::validate(&scratch) } else { Vec::new() };
    let _ = std::fs::remove_dir_all(scratch.parent().unwrap_or(&scratch));
    written?;
    if !errs.is_empty() {
        bail!("not saved: {}", errs.join("; "));
    }
    let dir = home.join(&domain).join(&name);
    write_skill(&dir, &md, &args["files"])?;
    let what = if current { "Updated" } else { "Saved" };
    crate::git::commit_all(&home, &format!("{what} {domain}/{name} (session {})\n\n{reason}", &session.to_string()[..8])).await;
    Ok(format!("{what} {domain}/{name} ({}); it is active now.", dir.display()))
}

/// After the owner archives a session as done: the skills it loaded get the outcome.
pub async fn on_accept(app: &App, session: Uuid) {
    if let Err(e) = sqlx::query("UPDATE brain_skill_uses SET outcome='session accepted by owner' WHERE session_id=$1").bind(session).execute(&app.db).await {
        tracing::warn!("recording skill outcome: {e:#}");
    }
}

pub async fn stats(db: &sqlx::SqlitePool) -> Result<Value> {
    let rows = sqlx::query(
        "WITH loads AS (SELECT args->>'name' AS name, session_id, created_at FROM tool_calls WHERE name = 'load_skill' AND NOT is_error),
              legacy AS (SELECT session_id,decision FROM (SELECT session_id,decision,row_number() OVER(PARTITION BY session_id ORDER BY created_at DESC,id DESC) AS rn FROM session_decisions) WHERE rn=1),
              verdicts AS (SELECT id AS session_id, CASE outcome WHEN 'done' THEN 'accept' ELSE outcome END AS decision FROM sessions WHERE outcome IS NOT NULL
                           UNION ALL SELECT session_id,decision FROM legacy WHERE session_id NOT IN (SELECT id FROM sessions WHERE outcome IS NOT NULL))
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
    let mut out = Vec::new();
    for s in skills::scan(&skills::root()) {
        let r = used(&s);
        out.push(json!({
            "skill": format!("{}/{}", s.domain, s.name), "status": "active", "description": s.description,
            "loads": r.map(|r| r.get::<i64, _>("loads")).unwrap_or(0),
            "last_load": r.map(|r| r.get::<chrono::DateTime<chrono::Utc>, _>("last")),
            "sessions_accepted": r.map(|r| r.get::<i64, _>("accepted")).unwrap_or(0),
            "sessions_judged": r.map(|r| r.get::<i64, _>("judged")).unwrap_or(0),
        }));
    }
    Ok(json!({ "skills": out }))
}

/// The nightly `skills` job (jobs/): active skills unused for 30 days are flagged, for 90 archived.
/// A skill's age counts from when it was written (its SKILL.md), so new skills aren't flagged.
pub async fn tend(db: &sqlx::SqlitePool) -> Vec<String> {
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

pub fn save_skill_spec() -> Value {
    json!({
        "name": "save_skill",
        "description": "Create or improve an evidence-based reusable skill, not a one-off. find_skills first; update an existing domain/name instead of duplicating. A saved skill is active at once. Keep instructions short and verifiable; put detail in references/ and repeatable steps in scripts/ via files.",
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
pub async fn run_tool(app: &App, session: Uuid, who: &crate::agents::Agent, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    let res = match name {
        "save_skill" => save_skill(app, session, who, args).await,
        _ => return None,
    };
    Some(match res {
        Ok(content) => tools::ToolOutput::ok(content),
        Err(e) => tools::ToolOutput::error(format!("{e:#}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_overlap() {
        assert!(valid_name("rust-release") && !valid_name("Rust") && !valid_name("a--b") && !valid_name("-a"));
        assert!(overlap("verify work before reporting it done", "verify the work before reporting done") > 0.6);
        assert!(overlap("verify work before reporting", "deploy a rust binary to the server") < 0.2);
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
