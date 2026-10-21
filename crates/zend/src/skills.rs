//! Skills: how to do a kind of work well, loaded on demand (DESIGN.md, "Skills and tools that
//! improve themselves"; the agentskills.io format).
//!
//! A skill is a folder `<zen home>/global/skills/<domain>/<name>/` with a `SKILL.md` (frontmatter `name` and
//! `description`, then the instructions) and optional `references/`, `scripts/` and `assets/`. The
//! session's fixed prefix carries only an index (`index_text`); the model finds skills with
//! `find_skills` and loads one with `load_skill`, whose text arrives as a tool result, so the prefix
//! and the prompt cache never change mid-session. Loads are recorded like any tool call
//! (`tool_calls`), which is how unused and overused skills are measured.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Largest SKILL.md body (or reference file) a load returns; the rest is cut with a note.
const MAX_LOAD: usize = 48 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub domain: String,
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    /// Exact SKILL.md bytes used for validation, search and source ratings.
    pub text: String,
}

/// Where skills live: `ZEN_SKILLS_DIR`, else `~/.zenbot/global/skills` (shared by every agent).
pub fn root() -> PathBuf {
    std::env::var("ZEN_SKILLS_DIR").map(PathBuf::from).unwrap_or_else(|_| crate::layout::global_dir().join("skills"))
}

/// The frontmatter of a SKILL.md: simple `key: value` lines between `---` fences (a folded `>`
/// or `|` value continues on indented lines). Returns the fields and the body after the fences.
pub fn frontmatter(text: &str) -> (Vec<(String, String)>, &str) {
    let rest = match text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) {
        Some(r) => r,
        None => return (Vec::new(), text),
    };
    // The closing fence is a whole `---` line (not `---x`, and not a body's leading list dash).
    let fence = |i: usize| rest[i + 4..].is_empty() || rest[i + 4..].starts_with('\n') || rest[i + 4..].starts_with("\r\n");
    let Some(end) = rest.match_indices("\n---").map(|(i, _)| i).find(|&i| fence(i)) else { return (Vec::new(), text) };
    let head = rest[..end].trim_end_matches('\r');
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = fields.last_mut() {
                let t = line.trim();
                if !t.is_empty() {
                    if !v.is_empty() {
                        v.push(' ');
                    }
                    v.push_str(t);
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            let v = if matches!(v, ">" | "|" | ">-" | "|-") { "" } else { v.trim_matches(|c| c == '"' || c == '\'') };
            fields.push((k.trim().to_string(), v.to_string()));
        }
    }
    (fields, body)
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Problems with a skill folder, by the agentskills.io rules: a SKILL.md with a `name` that matches
/// the folder (lowercase letters, digits and hyphens, at most 64) and a `description` (at most 1024
/// characters) saying what it does and when to use it. Empty when it's valid.
pub fn validate(dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dir.join("SKILL.md")) else {
        return vec![format!("{} has no SKILL.md", dir.display())];
    };
    validate_text(dir, &text)
}

fn validate_text(dir: &Path, text: &str) -> Vec<String> {
    let mut errs = Vec::new();
    let (fields, body) = frontmatter(text);
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match field(&fields, "name") {
        None | Some("") => errs.push("frontmatter has no `name`".into()),
        Some(n) => {
            if n != folder {
                errs.push(format!("name `{n}` must match the folder name `{folder}`"));
            }
            if !crate::valid_name(n, 64) {
                errs.push(format!("name `{n}`: lowercase letters, digits and single hyphens, at most 64 characters"));
            }
        }
    }
    match field(&fields, "description") {
        None | Some("") => errs.push("frontmatter has no `description` (what it does and when to use it)".into()),
        Some(d) if d.chars().count() > 1024 => errs.push("description is over 1024 characters".into()),
        Some(d) if d.contains(['<', '>']) => errs.push("description can't contain < or >".into()),
        _ => {}
    }
    if body.trim().is_empty() {
        errs.push("SKILL.md has no instructions after the frontmatter".into());
    }
    errs
}

/// Every valid skill under `root`, sorted by domain and name. Invalid folders are skipped (and
/// logged), so one broken skill doesn't hide the rest.
pub fn scan(root: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    let Ok(domains) = std::fs::read_dir(root) else { return out };
    for d in domains.flatten() {
        let dpath = d.path();
        let domain = d.file_name().to_string_lossy().to_string();
        // `_proposed` (drafts) and `_archived` aren't domains (workshop.rs).
        if !dpath.is_dir() || domain.starts_with('.') || domain.starts_with('_') {
            continue;
        }
        let Ok(skills) = std::fs::read_dir(&dpath) else { continue };
        for s in skills.flatten() {
            let dir = s.path();
            if !dir.join("SKILL.md").is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(dir.join("SKILL.md")) else { continue };
            let errs = validate_text(&dir, &text);
            if !errs.is_empty() {
                tracing::warn!("skipping skill {}: {}", dir.display(), errs.join("; "));
                continue;
            }
            let (fields, _) = frontmatter(&text);
            out.push(Skill {
                domain: domain.clone(),
                name: field(&fields, "name").unwrap_or("").to_string(),
                description: field(&fields, "description").unwrap_or("").to_string(),
                dir,
                text,
            });
        }
    }
    out.sort_by(|a, b| (&a.domain, &a.name).cmp(&(&b.domain, &b.name)));
    out
}

