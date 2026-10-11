//! The folder tree in the side panel's Files tab: which folders are open, which row is selected,
//! and what is listed. Listing is lazy: a folder is read only while it is expanded. Files that
//! hold secrets (`secret_paths`) are never listed, and the side panel won't open them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Never listed: huge, generated, or noise.
const SKIP: &[&str] = &[".git", "target", "node_modules", ".venv", "__pycache__"];

/// Most rows kept, so a huge folder can't stall a redraw.
const MAX_ROWS: usize = 5000;

/// Files in zenbot's home that hold secrets: the kernel's token, the database password, the
/// environment (API keys) and its previous copy, MCP servers' settings, the Matrix bridge's
/// login and recovery key. Kept in line with the kernel's `secrets::secret_files`.
const HOME_SECRETS: &[&str] = &["token", "db-password", "env", "env.prev", "mcp.json", "matrix.env", "matrix"];

/// Paths that hold secrets (a folder: everything in it), given zenbot's home and the user's home
/// (the engines' sign-ins).
pub fn secret_paths(zen_home: &Path, home: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = HOME_SECRETS.iter().map(|f| zen_home.join(f)).collect();
    paths.extend([home.join(".codex/auth.json"), home.join(".claude/.credentials.json")]);
    paths
}

/// The secret paths of this machine's zenbot, also as their real paths (a home reached through a
/// symlink).
pub fn default_secrets() -> Vec<PathBuf> {
    let paths = secret_paths(&crate::client::zen_home(), Path::new(&std::env::var_os("HOME").unwrap_or_default()));
    let real: Vec<PathBuf> = paths.iter().filter_map(|p| std::fs::canonicalize(p).ok()).filter(|r| !paths.contains(r)).collect();
    [paths, real].concat()
}

/// `path` is, or is inside, one of `secrets`.
pub fn is_secret(path: &Path, secrets: &[PathBuf]) -> bool {
    secrets.iter().any(|s| path.starts_with(s))
}

pub struct Row {
    pub path: PathBuf,
    pub depth: usize,
    pub dir: bool,
}

pub struct Files {
    pub root: PathBuf,
    pub expanded: HashSet<PathBuf>,
    pub rows: Vec<Row>,
    pub sel: usize,
    /// First row shown.
    pub scroll: usize,
    /// List dotfiles too.
    pub hidden: bool,
    /// Never listed (`secret_paths`).
    pub secrets: Vec<PathBuf>,
}

impl Files {
    pub fn new(root: PathBuf) -> Files {
        Files::with_secrets(root, default_secrets())
    }

    pub fn with_secrets(root: PathBuf, secrets: Vec<PathBuf>) -> Files {
        let mut f = Files { expanded: HashSet::from([root.clone()]), root, rows: Vec::new(), sel: 0, scroll: 0, hidden: false, secrets };
        f.rebuild();
        f
    }

    /// List the expanded folders again, keeping the selection on the same path when it still exists.
    pub fn rebuild(&mut self) {
        let keep = self.rows.get(self.sel).map(|r| r.path.clone());
        let mut rows = Vec::new();
        self.list(&self.root.clone(), 0, &mut rows);
        self.rows = rows;
        self.sel = keep.and_then(|p| self.rows.iter().position(|r| r.path == p)).unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
    }

