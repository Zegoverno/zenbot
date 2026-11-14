//! The owner's wiki: the knowledge half of the second brain, an Obsidian vault folder (`wiki/`
//! under `global/`). Pages sit flat, named by their title (`Bayes' theorem.md`), with a YAML
//! frontmatter holding `type` (primitive, hub, source, fact, solution, decision, idea) and optional
//! `aliases` (inline `[a, b]` or a block list); the body starts with `# Title` and links inline with
//! `[[...]]`. The owner writes them in Obsidian and the agent with its ordinary file tools, following
//! the vault's AGENTS.md; the kernel only reads them, to index every page's whole body for search.
//! Knowledge lives here, not in brain records (see `brain`).
use std::path::{Path, PathBuf};

/// Where the wiki lives: ZEN_WIKI_DIR, else `~/.zenbot/global/wiki` (shared by every agent).
pub fn root() -> PathBuf {
    std::env::var("ZEN_WIKI_DIR").map(PathBuf::from).unwrap_or_else(|_| crate::layout::global_dir().join("wiki"))
}

/// A slug from a title: lowercase letters, digits and hyphens.
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
    /// The file stem, which is how Obsidian links to the page.
    pub name: String,
    pub title: String,
    pub kind: String,
    pub aliases: Vec<String>,
    /// Everything after the frontmatter.
    pub body: String,
}

/// Read a page: `type`, `aliases` (either YAML form), the first `# ` heading as its title (else the
/// file name) and the whole body after the frontmatter.
pub fn parse(name: &str, text: &str) -> Page {
    let (fields, body) = crate::skills::frontmatter(text);
    let head = &text[..text.len() - body.len()];
    let kind = fields.iter().find(|(f, _)| f == "type").map(|(_, v)| v.clone()).unwrap_or_default();
    let title = body.lines().find_map(|l| l.strip_prefix("# ")).map(str::trim).filter(|t| !t.is_empty()).unwrap_or(name);
    Page { name: name.to_string(), title: title.to_string(), kind, aliases: aliases(head), body: body.trim().to_string() }
}

/// `aliases: [a, b]`, `aliases: a`, or a block list of `- a` lines under `aliases:`.
fn aliases(head: &str) -> Vec<String> {
    let clean = |a: &str| a.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_string();
    let mut lines = head.lines().skip_while(|l| !l.starts_with("aliases:"));
    let Some(first) = lines.next() else { return vec![] };
    let inline = first["aliases:".len()..].trim();
    let items: Vec<String> = if inline.is_empty() {
        lines.map_while(|l| l.trim_start().strip_prefix("- ").filter(|_| l.starts_with([' ', '\t', '-']))).map(clean).collect()
    } else {
        inline.trim_start_matches('[').trim_end_matches(']').split(',').map(clean).collect()
    };
    items.into_iter().filter(|a| !a.is_empty()).collect()
}

/// The pages in the wiki folder (not its subfolders), as (name, modified).
pub fn page_times(dir: &Path) -> Vec<(String, std::time::SystemTime)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if p.is_file() && p.extension().is_some_and(|x| x == "md") && !name.starts_with('.') {
            let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            out.push((name, modified));
        }
    }
    out
}

/// Search documents (name, title, identifiers, body, modified) for the pages `wanted(name, modified)`
/// picks; only those are read and parsed.
pub fn documents_where(dir: &Path, wanted: impl Fn(&str, std::time::SystemTime) -> bool) -> Vec<(String, String, String, String, std::time::SystemTime)> {
    let mut out = Vec::new();
    for (name, modified) in page_times(dir) {
        if !wanted(&name, modified) {
            continue;
        }
        // Through `read_vault_file`: a page linked in from outside its vault is never read.
        let Ok(text) = crate::sandbox::read_vault_file(&dir.join(format!("{name}.md"))) else { continue };
        let p = parse(&name, &text);
        let ident = std::iter::once(p.name.clone()).chain(p.aliases.iter().cloned()).collect::<Vec<_>>().join(" ");
        out.push((p.name, p.title, ident, p.body, modified));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_reads_type_inline_aliases_and_the_whole_body() {
        let text = "---\ntype: primitive\naliases: [Bayes, \"Bayes rule\"]\n---\n# Bayes' theorem\n\nPrior times likelihood.\n\n---\n\nAfter the rule: the [[Base rate]].\n";
        let p = parse("Bayes' theorem", text);
        assert_eq!((p.title.as_str(), p.kind.as_str()), ("Bayes' theorem", "primitive"));
        assert_eq!(p.aliases, ["Bayes", "Bayes rule"]);
        assert!(p.body.starts_with("# Bayes' theorem") && p.body.ends_with("After the rule: the [[Base rate]]."), "text after a `---` line is kept");
        assert_eq!(slug("Two Systems"), "two-systems");
    }

    #[test]
    fn block_aliases_and_title_fallback() {
        let p = parse("Shape Up", "---\ntype: source\naliases:\n  - Shape Up book\n  - 'Basecamp method'\nauthor: Ryan\n---\nNo heading here.\n");
        assert_eq!(p.aliases, ["Shape Up book", "Basecamp method"]);
        assert_eq!((p.title.as_str(), p.body.as_str()), ("Shape Up", "No heading here."));
        let p = parse("Plain", "Just text, no frontmatter.");
        assert!(p.aliases.is_empty() && p.kind.is_empty() && p.title == "Plain");
    }

    #[test]
    fn pages_named_with_spaces_and_apostrophes_are_indexed() {
        let dir = std::env::temp_dir().join(format!("zen-wiki-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("concepts")).unwrap();
        std::fs::write(dir.join("Bayes' theorem.md"), "---\ntype: primitive\naliases:\n  - Bayes\n---\n# Bayes' theorem\n\nUpdate beliefs.\n").unwrap();
        std::fs::write(dir.join("concepts/record.md"), "---\n{}\n---\nA record, not a page.\n").unwrap();
        let docs = documents_where(&dir, |_, _| true);
        assert_eq!(docs.len(), 1);
        let (name, title, ident, body, _) = &docs[0];
        assert_eq!((name.as_str(), title.as_str(), ident.as_str()), ("Bayes' theorem", "Bayes' theorem", "Bayes' theorem Bayes"));
        assert!(body.contains("Update beliefs."));
        assert!(dir.join(format!("{name}.md")).is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
