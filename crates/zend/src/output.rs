//! Shared limits for tool previews, capture and saved-output retention. Callers mask complete text
//! before previewing it, and keep its source rating when exposing a saved file.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// A shell's or MCP server's output past this shows its head and tail; the rest is saved.
pub(crate) const PREVIEW_BYTES: usize = 12 * 1024;
/// One `read` page: the model asked for these lines and pages on with `offset`.
pub(crate) const READ_BYTES: usize = 50 * 1024;
/// Any other tool's result past this (they bound and page their own) is cut the same way.
pub(crate) const RESULT_BYTES: usize = 64 * 1024;
pub(crate) const CAPTURE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Keep both the beginning and end so a command's final error remains visible. The marker is in
/// addition to the preview budget. None means the complete result already fits.
pub(crate) fn preview(text: &str) -> Option<String> {
    preview_at(text, PREVIEW_BYTES)
}

/// `preview` with another budget.
pub(crate) fn preview_at(text: &str, budget: usize) -> Option<String> {
    if text.len() <= budget {
        return None;
    }
    let half = budget / 2;
    let mut head = text.floor_char_boundary(half);
    if let Some(nl) = text[..head].rfind('\n') {
        head = nl + 1;
    }
    let mut tail = text.ceil_char_boundary(text.len() - half);
    if let Some(nl) = text[tail..].find('\n') {
        if tail + nl + 1 < text.len() {
            tail += nl + 1;
        }
    }
    Some(format!("{}\n[... {} bytes omitted from preview ...]\n\n{}", &text[..head], tail - head, &text[tail..]))
}

/// Persist a masked capture in an owner-only file. Mask before taking the byte limit so a secret
/// cannot become unrecognizable at the cut. A bounded capture explicitly identifies discarded data.
pub(crate) fn save_text(dir: &Path, text: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let masked = crate::secrets::mask(text);
    let bounded = if masked.len() > CAPTURE_BYTES {
        let end = masked.floor_char_boundary(CAPTURE_BYTES - 100);
        format!("{}\n[{} bytes discarded beyond saved capture limit]\n", &masked[..end], masked.len() - end)
    } else {
        masked
    };
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).ok()?;
    let metadata = std::fs::symlink_metadata(dir).ok()?;
    if !metadata.file_type().is_dir() { return None; }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
    let path = dir.join(format!("{nanos}.log"));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).ok()?;
    if std::io::Write::write_all(&mut file, bounded.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    prune_saved(dir, 30, TOTAL_BYTES);
    Some(path)
}

fn saved_name(name: &str) -> bool {
    name.ends_with(".log") || (name.starts_with("mcp-") && name.ends_with(".txt")) || (name.starts_with("web-") && name.ends_with(".pdf"))
}

/// Remove expired saved outputs, then oldest first until the total fits. Only recognized regular
/// files directly in the output directory count; symlinks and unrelated owner files stay untouched.
pub(crate) fn prune_saved(dir: &Path, days: u64, max_bytes: u64) -> usize {
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir()) { return 0; }
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let now = SystemTime::now();
    let age_limit = Duration::from_secs(days.saturating_mul(86_400));
    let mut files = Vec::new();
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) || !saved_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        if now.duration_since(modified).is_ok_and(|age| age > age_limit) && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        } else {
            files.push((modified, entry.path(), meta.len()));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut total = files.iter().fold(0u64, |sum, f| sum.saturating_add(f.2));
    for (_, path, len) in files {
        if total <= max_bytes { break; }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(len);
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_preserves_utf8_and_both_ends() {
        assert!(preview("small").is_none());
        let text = format!("first\n{}\nlast error", "🦀".repeat(PREVIEW_BYTES));
        let cut = preview(&text).unwrap();
        assert!(cut.starts_with("first\n"));
        assert!(cut.ends_with("last error"));
        assert!(cut.contains("bytes omitted from preview"));
        assert!(cut.len() < PREVIEW_BYTES + 100);
    }

    #[test]
    fn saved_capture_is_bounded_private_and_honest() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_util::TestDir::new("saved-output");
        let path = save_text(&dir, &"x".repeat(CAPTURE_BYTES + 20)).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.len() <= CAPTURE_BYTES);
        assert!(content.contains("bytes discarded beyond saved capture limit"));
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn save_refuses_symlink_directory_and_repairs_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root=crate::test_util::TestDir::new("output-dir");
        let real=root.join("real"); std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real,std::fs::Permissions::from_mode(0o755)).unwrap();
        save_text(&real,"capture").unwrap();
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,0o700);
        let link=root.join("link"); std::os::unix::fs::symlink(&real,&link).unwrap();
        assert!(save_text(&link,"capture").is_none());
        assert_eq!(prune_saved(&link,0,0),0);
    }

    #[test]
    fn retention_evicts_oldest_and_ignores_symlinks_and_owner_files() {
        let dir = crate::test_util::TestDir::new("output-budget");
        for name in ["old.log", "new.log", "notes.md"] {
            std::fs::write(dir.join(name), "12345678").unwrap();
        }
        let old = SystemTime::now() - Duration::from_secs(60);
        std::fs::File::options().write(true).open(dir.join("old.log")).unwrap().set_modified(old).unwrap();
        std::os::unix::fs::symlink(dir.join("notes.md"), dir.join("link.log")).unwrap();
        assert_eq!(prune_saved(&dir, 30, 8), 1);
        assert!(!dir.join("old.log").exists());
        assert!(dir.join("new.log").exists());
        assert!(dir.join("notes.md").exists());
        assert!(dir.join("link.log").symlink_metadata().unwrap().file_type().is_symlink());
    }
}