/// The skills index for the system prompt: every skill's name and description while that fits
/// in `ZEN_SKILL_INDEX_CHARS` (default 2500), else the domains with their skill counts. Empty
/// when there are no skills.
pub fn index_text(skills: &[Skill]) -> String {
    index_within(skills, crate::config::get().skill_index_chars)
}

/// The skills index in at most `limit` characters (domains only above it).
fn index_within(skills: &[Skill], limit: usize) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let full: String = skills.iter().map(|s| format!("- {}/{}: {}\n", s.domain, s.name, s.description)).collect();
    if full.len() <= limit {
        return full;
    }
    let mut domains: Vec<(String, usize)> = Vec::new();
    for s in skills {
        match domains.iter_mut().find(|(d, _)| d == &s.domain) {
            Some((_, n)) => *n += 1,
            None => domains.push((s.domain.clone(), 1)),
        }
    }
    domains.iter().map(|(d, n)| format!("- {d}: {n} skill(s)\n")).collect()
}

/// Lowercase words of three letters or more, for matching.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 3).map(String::from).collect()
}

/// Skills ranked by how well they match `query`: words in the name count most, then the
/// description, then the body. Ties keep index order. At most `limit`.
pub fn find(skills: &[Skill], query: &str, limit: usize) -> Vec<(Skill, f64)> {
    let q = words(query);
    if q.is_empty() {
        return skills.iter().take(limit).map(|s| (s.clone(), 0.0)).collect();
    }
    let mut scored: Vec<(Skill, f64)> = skills
        .iter()
        .map(|s| {
            let name = words(&format!("{} {}", s.domain, s.name.replace('-', " ")));
            let desc = words(&s.description);
            let body = words(&s.text);
            let score: f64 = q
                .iter()
                .map(|w| {
                    let hit = |list: &[String]| list.iter().any(|x| x == w || (w.len() >= 5 && (x.starts_with(w.as_str()) || w.starts_with(x.as_str()))));
                    if hit(&name) {
                        3.0
                    } else if hit(&desc) {
                        2.0
                    } else if hit(&body) {
                        0.5
                    } else {
                        0.0
                    }
                })
                .sum();
            (s.clone(), score)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored
}

/// A skill by `domain/name` or bare `name` (when only one domain has it).
pub fn lookup<'a>(skills: &'a [Skill], name: &str) -> Result<&'a Skill, String> {
    let name = name.trim().trim_end_matches("/SKILL.md");
    let found: Vec<&Skill> = match name.split_once('/') {
        Some((d, n)) => skills.iter().filter(|s| s.domain == d && s.name == n).collect(),
        None => skills.iter().filter(|s| s.name == name).collect(),
    };
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no skill `{name}`; use find_skills to see what exists")),
        many => Err(format!(
            "`{name}` is in several domains: {}; name it as domain/name",
            many.iter().map(|s| format!("{}/{}", s.domain, s.name)).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// What `load_skill` returns: the SKILL.md (or one of its files), cut at MAX_LOAD, with the list
/// of the skill's other files so the model knows what else it can load.
pub fn load(skill: &Skill, file: Option<&str>) -> Result<String, String> {
    let path = match file.map(str::trim).filter(|f| !f.is_empty() && *f != "SKILL.md") {
        None => skill.dir.join("SKILL.md"),
        Some(f) => {
            let p = skill.dir.join(f);
            let inside = p.canonicalize().ok().zip(skill.dir.canonicalize().ok()).is_some_and(|(p, d)| p.starts_with(d));
            if !inside {
                return Err(format!("`{f}` isn't a file of skill {}/{}", skill.domain, skill.name));
            }
            p
        }
    };
    let text = if path == skill.dir.join("SKILL.md") {skill.text.clone()} else {
        std::fs::read_to_string(&path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?
    };
    Ok(render_load(skill, &path, text))
}

fn render_load(skill: &Skill, path: &Path, mut text: String) -> String {
    if text.len() > MAX_LOAD {
        let mut end = MAX_LOAD;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(&format!("\n[... cut at {MAX_LOAD} bytes; read {} for the rest ...]", path.display()));
    }
    let others = files_of(&skill.dir);
    let mut out = format!("<skill name=\"{}/{}\" path=\"{}\">\n{}\n</skill>", skill.domain, skill.name, path.display(), text.trim_end());
    if !others.is_empty() {
        out.push_str(&format!("\nOther files in this skill (load_skill with `file`, or run scripts with bash from {}): {}", skill.dir.display(), others.join(", ")));
    }
    out
}

/// A skill's files other than SKILL.md, relative to its folder (two levels deep).
fn files_of(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name == "SKILL.md" || name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&p) {
                for f in sub.flatten().filter(|f| f.path().is_file()) {
                    out.push(format!("{name}/{}", f.file_name().to_string_lossy()));
                }
            }
        } else {
            out.push(name);
        }
    }
    out.sort();
    out
}

pub fn find_spec() -> Value {
    json!({
        "name": "find_skills",
        "description": "Find your skills (how to do a kind of work well: framing a job, verifying work, …) by what you need to do; returns names and descriptions. Load one with load_skill when a job matches it.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What you need to do, in a few words (e.g. \"verify a code change\")" }
            },
            "required": ["query"]
        }
    })
}

