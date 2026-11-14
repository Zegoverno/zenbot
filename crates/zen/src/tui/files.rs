//! The folder tree of the Files view (`views.rs`, on the home screen and in the side panel): which
//! folders are open, which row is selected, and what is listed. Listing is lazy: a folder is read
//! only while it is expanded. Files that hold secrets (`secret_paths`) are never listed, and
//! neither pane opens them. The tree also creates files and folders and renames rows (`create`,
//! `rename`): one name segment, never overwriting, never inside the secret paths, and the home
//! folder and zenbot's own top folders (`~/.zenbot` and two levels down) are protected.

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
    /// The user's home and zenbot's home: renaming either, or anything at most two levels
    /// below zenbot's home (`~/.zenbot/global`, `~/.zenbot/agents/zenbot`), is refused.
    pub home: PathBuf,
    pub zen_home: PathBuf,
}

/// A name that is one path segment: not empty, no `/` or NUL, not `.` or `..`.
fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains(['/', '\0']) || name == "." || name == ".." {
        return Err(format!("not a valid name: {name:?}"));
    }
    Ok(())
}

/// `path` with its parent's links resolved (the file itself may not exist, or be a link).
fn normal(path: &Path) -> PathBuf {
    match (path.parent().and_then(|p| std::fs::canonicalize(p).ok()), path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

impl Files {
    pub fn new(root: PathBuf) -> Files {
        Files::with_secrets(root, default_secrets())
    }

    pub fn with_secrets(root: PathBuf, secrets: Vec<PathBuf>) -> Files {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let mut f = Files { expanded: HashSet::from([root.clone()]), root, rows: Vec::new(), sel: 0, scroll: 0, hidden: false, secrets, home, zen_home: crate::client::zen_home() };
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

    /// The folder a new file or folder goes in: the selected folder, else the selected file's
    /// folder, else the tree's root.
    pub fn target_dir(&self) -> PathBuf {
        match self.selected() {
            Some(r) if r.dir => r.path.clone(),
            Some(r) => r.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        }
    }

    /// The home folder, `~/.zenbot` and what is at most two levels below it can't be renamed
    /// (zenbot's own layout: `global`, `global/wiki`, `agents/zenbot`).
    pub fn protected(&self, path: &Path) -> bool {
        let p = normal(path);
        p == normal(&self.home) || p.strip_prefix(normal(&self.zen_home)).is_ok_and(|rel| rel.components().count() <= 2)
    }

    /// The path `name` makes in `dir`, if the tree may have it: one valid segment, outside the
    /// secret paths, and not a name the tree never lists.
    fn check_target(&mut self, dir: &Path, name: &str) -> Result<PathBuf, String> {
        check_name(name)?;
        let path = dir.join(name);
        if is_secret(&path, &self.secrets) || is_secret(&normal(&path), &self.secrets) {
            return Err(format!("{name}: not allowed (secrets are never shown)"));
        }
        if SKIP.contains(&name) {
            return Err(format!("{name}: the tree never lists that name"));
        }
        Ok(path)
    }

    /// Show `path`, a new or renamed row: open its folder (and the ones above), list it again and
    /// select it.
    fn reveal(&mut self, path: &Path) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with('.') {
            self.hidden = true;
        }
        for a in path.ancestors().skip(1).take_while(|a| a.starts_with(&self.root)) {
            self.expanded.insert(a.to_path_buf());
        }
        self.rebuild();
        if let Some(i) = self.rows.iter().position(|r| r.path == path) {
            self.sel = i;
        }
    }

    /// Create the file or folder `name` in `dir` (which must be a folder) and select it. Never
    /// overwrites; a folder is made without its parents.
    pub fn create(&mut self, dir: &Path, name: &str, folder: bool) -> Result<PathBuf, String> {
        let path = self.check_target(dir, name)?;
        if !std::fs::metadata(dir).is_ok_and(|m| m.is_dir()) {
            return Err(format!("{}: not a folder", dir.display()));
        }
        let made = if folder { std::fs::create_dir(&path) } else { std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| ()) };
        made.map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { format!("{name}: already exists") } else { format!("{}: {e}", path.display()) })?;
        self.reveal(&path);
        Ok(path)
    }

    /// Rename `from` to `name` in its own folder and select it; refuses protected paths and
    /// never replaces anything.
    pub fn rename(&mut self, from: &Path, name: &str) -> Result<PathBuf, String> {
        if self.protected(from) {
            return Err(format!("{}: protected", from.display()));
        }
        let dir = from.parent().ok_or_else(|| format!("{}: protected", from.display()))?;
        let to = self.check_target(dir, name)?;
        if std::fs::symlink_metadata(&to).is_ok() {
            return if to == from { Err(format!("{name}: that is already its name")) } else { Err(format!("{name}: already exists")) };
        }
        std::fs::rename(from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
        // Folders open inside the renamed one stay open.
        let moved = |p: &PathBuf| match p.strip_prefix(from) {
            Ok(rest) if rest.as_os_str().is_empty() => to.clone(),
            Ok(rest) => to.join(rest),
            Err(_) => p.clone(),
        };
        self.expanded = self.expanded.iter().map(moved).collect();
        self.reveal(&to);
        Ok(to)
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
    fn create_makes_a_file_or_a_folder_never_overwrites_and_selects_the_new_row() {
        let d = tree();
        let mut f = Files::with_secrets(d.path().to_path_buf(), vec![]);
        let src = d.join("src");
        let made = f.create(&src, "new.md", false).unwrap();
        assert!(made.is_file() && std::fs::read(&made).unwrap().is_empty());
        assert_eq!(f.selected().unwrap().path, made, "the new row is selected, and its folder opened");
        assert!(names(&f).contains(&"  new.md".to_string()), "{:?}", names(&f));
        let sub = f.create(&src, "sub", true).unwrap();
        assert!(sub.is_dir() && f.selected().unwrap().path == sub && f.selected().unwrap().dir);
        std::fs::write(&made, "keep").unwrap();
        assert!(f.create(&src, "new.md", false).unwrap_err().contains("already exists"));
        assert!(f.create(&src, "sub", true).unwrap_err().contains("already exists"));
        assert_eq!(std::fs::read_to_string(&made).unwrap(), "keep", "never overwritten");
        for bad in ["", ".", "..", "a/b", "a\0b"] {
            assert!(f.create(&src, bad, false).unwrap_err().contains("not a valid name"), "{bad:?}");
        }
        assert!(f.create(&d.join("README.md"), "x", false).unwrap_err().contains("not a folder"));
        assert!(f.create(&d.join("missing"), "x", false).is_err() && !d.join("missing").exists(), "no parents are made");
        assert!(f.create(&src, "node_modules", true).is_err());
        // A dotfile shows even when dotfiles are off.
        let dot = f.create(&src, ".env.local", false).unwrap();
        assert!(f.hidden && f.selected().unwrap().path == dot);
    }

    #[test]
    fn the_target_folder_is_the_selected_folder_else_the_files_folder_else_the_root() {
        let d = tree();
        let mut f = Files::with_secrets(d.path().to_path_buf(), vec![]);
        assert_eq!(f.target_dir(), d.join("src"), "a folder: inside it");
        f.expand();
        f.sel = 2;
        assert_eq!(f.selected().unwrap().path, d.join("src/main.rs"));
        assert_eq!(f.target_dir(), d.join("src"), "a file: its folder");
        f.rows.clear();
        assert_eq!(f.target_dir(), d.path(), "nothing selected: the root");
    }

    #[test]
    fn rename_stays_in_its_folder_refuses_overwrites_and_protects_zenbots_top_folders() {
        let d = tree();
        let mut f = Files::with_secrets(d.path().to_path_buf(), vec![]);
        f.zen_home = d.join("zen");
        f.home = d.join("user");
        f.expand();
        let to = f.rename(&d.join("src/main.rs"), "lib.rs").unwrap();
        assert!(to == d.join("src/lib.rs") && to.is_file() && !d.join("src/main.rs").exists());
        assert_eq!(f.selected().unwrap().path, to, "kept selected");
        std::fs::write(d.join("src/other.rs"), "other").unwrap();
        assert!(f.rename(&to, "other.rs").unwrap_err().contains("already exists"));
        assert_eq!(std::fs::read_to_string(d.join("src/other.rs")).unwrap(), "other");
        for bad in ["", ".", "..", "../x", "a/b"] {
            assert!(f.rename(&to, bad).unwrap_err().contains("not a valid name"), "{bad:?}");
        }
        // An open folder keeps its open sub-folders when renamed.
        let sub = f.rename(&d.join("src"), "code").unwrap();
        assert!(f.expanded.contains(&sub) && names(&f).contains(&"  lib.rs".to_string()), "{:?}", names(&f));
        // Under zenbot's home: two levels are protected, the third isn't.
        for p in ["zen", "zen/global", "zen/global/wiki", "zen/agents/zenbot"] {
            std::fs::create_dir_all(d.join(p)).unwrap();
            assert!(f.rename(&d.join(p), "x").unwrap_err().contains("protected"), "{p}");
        }
        std::fs::create_dir_all(d.join("user")).unwrap();
        assert!(f.rename(&d.join("user"), "x").unwrap_err().contains("protected"), "the home folder");
        d.file("zen/global/wiki/page.md", "p");
        assert!(!f.protected(&d.join("zen/global/wiki/page.md")));
        assert!(f.rename(&d.join("zen/global/wiki/page.md"), "page2.md").is_ok());
        assert!(!f.protected(&d.join("src")), "outside zenbot's home: free");
    }

    #[test]
    fn names_the_tree_would_never_show_are_refused() {
        let d = TempDir::new("deny");
        let mut f = Files::with_secrets(d.path().to_path_buf(), secret_paths(d.path(), &d.join("home")));
        assert!(f.create(d.path(), "token", false).unwrap_err().contains("not allowed"));
        assert!(f.create(d.path(), "matrix", true).unwrap_err().contains("not allowed"));
        assert!(!d.join("token").exists());
        d.file("notes.md", "n");
        assert!(f.rename(&d.join("notes.md"), "env").unwrap_err().contains("not allowed"));
        assert!(d.join("notes.md").exists());
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
