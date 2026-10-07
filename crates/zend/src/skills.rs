//! Skills: how to do a kind of work well, loaded on demand (DESIGN.md, "Skills and tools that
//! improve themselves"; the agentskills.io format).
//!
//! A skill is a folder `~/.zenbot/skills/<domain>/<name>/` with a `SKILL.md` (frontmatter `name` and
//! `description`, then the instructions) and optional `references/`, `scripts/` and `assets/`. The
//! session's fixed prefix carries only an index (`index_text`); the model finds skills with
//! `find_skills` and loads one with `load_skill`, whose text arrives as a tool result, so the prefix
//! and the prompt cache never change mid-session. Loads are recorded like any tool call
//! (`tool_calls`), which is how unused and overused skills are measured.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Largest SKILL.md body (or reference file) a load returns; the rest is cut with a note.
const MAX_LOAD: usize = 48 * 1024;
/// Above this size the index lists domains only, not every skill.
const DEFAULT_INDEX_CHARS: usize = 2500;

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub domain: String,
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

/// Where skills live: `ZEN_SKILLS_DIR`, else `~/.zenbot/skills`.
pub fn root() -> PathBuf {
    std::env::var("ZEN_SKILLS_DIR").map(PathBuf::from).unwrap_or_else(|_| crate::zen_home().join("skills"))
}

/// The frontmatter of a SKILL.md: simple `key: value` lines between `---` fences (a folded `>`
/// or `|` value continues on indented lines). Returns the fields and the body after the fences.
pub fn frontmatter(text: &str) -> (Vec<(String, String)>, &str) {
    let rest = match text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) {
        Some(r) => r,
        None => return (Vec::new(), text),
    };
    let Some(end) = rest.find("\n---") else { return (Vec::new(), text) };
    let head = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['\r', '\n', '-']);
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
    let mut errs = Vec::new();
    let Ok(text) = std::fs::read_to_string(dir.join("SKILL.md")) else {
        return vec![format!("{} has no SKILL.md", dir.display())];
    };
    let (fields, body) = frontmatter(&text);
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match field(&fields, "name") {
        None | Some("") => errs.push("frontmatter has no `name`".into()),
        Some(n) => {
            if n != folder {
                errs.push(format!("name `{n}` must match the folder name `{folder}`"));
            }
            if n.len() > 64 || !n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') || n.starts_with('-') || n.ends_with('-') || n.contains("--") {
                errs.push(format!("name `{n}`: lowercase letters, digits and single hyphens, at most 64 characters"));
            }
        }
    }
    match field(&fields, "description") {
        None | Some("") => errs.push("frontmatter has no `description` (what it does and when to use it)".into()),
        Some(d) if d.chars().count() > 1024 => errs.push("description is over 1024 characters".into()),
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
        if !dpath.is_dir() || domain.starts_with('.') {
            continue;
        }
        let Ok(skills) = std::fs::read_dir(&dpath) else { continue };
        for s in skills.flatten() {
            let dir = s.path();
            if !dir.join("SKILL.md").is_file() {
                continue;
            }
            let errs = validate(&dir);
            if !errs.is_empty() {
                tracing::warn!("skipping skill {}: {}", dir.display(), errs.join("; "));
                continue;
            }
            let text = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
            let (fields, _) = frontmatter(&text);
            out.push(Skill {
                domain: domain.clone(),
                name: field(&fields, "name").unwrap_or("").to_string(),
                description: field(&fields, "description").unwrap_or("").to_string(),
                dir,
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
    if skills.is_empty() {
        return String::new();
    }
    let limit = crate::env_num("ZEN_SKILL_INDEX_CHARS", DEFAULT_INDEX_CHARS as f64) as usize;
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
            let body = words(&std::fs::read_to_string(s.dir.join("SKILL.md")).unwrap_or_default());
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
    let mut text = std::fs::read_to_string(&path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
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
    Ok(out)
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
        "description": "Search your skills: written know-how for doing a kind of work well (how to frame a job, verify work, \
change zenbot, …). Returns matching skills with their descriptions; load one with load_skill. Use it when a task \
matches a skill in the index in your instructions, or when you're unsure how a kind of work should be done here. \
Skills are cheap to find and load; don't guess at a procedure a skill covers.",
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
        "description": "Load a skill's instructions (its SKILL.md), or one of its other files, into this conversation, then follow \
them. Name it as domain/name (from the index or find_skills). A skill stays loaded for the rest of the session; \
don't load the same file twice.",
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

/// Run `find_skills` or `load_skill`. None for other tools.
pub fn run_tool(name: &str, args: &Value) -> Option<(String, bool)> {
    let skills = || scan(&root());
    match name {
        "find_skills" => {
            let all = skills();
            if all.is_empty() {
                return Some((format!("No skills yet (none in {}).", root().display()), false));
            }
            let found = find(&all, args["query"].as_str().unwrap_or(""), 8);
            if found.is_empty() {
                let list: Vec<String> = all.iter().map(|s| format!("{}/{}", s.domain, s.name)).collect();
                return Some((format!("No skill matches. All skills: {}", list.join(", ")), false));
            }
            let lines: Vec<String> = found.iter().map(|(s, _)| format!("- {}/{}: {}", s.domain, s.name, s.description)).collect();
            Some((lines.join("\n"), false))
        }
        "load_skill" => {
            let all = skills();
            Some(match lookup(&all, args["name"].as_str().unwrap_or("")).and_then(|s| load(s, args["file"].as_str())) {
                Ok(text) => (text, false),
                Err(e) => (e, true),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("zend-skills-{nanos}"));
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
        std::env::set_var("ZEN_SKILL_INDEX_CHARS", "50");
        let short = index_text(&all);
        std::env::remove_var("ZEN_SKILL_INDEX_CHARS");
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
    fn load_returns_the_skill_or_a_file_inside_it_only() {
        let all = scan(&tree());
        let brief = lookup(&all, "work/brief").unwrap();
        let text = load(brief, None).unwrap();
        assert!(text.contains("Write the goal.") && text.contains("references/template.md"));
        assert!(load(brief, Some("references/template.md")).unwrap().contains("Criteria:"));
        assert!(load(brief, Some("../verify/SKILL.md")).is_err(), "no reading outside the skill");
        assert!(lookup(&all, "nope").is_err());
        assert_eq!(lookup(&all, "verify").unwrap().name, "verify");
    }
}