pub fn load_spec() -> Value {
    json!({
        "name": "load_skill",
        "description": "Load a skill's SKILL.md (or a file inside it, with `file`) and follow it. Name it domain/name. Once loaded it stays in the conversation.",
        "parameters": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The skill, as domain/name (e.g. work/verify)" },
                "file": { "type": "string", "description": "Optional: a file inside the skill (e.g. references/checklist.md) instead of SKILL.md" }
            },
            "required": ["name"]
        }
    })
}

/// Draft skills (`_proposed/<domain>/<name>`), waiting for the owner's OK or a verified use.
pub fn drafts() -> Vec<Skill> {
    scan(&root().join("_proposed"))
}

/// Snapshots admitted to instruction context. Only clean origins may steer the agent;
/// record dependencies even for an index, so a later denial invalidates its cached envelope.
pub async fn instruction_index(db: &sqlx::PgPool, session: uuid::Uuid) -> anyhow::Result<String> {
    let mut admitted = Vec::new();
    for skill in scan(&root()) {
        // Same exact-byte, maximum-origin admission as prompt files (context::admit).
        if let Some(risk) = crate::context::admit(db, session, &skill.text, 1).await? {
            let mut skill = skill;
            skill.description = crate::context::label(&format!("{}/{}",skill.domain,skill.name),risk,&skill.description);
            admitted.push(skill);
        }
    }
    Ok(index_text(&admitted))
}

