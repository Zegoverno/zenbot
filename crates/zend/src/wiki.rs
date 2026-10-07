//! The wiki: structured notes zenbot keeps, and `capture` (DESIGN.md, "Memory and knowledge" →
//! Knowledge; Phase 4).
//!
//! Pages are markdown files in `~/.zenbot/wiki/` (ZEN_WIKI_DIR), in git, in gbrain's shape
//! (docs/research/memory-search-web.md §C): frontmatter, a title, a summary above a `---` that is
//! rewritten from the timeline, and below it an append-only timeline, newest first, each entry
//! dated with its source. `index.md` lists the pages (rebuilt by the kernel) and `log.md` records
//! every capture.
//!
//! `capture` takes a note: `search` finds candidate pages, System One picks the page (or a new one)
//! and says whether the note is already recorded or sensitive, and the kernel appends the dated
//! entry, updates the index and the log, and commits. Rewriting the summary is writing, so it's the
//! agent's job: the result says when the summary needs it. Without System One, an exact title or
//! alias match picks the page, else a new page. Notes from a session that read web content are
//! labelled `web`. The nightly sleep lints the wiki (missing summaries, broken links).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{tools, App};

const NO_SUMMARY: &str = "> [No summary yet: write one from the timeline.]";
const TYPES: [&str; 6] = ["concept", "entity", "decision", "playbook", "project", "person"];

/// Where the wiki lives: ZEN_WIKI_DIR, else `~/.zenbot/global/wiki` (shared by every agent).
pub fn root() -> PathBuf {
    std::env::var("ZEN_WIKI_DIR").map(PathBuf::from).unwrap_or_else(|_| crate::layout::global_dir().join("wiki"))
}

