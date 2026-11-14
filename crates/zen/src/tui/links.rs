//! Links and paths: the links in a file's rendered rows (`[[wiki link]]`, `[text](path)`), the
//! file each points at, and what a click in the conversation points at (a link, a `code` span or
//! a word, resolved to a file or folder).

use super::*;
use std::path::Path;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A link in a file: where it points and where it shows (row, and byte range in the row's text;
/// more than one span when it wraps).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Link {
    pub(super) target: String,
    /// `[[wiki link]]` (found by name in the vault) rather than `[text](path)`.
    pub(super) wiki: bool,
    pub(super) spans: Vec<(usize, usize, usize)>,
}

/// The links in rendered rows: those the reader showed by their text (`Sty::Link`, targets in
/// `table`), and any still written out (`find_links`), in the order they show.
pub(super) fn find_links_in(lines: &[Line], table: &[(String, bool)]) -> Vec<Link> {
    let mut by: Vec<Option<Link>> = vec![None; table.len()];
    for (r, l) in lines.iter().enumerate() {
        let mut at = 0;
        for (t, s) in l {
            if let Sty::Link(i) = s {
                if let Some((target, wiki)) = table.get(*i as usize) {
                    let k = by[*i as usize].get_or_insert_with(|| Link { target: target.clone(), wiki: *wiki, spans: Vec::new() });
                    k.spans.push((r, at, at + t.len()));
                }
            }
            at += t.len();
        }
    }
    let mut out: Vec<Link> = by.into_iter().flatten().chain(find_links(lines)).collect();
    out.sort_by_key(|l| (l.spans[0].0, l.spans[0].1));
    out
}

/// The links still written out in rendered rows (`[[page]]`, `[text](path)`). The rows are read
/// as one text, so a link that wraps is found whole; the whitespace a wrap leaves inside its
/// target becomes one space.
pub(super) fn find_links(lines: &[Line]) -> Vec<Link> {
    let mut text = String::new();
    let mut starts = Vec::new();
    for l in lines {
        starts.push(text.len());
        for (t, _) in l {
            text.push_str(t);
        }
        text.push('\n');
    }
    let spans = |a: usize, b: usize| -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for (r, st) in starts.iter().enumerate() {
            let end = starts.get(r + 1).map(|n| n - 1).unwrap_or(text.len());
            let (x, y) = (a.max(*st), b.min(end));
            if x < y {
                out.push((r, x - st, y - st));
            }
        }
        out
    };
    let clean = |t: &str| -> String {
        t.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if let Some(inner) = rest.strip_prefix("[[") {
            if let Some(e) = inner.find("]]").filter(|e| !inner[..*e].contains('[')) {
                let target = clean(&inner[..e]);
                if !target.is_empty() {
                    out.push(Link { target, wiki: true, spans: spans(i, i + e + 4) });
                }
                i += e + 4;
                continue;
            }
        }
        if rest.starts_with('[') {
            // [text](target), the text on one paragraph.
            if let Some(m) = rest.find("](").filter(|m| !rest[1..*m].contains(['[', ']']) && !rest[1..*m].contains("\n\n")) {
                if let Some(e) = rest[m + 2..].find(')') {
                    // A path has no spaces (`%20` stands for one), so a wrap inside it is dropped.
                    let target: String = rest[m + 2..m + 2 + e].split_whitespace().collect();
                    if !target.is_empty() {
                        out.push(Link { target, wiki: false, spans: spans(i, i + m + 3 + e) });
                        i += m + 3 + e;
                        continue;
                    }
                }
            }
        }
        i += rest.chars().next().map(char::len_utf8).unwrap_or(1);
    }
    out
}