/// Catalog and loads rate the same snapshots they render, never a later reread. A denied
/// skill is absent from discovery; loading one explicitly returns its level-5 rating.
pub async fn run_tool(db: &sqlx::PgPool, session: uuid::Uuid, name: &str, args: &Value) -> anyhow::Result<(String, bool, i16)> {
    let mut all = scan(&root());
    all.extend(drafts());
    if name == "find_skills" {
        let mut admitted = Vec::new();
        let mut risk = 1;
        for mut skill in all {
            let rating = crate::sources::file_rating(db, &skill.dir.join("SKILL.md"), &skill.text).await?;
            if rating >= 5 { continue; }
            // Ranking consumes body words too, including candidates not returned.
            crate::sources::depend_file(db, session, &skill.text).await?;
            risk = risk.max(rating);
            if skill.dir.starts_with(root().join("_proposed")) {
                skill.description = format!("(draft) {}", skill.description);
            }
            admitted.push(skill);
        }
        let found = find(&admitted, args["query"].as_str().unwrap_or(""), 8);
        let content = if found.is_empty() {
            format!("No skill matches. Available skills: {}", admitted.iter().map(|s| format!("{}/{}", s.domain, s.name)).collect::<Vec<_>>().join(", "))
        } else {
            found.iter().map(|(s, _)| format!("- {}/{}: {}", s.domain, s.name, s.description)).collect::<Vec<_>>().join("\n")
        };
        return Ok((content, false, risk));
    }
    let skill = match lookup(&all, args["name"].as_str().unwrap_or("")) {
        Ok(skill) => skill,
        Err(e) => return Ok((e, true, 1)),
    };
    let mut risk = crate::sources::file_rating(db, &skill.dir.join("SKILL.md"), &skill.text).await?;
    crate::sources::depend_file(db, session, &skill.text).await?;
    let file = args["file"].as_str().map(str::trim).filter(|f| !f.is_empty() && *f != "SKILL.md");
    let (path, text) = if let Some(file) = file {
        let path = skill.dir.join(file).canonicalize()?;
        anyhow::ensure!(path.starts_with(skill.dir.canonicalize()?), "file is outside this skill");
        let text = tokio::fs::read_to_string(&path).await?;
        risk = risk.max(crate::sources::file_rating(db, &path, &text).await?);
        crate::sources::depend_file(db, session, &text).await?;
        (path, text)
    } else {(skill.dir.join("SKILL.md"), skill.text.clone())};
    sqlx::query("INSERT INTO brain_skill_uses(session_id,name,digest) VALUES($1,$2,$3)")
        .bind(session).bind(format!("{}/{}",skill.domain,skill.name)).bind(crate::brain::hash(&skill.text)).execute(db).await?;
    Ok((render_load(skill, &path, text), false, risk))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> crate::test_util::TestDir {
        let root = crate::test_util::TestDir::new("skills");
        let mk = |domain: &str, name: &str, desc: &str, body: &str| {
            let d = root.join(domain).join(name);
            std::fs::create_dir_all(d.join("references")).unwrap();
            std::fs::write(d.join("SKILL.md"), format!("---\nname: {name}\ndescription: {desc}\n---\n\n{body}\n")).unwrap();
            d
        };
        mk("work", "verify", "Check that work is done before saying so. Use before reporting a change as finished.", "Run the tests.");
        let brief = mk("work", "brief", "Frame a big, risky or unclear job as a short brief. Use before changing anything on such a job.", "Write the goal.");
        std::fs::write(brief.join("references/template.md"), "Goal:\nCriteria:\n").unwrap();
        mk("build", "rust", "Rust conventions for this owner's crates.", "Use clippy.");
        // Invalid: the name doesn't match the folder.
        let bad = root.join("work").join("broken");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("SKILL.md"), "---\nname: other\ndescription: x\n---\nbody\n").unwrap();
        root
    }

    #[test]
    fn frontmatter_reads_simple_and_folded_values() {
        let (f, body) = frontmatter("---\nname: verify\ndescription: >\n  Check work\n  before reporting.\n---\n\n# Verify\n");
        assert_eq!(field(&f, "name"), Some("verify"));
        assert_eq!(field(&f, "description"), Some("Check work before reporting."));
        assert_eq!(body, "# Verify\n");
        let (f, body) = frontmatter("---\nname: x\nnote: a\n---x\n---\n- first item\n");
        assert_eq!(field(&f, "note"), Some("a"));
        assert_eq!(body, "- first item\n", "a leading list dash survives; `---x` is not a fence");
        let (f, body) = frontmatter("no frontmatter");
        assert!(f.is_empty() && body == "no frontmatter");
    }

    #[test]
    fn scan_skips_invalid_skills_and_validate_says_why() {
        let root = tree();
        let all = scan(&root);
        let names: Vec<String> = all.iter().map(|s| format!("{}/{}", s.domain, s.name)).collect();
        assert_eq!(names, ["build/rust", "work/brief", "work/verify"]);
        assert!(validate(&root.join("work/broken"))[0].contains("must match the folder"));
    }

    #[test]
    fn index_lists_skills_or_only_domains_when_long() {
        let all = scan(&tree());
        assert!(index_text(&all).contains("- work/verify: Check that work is done"));
        let short = index_within(&all, 50);
        assert_eq!(short, "- build: 1 skill(s)\n- work: 2 skill(s)\n");
        assert_eq!(index_text(&[]), "");
    }

    #[test]
    fn find_ranks_names_over_descriptions() {
        let all = scan(&tree());
        let found = find(&all, "verify the change", 5);
        assert_eq!(found[0].0.name, "verify");
        assert!(find(&all, "zzz qqq", 5).is_empty());
    }

    #[test]
    fn catalog_and_primary_load_use_the_validated_snapshot() {
        let root = tree();
        let all = scan(&root);
        let skill = lookup(&all, "work/brief").unwrap();
        std::fs::write(skill.dir.join("SKILL.md"), "---\nname: brief\ndescription: changed\n---\nREPLACEMENT TOKEN\n").unwrap();
        assert!(load(skill, None).unwrap().contains("Write the goal."));
        assert!(!load(skill, None).unwrap().contains("REPLACEMENT"));
        assert!(find(&all, "replacement", 8).is_empty());
        assert!(index_text(&all).contains("Frame a big"));
    }

    #[test]
    fn load_returns_the_skill_or_a_file_inside_it_only() {
        let root = tree();
        let all = scan(&root);
        let brief = lookup(&all, "work/brief").unwrap();
        let text = load(brief, None).unwrap();
        assert!(text.contains("Write the goal.") && text.contains("references/template.md"));
        assert!(load(brief, Some("references/template.md")).unwrap().contains("Criteria:"));
        assert!(load(brief, Some("../verify/SKILL.md")).is_err(), "no reading outside the skill");
        assert!(lookup(&all, "nope").is_err());
        assert_eq!(lookup(&all, "verify").unwrap().name, "verify");
    }
}