    fn list(&self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut items: Vec<(PathBuf, bool)> = rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                // Symlinks are hidden like dotfiles: in ~/.zenbot they are the old layout's paths
                // kept for rollbacks (crates/zend/src/layout.rs), not files of their own.
                let link = e.file_type().is_ok_and(|t| t.is_symlink());
                !SKIP.contains(&name.as_str()) && (self.hidden || (!name.starts_with('.') && !link)) && !is_secret(&e.path(), &self.secrets)
            })
            .map(|e| (e.path(), e.path().is_dir()))
            .collect();
        // Folders first, then by name, ignoring case.
        items.sort_by_key(|(p, d)| (!*d, p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()));
        for (path, dir) in items {
            if out.len() >= MAX_ROWS {
                return;
            }
            let open = dir && self.expanded.contains(&path);
            out.push(Row { path: path.clone(), depth, dir });
            if open {
                self.list(&path, depth + 1, out);
            }
        }
    }

    pub fn selected(&self) -> Option<&Row> {
        self.rows.get(self.sel)
    }

    pub fn is_open(&self, r: &Row) -> bool {
        r.dir && self.expanded.contains(&r.path)
    }

    pub fn move_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.sel = (self.sel as isize + delta).clamp(0, last.max(0)) as usize;
    }

    /// Expand the selected folder; true when it was a closed folder.
    pub fn expand(&mut self) -> bool {
        match self.selected() {
            Some(r) if r.dir && !self.expanded.contains(&r.path) => {
                let p = r.path.clone();
                self.expanded.insert(p);
                self.rebuild();
                true
            }
            _ => false,
        }
    }

    /// Fold the selected folder if it is open, otherwise move to its parent folder.
    pub fn collapse_or_parent(&mut self) {
        let Some(r) = self.selected() else { return };
        if r.dir && self.expanded.contains(&r.path) {
            let p = r.path.clone();
            self.expanded.remove(&p);
            self.rebuild();
        } else if let Some(parent) = r.path.parent().map(Path::to_path_buf) {
            if let Some(i) = self.rows.iter().position(|x| x.path == parent) {
                self.sel = i;
            }
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        self.rebuild();
    }

    /// Keep the selected row inside a window of `rows` rows.
    pub fn follow(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + rows {
            self.scroll = self.sel + 1 - rows;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(rows));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::TempDir;

    fn tree() -> TempDir {
        let d = TempDir::new("files");
        std::fs::create_dir_all(d.join("target")).unwrap();
        for f in ["README.md", ".env", "src/main.rs", "src/deep/a.rs"] {
            d.file(f, "x");
        }
        d
    }

    fn names(f: &Files) -> Vec<String> {
        f.rows.iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.path.file_name().unwrap().to_string_lossy())).collect()
    }

    #[test]
    fn lists_folders_first_and_skips_noise_and_dotfiles() {
        let d = tree();
        let f = Files::new(d.path().to_path_buf());
        assert_eq!(names(&f), ["src", "README.md"]);
    }

    #[test]
    fn symlinks_are_hidden_like_dotfiles() {
        let d = tree();
        std::os::unix::fs::symlink("README.md", d.join("OLD.md")).unwrap();
        let mut f = Files::new(d.path().to_path_buf());
        assert_eq!(names(&f), ["src", "README.md"]);
        f.toggle_hidden();
        assert!(names(&f).contains(&"OLD.md".to_string()));
    }

    #[test]
    fn arrows_expand_collapse_and_walk_to_the_parent() {
        let d = tree();
        let mut f = Files::new(d.path().to_path_buf());
        assert!(f.expand());
        assert_eq!(names(&f), ["src", "  deep", "  main.rs", "README.md"]);
        f.move_by(1);
        assert!(f.expand());
        f.move_by(1);
        assert_eq!(names(&f)[f.sel], "    a.rs");
        f.collapse_or_parent(); // a file: up to its folder
        assert_eq!(names(&f)[f.sel], "  deep");
        f.collapse_or_parent(); // an open folder: fold it
        assert_eq!(names(&f), ["src", "  deep", "  main.rs", "README.md"]);
        f.move_by(-5);
        assert_eq!(f.sel, 0, "clamped at the top");
        f.move_by(99);
        assert_eq!(f.sel, 3, "clamped at the bottom");
        f.toggle_hidden();
        assert!(names(&f).contains(&".env".to_string()));
    }

    #[test]
    fn secret_files_are_never_listed() {
        let d = TempDir::new("secrets");
        for f in ["token", "db-password", "env", "env.prev", "matrix.env", "matrix/recovery-key", "config.toml", "skills/x.md"] {
            d.file(f, "s3cret");
        }
        let mut f = Files::with_secrets(d.path().to_path_buf(), secret_paths(d.path(), &d.join("home")));
        f.toggle_hidden();
        assert_eq!(names(&f), ["skills", "config.toml"]);
        assert!(is_secret(&d.join("matrix/recovery-key"), &f.secrets) && !is_secret(&d.join("tokens.md"), &f.secrets));
    }

    #[test]
    fn follow_keeps_the_selection_in_view() {
        let d = tree();
        let mut f = Files::new(d.path().to_path_buf());
        f.expand();
        f.sel = 3;
        f.follow(2);
        assert_eq!(f.scroll, 2);
        f.sel = 0;
        f.follow(2);
        assert_eq!(f.scroll, 0);
    }
}