/// A page name from a title: lowercase letters, digits and hyphens.
pub fn slug(title: &str) -> String {
    let mut s = String::new();
    for c in title.trim().to_lowercase().chars() {
        if c.is_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_matches('-').to_string();
    let s: String = s.chars().take(60).collect();
    if s.is_empty() || s == "index" || s == "log" {
        format!("page-{s}")
    } else {
        s.trim_end_matches('-').to_string()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub slug: String,
    pub title: String,
    pub kind: String,
    pub aliases: Vec<String>,
    pub summary: String,
    pub entries: Vec<String>,
}

/// A new page's text.
pub fn new_page(title: &str, kind: &str, aliases: &[String], today: &str) -> String {
    format!(
        "---\ntype: {kind}\naliases: [{}]\nupdated: {today}\n---\n# {title}\n\n{NO_SUMMARY}\n\n---\n## Timeline\n",
        aliases.join(", ")
    )
}

/// Read a page: its frontmatter fields, title, summary (between the title and the `---` before the
/// timeline) and timeline entries.
pub fn parse(slug: &str, text: &str) -> Page {
    let (fields, body) = crate::skills::frontmatter(text);
    let get = |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let mut title = String::new();
    let mut summary = Vec::new();
    let mut entries = Vec::new();
    let mut part = 0; // 0 before the title, 1 summary, 2 after the separator
    for line in body.lines() {
        if part == 0 {
            if let Some(t) = line.strip_prefix("# ") {
                title = t.trim().to_string();
                part = 1;
            }
            continue;
        }
        if part == 1 && line.trim() == "---" {
            part = 2;
            continue;
        }
        if part == 1 {
            summary.push(line);
        } else if line.starts_with("- **") {
            entries.push(line.to_string());
        }
    }
    let aliases = get("aliases").trim_matches(|c| c == '[' || c == ']').split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    Page { slug: slug.to_string(), title: if title.is_empty() { slug.to_string() } else { title }, kind: get("type"), aliases, summary: summary.join("\n").trim().to_string(), entries }
}

/// Add a timeline entry (newest first: right under `## Timeline`) and set `updated`.
pub fn add_entry(text: &str, entry: &str, today: &str) -> String {
    let mut out = String::new();
    let mut added = false;
    let mut in_front = false;
    for (i, line) in text.lines().enumerate() {
        if i == 0 && line == "---" {
            in_front = true;
        } else if in_front && line == "---" {
            in_front = false;
        } else if in_front && line.starts_with("updated:") {
            out.push_str(&format!("updated: {today}\n"));
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if !added && line.trim() == "## Timeline" {
            out.push_str(entry);
            out.push('\n');
            added = true;
        }
    }
    if !added {
        out.push_str(&format!("\n---\n## Timeline\n{entry}\n"));
    }
    out
}

fn pages(dir: &Path) -> Vec<Page> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if p.extension().is_some_and(|x| x == "md") && name != "index" && name != "log" {
            if let Ok(text) = std::fs::read_to_string(&p) {
                out.push(parse(&name, &text));
            }
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// `index.md`: one line per page, the first line of its summary.
pub fn index_text(pages: &[Page]) -> String {
    let mut out = String::from("# Wiki index\n\nOne line per page; rebuilt by the kernel on every capture.\n\n");
    for p in pages {
        let line = p.summary.lines().map(|l| l.trim_start_matches('>').trim()).find(|l| !l.is_empty()).unwrap_or("");
        out.push_str(&format!("- [[{}]] {} ({}, {} entries): {}\n", p.slug, p.title, if p.kind.is_empty() { "page" } else { &p.kind }, p.entries.len(), zen_proto::head(line, 160)));
    }
    out
}

/// What's wrong with the wiki: pages without a summary, links to pages that don't exist.
pub fn lint(dir: &Path) -> Vec<String> {
    let all = pages(dir);
    let slugs: Vec<&str> = all.iter().map(|p| p.slug.as_str()).collect();
    let mut problems = Vec::new();
    for p in &all {
        if p.summary.contains("[No summary yet") && !p.entries.is_empty() {
            problems.push(format!("{}: no summary yet ({} entries)", p.slug, p.entries.len()));
        }
        let text = std::fs::read_to_string(dir.join(format!("{}.md", p.slug))).unwrap_or_default();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("[[") {
            let after = &rest[i + 2..];
            let Some(j) = after.find("]]") else { break };
            let target = after[..j].split('|').next().unwrap_or("").trim();
            if !target.is_empty() && !slugs.contains(&target) {
                problems.push(format!("{}: link to missing page [[{target}]]", p.slug));
            }
            rest = &after[j + 2..];
        }
    }
    problems
}

async fn git(dir: &Path, args: &[&str]) -> bool {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Make sure the wiki folder exists and is a git repository with an index and a log.
pub async fn ensure(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    if !dir.join(".git").exists() {
        git(dir, &["init", "-q"]).await;
    }
    if !dir.join("log.md").exists() {
        std::fs::write(dir.join("log.md"), "# Wiki log\n\nOne line per capture, newest last.\n\n")?;
    }
    if !dir.join("index.md").exists() {
        std::fs::write(dir.join("index.md"), index_text(&[]))?;
    }
    Ok(())
}

/// Commit everything in the wiki (captures, and pages the agent edited since).
pub async fn commit(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]).await;
    git(dir, &["-c", "user.name=zenbot", "-c", "user.email=zenbot@localhost", "commit", "-q", "-m", message]).await;
}

/// Which page a note goes to, as System One judges it: a candidate's slug or "new", whether the
/// page already records it, and whether it is sensitive. None when System One isn't available.
async fn route(app: &App, note: &str, candidates: &[Page]) -> Option<(String, f64, f64, f64)> {
    if crate::score::scorer().is_none() || !crate::score::private_ok() || candidates.is_empty() {
        return None;
    }
    let mut criteria = serde_json::Map::new();
    let mut pages = serde_json::Map::new();
    for p in candidates {
        criteria.insert(p.slug.clone(), json!(format!("{}: {}", p.title, zen_proto::head(&p.summary, 200))));
        pages.insert(p.slug.clone(), json!({ "title": p.title, "aliases": p.aliases, "summary": zen_proto::head(&p.summary, 600), "timeline": p.entries.iter().take(15).collect::<Vec<_>>() }));
    }
    criteria.insert("new".into(), json!("None of these pages is about it: it needs a page of its own"));
    let questions = json!({
        "page": { "type": "choice", "instructions": "Which wiki page is this note about? One page per concept, entity, decision or project.", "criteria": criteria },
        "known": { "type": "bool", "instructions": "Does the page you chose already record what this note says (in its summary or timeline)?",
                   "criteria": { "true": "Already recorded", "false": "Something new" } },
        "sensitive": { "type": "bool", "instructions": "Does the note hold secrets (passwords, keys, tokens) or private personal data that shouldn't be kept in notes?",
                       "criteria": { "true": "Sensitive", "false": "Fine to keep" } }
    });
    let state = json!({ "note": note, "pages": pages });
    let res = crate::score::decide(app, &state, &questions).await.ok()?;
    if !res["error"].is_null() {
        return None;
    }
    let a = &res["answers"];
    let page = a["page"]["choice"].as_str()?.to_string();
    let p = a["page"]["probabilities"][&page].as_f64().or(a["page"]["confidence"].as_f64()).unwrap_or(0.0);
    crate::agent::log_decision(&app.db, None, "capture", &json!({ "note": zen_proto::head(note, 300), "candidates": candidates.iter().map(|c| &c.slug).collect::<Vec<_>>() }), a, Some(&page), Some(p), true, None).await;
    Some((page, p, a["known"]["probability"].as_f64().unwrap_or(0.0), a["sensitive"]["probability"].as_f64().unwrap_or(0.0)))
}

async fn capture(app: &App, session: Uuid, args: &Value) -> Result<String> {
    let note = args["note"].as_str().map(str::trim).filter(|n| !n.is_empty()).context("capture needs a `note`")?;
    let note = crate::secrets::mask(note).replace('\n', " ");
    let title_masked = args["title"].as_str().map(|t| crate::secrets::mask(t.trim()));
    let title = title_masked.as_deref().filter(|t| !t.is_empty());
    let kind = args["type"].as_str().filter(|k| TYPES.contains(k)).unwrap_or("concept");
    let dir = root();
    ensure(&dir).await?;
    let all = pages(&dir);
    // Candidates: an exact title or alias match first, then what search finds.
    let wanted = title.unwrap_or(&note);
    let mut candidates: Vec<Page> = all
        .iter()
        .filter(|p| title.is_some_and(|t| p.title.eq_ignore_ascii_case(t) || p.aliases.iter().any(|a| a.eq_ignore_ascii_case(t)) || p.slug == slug(t)))
        .cloned()
        .collect();
    if let Err(e) = crate::search::index_once(&app.db).await {
        tracing::warn!("indexing before a capture: {e:#}");
    }
    for f in crate::search::query(&app.db, wanted, &["wiki"], None, 5).await.unwrap_or_default() {
        if let Some(p) = all.iter().find(|p| p.slug == f.reference) {
            if !candidates.iter().any(|c| c.slug == p.slug) {
                candidates.push(p.clone());
            }
        }
    }
    let exact = candidates.first().filter(|p| title.is_some_and(|t| p.title.eq_ignore_ascii_case(t) || p.slug == slug(t))).cloned();
    let routed = route(app, &note, &candidates).await;
    let (target, why) = match (&exact, &routed) {
        (Some(p), _) => (Some(p.clone()), "the title matches".to_string()),
        (None, Some((page, p, _, _))) if page != "new" && *p >= 0.5 => (candidates.iter().find(|c| &c.slug == page).cloned(), format!("System One: {p:.2}")),
        _ => (None, if routed.is_some() { "System One: no existing page fits".into() } else { "no page matches".into() }),
    };
    if let Some((_, _, known, _)) = &routed {
        if *known >= 0.8 && target.is_some() {
            let t = target.as_ref().map(|p| p.slug.clone()).unwrap_or_default();
            return Ok(format!("Not added: [[{t}]] already records this (System One: {known:.2}). Read it at {}.", dir.join(format!("{t}.md")).display()));
        }
    }
    let sensitive = routed.as_ref().is_some_and(|r| r.3 >= 0.8);
    if sensitive {
        anyhow::bail!("Not captured: System One judged the note sensitive (secrets or private data). Leave those out of the wiki; rephrase without them if the rest is worth keeping.");
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let tainted = crate::web::tainted(&app.db, session).await;
    let source = match args["source"].as_str().unwrap_or("inferred") {
        _ if tainted => "web",
        s @ ("owner" | "verified" | "inferred") => s,
        _ => "inferred",
    };
    let from = args["from"].as_str().map(|f| format!("{}, ", crate::secrets::mask(f))).unwrap_or_default();
    let entry = format!("- **{today}** | {from}session {} ({source}) — {note}", &session.to_string()[..8]);
    let (slug_, created) = match &target {
        Some(p) => (p.slug.clone(), false),
        None => {
            let t = title.map(String::from).unwrap_or_else(|| note.split_whitespace().take(6).collect::<Vec<_>>().join(" "));
            let mut s = slug(&t);
            let mut n = 2;
            while dir.join(format!("{s}.md")).exists() {
                s = format!("{}-{n}", slug(&t));
                n += 1;
            }
            let aliases: Vec<String> = args["aliases"].as_array().into_iter().flatten().filter_map(Value::as_str).map(crate::secrets::mask).collect();
            std::fs::write(dir.join(format!("{s}.md")), new_page(&t, kind, &aliases, &today))?;
            (s, true)
        }
    };
    let path = dir.join(format!("{slug_}.md"));
    let text = std::fs::read_to_string(&path)?;
    std::fs::write(&path, add_entry(&text, &entry, &today))?;
    let mut log = std::fs::read_to_string(dir.join("log.md")).unwrap_or_default();
    log.push_str(&format!("## [{today}] capture | {slug_}{}\n", if created { " (new page)" } else { "" }));
    std::fs::write(dir.join("log.md"), log)?;
    std::fs::write(dir.join("index.md"), index_text(&pages(&dir)))?;
    commit(&dir, &format!("capture: {slug_}")).await;
    let page = parse(&slug_, &std::fs::read_to_string(&path)?);
    let needs = if page.summary.contains("[No summary yet") || created {
        "The page has no summary yet: write one (a short paragraph: if someone reads only this, they know the state of play) in place of the placeholder, with edit."
    } else {
        "If this changes the state of play, rewrite the summary (above the `---`) with edit so it matches the timeline; leave the timeline as it is."
    };
    Ok(format!(
        "Captured to [[{slug_}]] ({}; {why}): {}\nSummary now:\n{}\n{needs}",
        if created { "new page" } else { "existing page" },
        path.display(),
        page.summary
    ))
}

pub fn spec() -> Value {
    json!({
        "name": "capture",
        "description": "Put a piece of knowledge into the wiki: your structured, lasting notes (one page per concept, entity, \
decision, playbook, project or person, each a summary over a dated timeline). Use it for what's worth knowing later and isn't \
about how the owner wants you to act (that's remember): what a library does, how a system works, why a decision was made, \
what a project's state is. The kernel picks the page (or makes one), skips what's already recorded and appends a dated entry \
with its source; you then keep the page's summary current with edit. Find pages with search (scope wiki); they live in \
~/.zenbot/global/wiki/. Never put secrets in the wiki.",
        "parameters": {
            "type": "object",
            "properties": {
                "note": { "type": "string", "description": "The knowledge, one to three self-contained sentences" },
                "title": { "type": "string", "description": "The page it's about, if you know (e.g. \"dom_smoothie\", \"D-033 MCP design\")" },
                "type": { "type": "string", "enum": TYPES, "description": "For a new page: what it is (default concept)" },
                "source": { "type": "string", "enum": ["owner", "verified", "inferred"], "description": "Where it comes from (default inferred)" },
                "from": { "type": "string", "description": "Optional: a URL, file or document it comes from" },
                "aliases": { "type": "array", "items": { "type": "string" }, "description": "For a new page: other names it goes by" }
            },
            "required": ["note"]
        }
    })
}

/// Run `capture`. None for other tools.
pub async fn run_tool(app: &App, session: Uuid, name: &str, args: &Value) -> Option<tools::ToolOutput> {
    if name != "capture" {
        return None;
    }
    Some(match capture(app, session, args).await {
        Ok(content) => tools::ToolOutput { content, is_error: false },
        Err(e) => tools::ToolOutput { content: format!("{e:#}"), is_error: true },
    })
}

/// Every page, for the search index: (slug, title, ident, body, modified).
pub fn documents(dir: &Path) -> Vec<(String, String, String, String, std::time::SystemTime)> {
    let mut out = Vec::new();
    for p in pages(dir) {
        let path = dir.join(format!("{}.md", p.slug));
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        let body = format!("{}\n{}", p.summary, p.entries.join("\n"));
        let ident = std::iter::once(p.slug.clone()).chain(p.aliases.iter().cloned()).collect::<Vec<_>>().join(" ");
        out.push((p.slug, p.title, ident, body, modified));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_plain() {
        assert_eq!(slug("D-033: MCP behind fixed tools!"), "d-033-mcp-behind-fixed-tools");
        assert_eq!(slug("index"), "page-index");
        assert_eq!(slug("  dom_smoothie "), "dom-smoothie");
    }

    #[test]
    fn a_page_round_trips_and_entries_go_on_top() {
        let text = new_page("dom_smoothie", "concept", &["readability".into()], "2026-10-06");
        let text = add_entry(&text, "- **2026-10-06** | session 1 (owner) — First.", "2026-10-06");
        let text = add_entry(&text, "- **2026-10-07** | session 2 (inferred) — Second.", "2026-10-07");
        let p = parse("dom-smoothie", &text);
        assert_eq!(p.title, "dom_smoothie");
        assert_eq!(p.aliases, ["readability"]);
        assert_eq!(p.summary, NO_SUMMARY);
        assert_eq!(p.entries.len(), 2);
        assert!(p.entries[0].contains("Second"), "newest first");
        assert!(text.contains("updated: 2026-10-07"));
        assert!(index_text(&[p]).contains("- [[dom-smoothie]] dom_smoothie (concept, 2 entries): [No summary yet"));
    }

    #[test]
    fn lint_finds_missing_summaries_and_broken_links() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("zend-wiki-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let a = add_entry(&new_page("A", "concept", &[], "2026-10-07"), "- **2026-10-07** | x — y", "2026-10-07");
        std::fs::write(dir.join("a.md"), a).unwrap();
        std::fs::write(dir.join("b.md"), "---\ntype: concept\n---\n# B\n\n> B is fine. See [[a]] and [[nowhere]].\n\n---\n## Timeline\n").unwrap();
        let problems = lint(&dir);
        assert_eq!(problems, ["a: no summary yet (1 entries)", "b: link to missing page [[nowhere]]"]);
    }
}