/// The file a link from `from` points at. A wiki link is found by name anywhere in the vault
/// (the nearest folder above `from` with `.obsidian`, else `from`'s folder), as Obsidian does,
/// nearest first; `[text](path)` is a path from `from`'s folder. Web links aren't files.
pub(super) fn resolve_link(link: &Link, from: &Path) -> std::result::Result<PathBuf, String> {
    let dir = from.parent().unwrap_or(Path::new("."));
    if !link.wiki {
        let t = link.target.split('#').next().unwrap_or("");
        if t.contains("://") || t.starts_with("mailto:") {
            return Err(format!("{} is a web link", link.target));
        }
        let t = t.replace("%20", " ");
        let p = if t.starts_with("~/") { expand_home(&t) } else { dir.join(&t) };
        return if p.is_file() { Ok(p) } else { Err(format!("{} isn't a file", p.display())) };
    }
    let name = link.target.split(['|', '#', '^']).next().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err("a link to a heading in this file".into());
    }
    let root = from.ancestors().skip(1).find(|a| a.join(".obsidian").is_dir()).unwrap_or(dir).to_path_buf();
    let want = if Path::new(&name).extension().is_some() { name.to_lowercase() } else { format!("{}.md", name.to_lowercase()) };
    // `folder/Name` is a path from the vault's root.
    if name.contains('/') {
        let p = root.join(if want.ends_with(".md") && !name.ends_with(".md") { format!("{name}.md") } else { name.clone() });
        if p.is_file() {
            return Ok(p);
        }
    }
    let base = want.rsplit('/').next().unwrap_or(&want).to_string();
    let mut found: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.clone()];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 50_000 {
                break;
            }
            let p = e.path();
            let fname = e.file_name().to_string_lossy().to_lowercase();
            if fname.starts_with('.') {
                continue;
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(p);
            } else if fname == base {
                found.push(p);
            }
        }
    }
    // Nearest to `from` first: the most folders in common.
    found.sort_by_key(|p| std::cmp::Reverse(p.components().zip(from.components()).take_while(|(a, b)| a == b).count()));
    found.into_iter().next().ok_or_else(|| format!("no page named {name}"))
}

/// What a click at column `col` of row `r` in the conversation points at: a `[[wiki link]]` or
/// `[text](path)` (found across wrapped rows), else the `code` span under it (joined across a
/// wrap), else the word under it. None on a blank.
pub(super) fn target_at(rows: &[Line], r: usize, col: usize) -> Option<(String, bool)> {
    let row = rows.get(r)?;
    // The segment and byte under the column.
    let (mut w, mut at, mut seg) = (0, None, None);
    let mut byte = 0;
    'outer: for (i, (t, _)) in row.iter().enumerate() {
        for (b, g) in t.grapheme_indices(true) {
            let gw = UnicodeWidthStr::width(g).max(1);
            if col < w + gw {
                (at, seg) = (Some(byte + b), Some(i));
                break 'outer;
            }
            w += gw;
        }
        byte += t.len();
    }
    let (at, seg) = (at?, seg?);
    // Links, read over a few rows around so one that wraps is found whole.
    let first = r.saturating_sub(3);
    let window = &rows[first..(r + 4).min(rows.len())];
    for l in find_links(window) {
        if l.spans.iter().any(|(lr, a, b)| first + lr == r && (*a..*b).contains(&at)) {
            return Some((l.target, l.wiki));
        }
    }
    let text = |l: &Line| l.iter().map(|(t, _)| t.as_str()).collect::<String>();
    if row[seg].1 == Sty::Code {
        // The run of code segments around the one clicked (a span may come in pieces).
        let is_code = |(t, s): &(String, Sty)| *s == Sty::Code || t.is_empty();
        let a = row[..seg].iter().rposition(|x| !is_code(x)).map(|i| i + 1).unwrap_or(0);
        let b = row[seg..].iter().position(|x| !is_code(x)).map(|i| seg + i).unwrap_or(row.len());
        let mut code = row[a..b].iter().map(|(t, _)| t.as_str()).collect::<String>().trim().to_string();
        let seg = (a, b);
        // A span cut by the wrap goes on at the start of the next row, or came from the last.
        let lead = |l: &Line| l.iter().take_while(|x| is_code(x)).map(|(t, _)| t.as_str()).collect::<String>();
        let tail = |l: &Line| {
            let i = l.iter().rposition(|x| !is_code(x)).map(|i| i + 1).unwrap_or(0);
            l[i..].iter().map(|(t, _)| t.as_str()).collect::<String>()
        };
        if seg.1 == row.len() {
            let next = rows.get(r + 1).map(lead).unwrap_or_default();
            if !next.trim().is_empty() {
                code = format!("{code} {}", next.trim());
            }
        }
        if row[..seg.0].iter().all(|(t, _)| t.trim().is_empty()) {
            let prev = r.checked_sub(1).map(|p| tail(&rows[p])).unwrap_or_default();
            if !prev.trim().is_empty() {
                code = format!("{} {code}", prev.trim());
            }
        }
        return (!code.is_empty()).then_some((code, false));
    }
    let t = text(row);
    if t[at..].starts_with(char::is_whitespace) {
        return None;
    }
    let start = t[..at].rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
    let end = t[at..].find(char::is_whitespace).map(|i| at + i).unwrap_or(t.len());
    let word = t[start..end].trim_matches(|c: char| "\"'`()<>,;".contains(c)).trim_end_matches(['.', ':']);
    (!word.is_empty()).then(|| (word.to_string(), false))
}

