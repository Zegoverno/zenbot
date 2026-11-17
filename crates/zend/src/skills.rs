//! Skills: how to do a kind of work well, loaded on demand (the agentskills.io format).
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
        // `_archived` isn't a domain (workshop.rs).
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

/// The skills index for the system prompt: every skill by name with what it's for, in
/// `ZEN_SKILL_INDEX_CHARS` (default 2500) when it can be (`index_within`). Empty when there are no
/// skills.
pub fn index_text(skills: &[Skill]) -> String {
    index_within(skills, crate::config::get().skill_index_chars)
}

/// The shortest description that still says when to use a skill: its first sentence or clause
/// (up to the first `. `, ` - `, ` — ` or `; `), without the closing full stop.
fn trigger(description: &str) -> &str {
    let d = description.trim();
    let cut = [". ", " - ", " — ", "; "].iter().filter_map(|sep| d.find(sep)).min().unwrap_or(d.len());
    d[..cut].trim_end_matches('.').trim_end()
}

/// `text` in at most `max` characters, cut at a word with `…` when longer.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    // The last whole word that leaves room for the `…`.
    let head: String = text.chars().take(max).collect();
    let head = match head.rfind(' ').filter(|&i| i > 0) {
        Some(i) => head[..i].to_string(),
        None => head.chars().take(max.saturating_sub(1)).collect(),
    };
    format!("{}…", head.trim_end_matches([',', ';', ':', ' ']))
}

