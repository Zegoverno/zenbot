//! Read-only compatibility for flat wiki pages written before D-069. New knowledge uses brain
//! records. The owner migrates old pages with `zen brain migrate_legacy`; until then search and
//! origin ratings still understand their original format. No legacy capture writer remains.
use std::path::{Path, PathBuf};

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

/// Whether a page's text has an entry labelled `web` (from a session that read untrusted content).
pub fn has_web_entry(text: &str) -> bool {
    text.contains("(web) —")
}

pub fn page_times(dir: &Path) -> Vec<(String, std::time::SystemTime)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if p.extension().is_some_and(|x| x == "md") && name != "index" && name != "log" {
            let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            out.push((name, modified));
        }
    }
    out
}

/// Search documents (slug, title, identifiers, body, modified) for the pages `wanted(slug, modified)`
/// picks; only those are read and parsed.
pub fn documents_where(dir: &Path, wanted: impl Fn(&str, std::time::SystemTime) -> bool) -> Vec<(String, String, String, String, std::time::SystemTime)> {
    let mut out = Vec::new();
    for (slug, modified) in page_times(dir) {
        if !wanted(&slug, modified) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.join(format!("{slug}.md"))) else { continue };
        let p = parse(&slug, &text);
        let body = format!("{}\n{}", p.summary, p.entries.join("\n"));
        let ident = std::iter::once(p.slug.clone()).chain(p.aliases.iter().cloned()).collect::<Vec<_>>().join(" ");
        out.push((p.slug, p.title, ident, body, modified));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_page_keeps_titles_aliases_and_origin() {
        let text = "---\ntype: concept\naliases: [Quick thought]\n---\n# Thinking\n\nTwo systems.\n\n---\n## Timeline\n- **2026-10-01** | session x (web) — A claim.\n";
        let p = parse("thinking", text);
        assert_eq!(p.title, "Thinking");
        assert_eq!(p.aliases, ["Quick thought"]);
        assert_eq!(p.summary, "Two systems.");
        assert_eq!(p.entries.len(), 1);
        assert!(has_web_entry(text));
        assert_eq!(slug("Two Systems"), "two-systems");
    }
}
