//! Where zenbot's files live under its home (`~/.zenbot`, D-040). Split by scope, ready for more
//! agents:
//!
//! ```text
//! USER.md  AGENTS.md  mcp.json     system-wide: one owner, one environment
//! agents/<name>/SOUL.md            each agent's own files (one agent today: `zenbot`)
//! global/MEMORY.md wiki/ skills/ tools/   knowledge every agent shares
//! bin/ env token engine/ outputs/ ...      runtime (unchanged)
//! ```
//!
//! `migrate` moves an older flat layout into this one when the kernel starts, once, and leaves a
//! relative symlink at each old path, so a rolled-back build (which reads the old paths) still finds
//! the owner's files instead of writing fresh defaults. A later release removes the symlinks.

use std::path::{Path, PathBuf};

/// The agent's own prompt file, relative to the zen home (`agents/<name>/SOUL.md`; one agent,
/// `zenbot`, today).
pub const SOUL: &str = "agents/zenbot/SOUL.md";

/// Knowledge shared by every agent: `<zen home>/global`.
pub fn global_dir() -> PathBuf {
    crate::zen_home().join("global")
}

/// (old path, new path, the setting that moves it elsewhere): each relative to the zen home.
const MOVES: &[(&str, &str, Option<&str>)] = &[
    ("SOUL.md", SOUL, None),
    ("MEMORY.md", "global/MEMORY.md", None),
    ("wiki", "global/wiki", Some("ZEN_WIKI_DIR")),
    ("skills", "global/skills", Some("ZEN_SKILLS_DIR")),
    ("tools", "global/tools", Some("ZEN_TOOLS_DIR")),
];

/// Move the flat layout under `home` into the scoped one. Runs before the defaults are written, so
/// a default never takes the place of the owner's file. Never overwrites: when both the old and the
/// new path exist, both are left as they are and a warning says so. Returns what it did.
pub fn migrate(home: &Path) -> Vec<String> {
    let mut done = Vec::new();
    for (old, new, setting) in MOVES {
        // A folder moved elsewhere by a setting isn't read from the home, so it isn't moved either.
        if setting.is_some_and(|s| std::env::var_os(s).is_some()) {
            continue;
        }
        let (from, to) = (home.join(old), home.join(new));
        let Ok(meta) = std::fs::symlink_metadata(&from) else { continue }; // nothing at the old path
        if meta.file_type().is_symlink() {
            continue; // already moved
        }
        if to.exists() {
            tracing::warn!("{} and {} both exist; leaving both (move or merge by hand)", from.display(), to.display());
            continue;
        }
        let moved = to
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::rename(&from, &to))
            .and_then(|()| std::os::unix::fs::symlink(new, &from));
        match moved {
            Ok(()) => done.push(format!("{old} → {new}")),
            Err(e) => tracing::warn!("moving {} to {}: {e}", from.display(), to.display()),
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(name: &str) -> crate::test_util::TestDir {
        crate::test_util::TestDir::new(&format!("layout-{name}"))
    }

    #[test]
    fn the_flat_layout_moves_once_and_leaves_symlinks() {
        let h = home("move");
        std::fs::write(h.join("SOUL.md"), "my soul").unwrap();
        std::fs::write(h.join("USER.md"), "me").unwrap();
        std::fs::create_dir_all(h.join("wiki/.git")).unwrap();
        std::fs::write(h.join("wiki/page.md"), "a page").unwrap();
        std::fs::create_dir_all(h.join("skills/work/brief")).unwrap();
        let done = migrate(&h);
        assert_eq!(done, ["SOUL.md → agents/zenbot/SOUL.md", "wiki → global/wiki", "skills → global/skills"]);
        assert_eq!(std::fs::read_to_string(h.join("agents/zenbot/SOUL.md")).unwrap(), "my soul");
        assert!(h.join("global/wiki/.git").is_dir() && h.join("global/skills/work/brief").is_dir());
        // The old paths still lead to the same files (a rolled-back build reads them there).
        assert!(std::fs::symlink_metadata(h.join("SOUL.md")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(h.join("wiki")).unwrap(), PathBuf::from("global/wiki"));
        assert_eq!(std::fs::read_to_string(h.join("wiki/page.md")).unwrap(), "a page");
        // System-wide files stay; a second start changes nothing.
        assert_eq!(std::fs::read_to_string(h.join("USER.md")).unwrap(), "me");
        assert!(migrate(&h).is_empty());
    }

    #[test]
    fn nothing_is_overwritten_when_both_paths_exist() {
        let h = home("both");
        std::fs::write(h.join("SOUL.md"), "old soul").unwrap();
        std::fs::create_dir_all(h.join("agents/zenbot")).unwrap();
        std::fs::write(h.join("agents/zenbot/SOUL.md"), "new soul").unwrap();
        assert!(migrate(&h).is_empty());
        assert_eq!(std::fs::read_to_string(h.join("SOUL.md")).unwrap(), "old soul");
        assert_eq!(std::fs::read_to_string(h.join("agents/zenbot/SOUL.md")).unwrap(), "new soul");
    }

    #[test]
    fn a_fresh_home_has_nothing_to_move() {
        let h = home("fresh");
        assert!(migrate(&h).is_empty());
        assert!(!h.join("agents").exists() && !h.join("global").exists());
    }
}