/// The skills index in `limit` characters, always naming every skill (never just counts), in the
/// first form that fits: full descriptions; each cut to its trigger (`trigger`); the triggers
/// clipped to an equal share of what's left; names only, grouped by domain (even past `limit`).
fn index_within(skills: &[Skill], limit: usize) -> String {
    /// Below this a clipped trigger says too little to choose by; names alone are clearer.
    const MIN_TRIGGER: usize = 24;
    if skills.is_empty() {
        return String::new();
    }
    let lines = |describe: &dyn Fn(&Skill) -> String| -> String { skills.iter().map(|s| format!("- {}/{}: {}\n", s.domain, s.name, describe(s))).collect() };
    let fits = |text: &String| text.chars().count() <= limit;
    let full = lines(&|s| s.description.trim().to_string());
    if fits(&full) {
        return full;
    }
    let short = lines(&|s| trigger(&s.description).to_string());
    if fits(&short) {
        return short;
    }
    let names: usize = skills.iter().map(|s| format!("- {}/{}: \n", s.domain, s.name).chars().count()).sum();
    let share = limit.saturating_sub(names) / skills.len();
    if share >= MIN_TRIGGER {
        let clipped = lines(&|s| clip(trigger(&s.description), share));
        if fits(&clipped) {
            return clipped;
        }
    }
    let mut domains: Vec<(&str, Vec<&str>)> = Vec::new();
    for s in skills {
        match domains.iter_mut().find(|(d, _)| *d == s.domain) {
            Some((_, names)) => names.push(&s.name),
            None => domains.push((&s.domain, vec![&s.name])),
        }
    }
    domains.iter().map(|(d, names)| format!("- {d}: {}\n", names.join(", "))).collect()
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
        "description": "Find skills by task; returns names and descriptions. Load matching instructions with load_skill.",
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
        "description": "Load and follow a skill by domain/name. file reads a supporting file instead of SKILL.md; loaded instructions remain in this conversation.",
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

/// Every valid skill under these folders, sorted; a skill in an earlier folder wins over one of
/// the same domain and name in a later one (an agent's own skills over the shared ones).
pub fn scan_roots(roots: &[PathBuf]) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    for root in roots {
        for s in scan(root) {
            if !out.iter().any(|o| o.domain == s.domain && o.name == s.name) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| (&a.domain, &a.name).cmp(&(&b.domain, &b.name)));
    out
}

/// `find_skills` and `load_skill` over the skills in `roots`: the text and whether it's an error.
pub async fn run_tool(db: &sqlx::SqlitePool, session: uuid::Uuid, roots: &[PathBuf], name: &str, args: &Value) -> (String, bool) {
    let all = scan_roots(roots);
    if name == "find_skills" {
        let found = find(&all, args["query"].as_str().unwrap_or(""), 8);
        let content = if found.is_empty() {
            format!("No skill matches. Available skills: {}", all.iter().map(|s| format!("{}/{}", s.domain, s.name)).collect::<Vec<_>>().join(", "))
        } else {
            found.iter().map(|(s, _)| format!("- {}/{}: {}", s.domain, s.name, s.description)).collect::<Vec<_>>().join("\n")
        };
        return (content, false);
    }
    let skill = match lookup(&all, args["name"].as_str().unwrap_or("")) {
        Ok(skill) => skill,
        Err(e) => return (e, true),
    };
    let file = args["file"].as_str().map(str::trim).filter(|f| !f.is_empty() && *f != "SKILL.md");
    let (path, text) = if let Some(file) = file {
        let read = async {
            let path = skill.dir.join(file).canonicalize()?;
            anyhow::ensure!(path.starts_with(skill.dir.canonicalize()?), "file is outside this skill");
            let text = tokio::fs::read_to_string(&path).await?;
            Ok((path, text))
        };
        match read.await {
            Ok(found) => found,
            Err(e) => return (format!("cannot load {file}: {e}"), true),
        }
    } else {
        (skill.dir.join("SKILL.md"), skill.text.clone())
    };
    if let Err(e) = sqlx::query("INSERT INTO brain_skill_uses(session_id,name,digest) VALUES($1,$2,$3)")
        .bind(session).bind(format!("{}/{}", skill.domain, skill.name)).bind(crate::brain::hash(&skill.text)).execute(db).await
    {
        tracing::warn!("recording a skill use: {e:#}");
    }
    (render_load(skill, &path, text), false)
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
    fn an_agents_own_skill_wins_over_a_shared_one() {
        let shared = tree();
        let own = crate::test_util::TestDir::new("own-skills");
        let d = own.join("work").join("verify");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), "---\nname: verify\ndescription: Garry's own way to check work.\n---\n\nAsk the hard question.\n").unwrap();
        let all = scan_roots(&[own.to_path_buf(), shared.to_path_buf()]);
        let names: Vec<String> = all.iter().map(|s| format!("{}/{}", s.domain, s.name)).collect();
        assert_eq!(names, ["build/rust", "work/brief", "work/verify"], "one of each, sorted");
        assert_eq!(lookup(&all, "work/verify").unwrap().description, "Garry's own way to check work.");
        assert_eq!(scan_roots(&[own.to_path_buf()]).len(), 1, "without the shared ones, only its own");
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
    fn index_always_names_every_skill() {
        let all = scan(&tree());
        let full = index_within(&all, 10_000);
        assert!(full.contains("- work/verify: Check that work is done"));
        assert_eq!(full.lines().count(), 3);
        // Too long for full descriptions: each cut to its trigger, every skill still named.
        let full_len = full.chars().count();
        let short = index_within(&all, full_len - 1);
        assert_eq!(short.lines().count(), 3);
        assert!(short.chars().count() < full_len);
        for s in &all {
            assert!(short.contains(&format!("- {}/{}: {}\n", s.domain, s.name, trigger(&s.description))));
        }
        // Tighter: triggers clipped to an equal share, every skill still named.
        let clipped = index_within(&all, 140);
        assert!(clipped.chars().count() <= 140 && clipped.contains('…'), "{clipped}");
        assert!(all.iter().all(|s| clipped.contains(&format!("- {}/{}: ", s.domain, s.name))));
        // Tightest: names only, by domain. Never counts.
        let names = index_within(&all, 50);
        assert_eq!(names, "- build: rust\n- work: brief, verify\n");
        assert!(!names.contains("skill(s)"));
        assert_eq!(index_text(&[]), "");
    }

    #[test]
    fn triggers_and_clips() {
        assert_eq!(trigger("Frame a big job before changing anything - find the real job. Use when large."), "Frame a big job before changing anything");
        assert_eq!(trigger("Write the changelog line for a note. Used by zenwrite."), "Write the changelog line for a note");
        assert_eq!(trigger("One clause only."), "One clause only");
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("check that work is really done", 16), "check that work…");
        assert!(clip("check that work is really done", 16).chars().count() <= 16);
        assert_eq!(clip("unbreakablewordhere", 8), "unbreak…");
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