/// The file or folder `raw` names, as written in the conversation: `~/…`, absolute, or relative
/// to one of `bases` (the vault first); a `:line` after it is dropped and `.md` may be left off.
pub(super) fn resolve_path(raw: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    let mut t = raw.trim().replace("%20", " ");
    // `file.rs:12` or `file.rs:12:3`: the file.
    while let Some((head, tail)) = t.rsplit_once(':') {
        if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        t = head.to_string();
    }
    if t.is_empty() || t.contains("://") {
        return None;
    }
    let p = expand_home(&t);
    let tries: Vec<PathBuf> = if p.is_absolute() { vec![p] } else { bases.iter().map(|b| b.join(&p)).collect() };
    tries.into_iter().flat_map(|p| {
        let md = p.with_file_name(format!("{}.md", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        [p, md]
    }).find(|p| p.exists())
}

/// A path the owner typed, with a leading `~` meaning the home folder.
pub(super) fn expand_home(raw: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    match raw.strip_prefix("~/") {
        Some(rest) => PathBuf::from(&home).join(rest),
        None if raw == "~" => PathBuf::from(&home),
        None => PathBuf::from(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::*;

    #[test]
    fn a_click_finds_the_link_code_span_or_word_under_it() {
        let rows: Vec<Line> = vec![
            vec![("see ".into(), Sty::Plain), ("~/vault/wiki/Economic".into(), Sty::Code)],
            vec![("".into(), Sty::Plain), ("moat".into(), Sty::Code), (".md".into(), Sty::Code), (" and [[Alpha".into(), Sty::Plain)],
            vec![("decay]] or notes/a.md:12, then".into(), Sty::Plain)],
        ];
        // A code span cut by the wrap is one path, clicked on either row.
        let code = Some(("~/vault/wiki/Economic moat.md".to_string(), false));
        assert_eq!(target_at(&rows, 0, 6), code);
        assert_eq!(target_at(&rows, 1, 2), code);
        // A wiki link that wraps, from either half.
        assert_eq!(target_at(&rows, 1, 12), Some(("Alpha decay".to_string(), true)));
        assert_eq!(target_at(&rows, 2, 2), Some(("Alpha decay".to_string(), true)));
        // A bare word, its punctuation off; a blank or past the end, nothing.
        assert_eq!(target_at(&rows, 2, 14), Some(("notes/a.md:12".to_string(), false)));
        assert_eq!(target_at(&rows, 0, 3), None);
        assert_eq!(target_at(&rows, 0, 70), None);
        // Paths: relative to the bases in order, `.md` optional, a line number dropped.
        let dir = TempDir::new("chat-paths");
        let f = dir.file("notes/a.md", "x");
        let bases = [dir.join("nowhere"), dir.path().to_path_buf()];
        assert_eq!(resolve_path("notes/a.md:12", &bases), Some(f.clone()));
        assert_eq!(resolve_path("notes/a", &bases), Some(f.clone()));
        assert_eq!(resolve_path(&f.to_string_lossy(), &[]), Some(f));
        assert_eq!(resolve_path("notes", &bases), Some(dir.join("notes")));
        assert_eq!(resolve_path("then", &bases), None);
        assert_eq!(resolve_path("https://x.y/a.md", &bases), None);
    }
}
