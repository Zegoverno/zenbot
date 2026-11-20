//! The prompt files the owner edits from the web app: `GET /api/prompt-files/USER.md` and
//! `PUT` `{text}`. Only the files on the allow-list (`ALLOWED`: `USER.md`, which sits above the
//! vault where the app's Files view is read-only); the other prompt files have their own routes
//! (agents) or belong to the owner alone. A save backs up the previous version to
//! `<zen home>/backups/prompt-files/` as every prompt-file edit does (`memory::backup_file`) and
//! replaces the file atomically. Sessions read it when they start, so a change applies to new
//! sessions.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

/// The files this route serves, and nothing else.
pub const ALLOWED: [&str; 1] = ["USER.md"];

/// The most a prompt file may hold.
const MAX_BYTES: usize = 100_000;

fn path_in(home: &Path, name: &str) -> Result<PathBuf> {
    if !ALLOWED.contains(&name) {
        bail!("no prompt file `{name}` here; the files are {}", ALLOWED.join(", "));
    }
    Ok(home.join(name))
}

/// The file's text (empty when it doesn't exist yet) and how many characters the session's
/// instructions keep of it.
pub fn read(name: &str) -> Result<(String, usize)> {
    read_in(&crate::zen_home(), name)
}

fn read_in(home: &Path, name: &str) -> Result<(String, usize)> {
    let path = path_in(home, name)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => bail!("{}: {e}", path.display()),
    };
    Ok((text, crate::config::get().user_chars))
}

/// Replace the file's text; returns the backup of the old version (None when there was nothing
/// to back up or nothing changed).
pub fn write(name: &str, text: &str) -> Result<Option<PathBuf>> {
    write_in(&crate::zen_home(), name, text)
}

fn write_in(home: &Path, name: &str, text: &str) -> Result<Option<PathBuf>> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().map_err(|_| anyhow::anyhow!("prompt file lock poisoned"))?;
    let path = path_in(home, name)?;
    if text.len() > MAX_BYTES {
        bail!("{name} can be {} KB at most", MAX_BYTES / 1000);
    }
    if text.contains('\0') {
        bail!("{name} can't contain NUL characters");
    }
    // Never through a link: the rename would replace the link, not the file it points to.
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("{} is a symbolic link; edit its target", path.display());
    }
    let old = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => bail!("{}: {e}", path.display()),
    };
    if old.as_deref() == Some(text) {
        return Ok(None);
    }
    let backup = if old.is_some() { Some(crate::memory::backup_file(home, &path)?) } else { None };
    std::fs::create_dir_all(home)?;
    crate::tasks::write_atomic(&path, text).map_err(|e| anyhow::anyhow!("{}", e.message()))?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_allowed_file_is_served() {
        let home = crate::test_util::TestDir::new("prompt-files-api");
        for name in ["SOUL.md", "AGENTS.md", "../USER.md", "user.md", "USER.md/", "mcp.json", ""] {
            assert!(read_in(&home, name).is_err() && write_in(&home, name, "x").is_err(), "{name:?}");
        }
        assert!(!home.join("SOUL.md").exists());
    }

    #[test]
    fn a_save_backs_up_the_old_text_and_replaces_the_file() {
        let home = crate::test_util::TestDir::new("prompt-files-api");
        assert_eq!(read_in(&home, "USER.md").unwrap().0, "", "missing reads as empty");
        assert_eq!(write_in(&home, "USER.md", "first").unwrap(), None, "nothing to back up the first time");
        assert_eq!(std::fs::read_to_string(home.join("USER.md")).unwrap(), "first");
        let backup = write_in(&home, "USER.md", "second\n").unwrap().expect("the old text is backed up");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "first");
        assert!(backup.starts_with(home.join("backups/prompt-files")) && backup.file_name().unwrap().to_string_lossy().starts_with("USER-"));
        assert_eq!(read_in(&home, "USER.md").unwrap().0, "second\n");
        // Saving the same text again changes nothing and backs nothing up.
        assert_eq!(write_in(&home, "USER.md", "second\n").unwrap(), None);
        assert_eq!(std::fs::read_dir(home.join("backups/prompt-files")).unwrap().count(), 1);
        assert!(std::fs::read_dir(&*home).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")), "no temporary file left");
    }

    #[test]
    fn refuses_what_it_should_not_write() {
        let home = crate::test_util::TestDir::new("prompt-files-api");
        assert!(write_in(&home, "USER.md", &"x".repeat(100_001)).is_err());
        assert!(write_in(&home, "USER.md", "a\0b").is_err());
        let target = home.join("elsewhere.md");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, home.join("USER.md")).unwrap();
        assert!(write_in(&home, "USER.md", "new").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }
}
