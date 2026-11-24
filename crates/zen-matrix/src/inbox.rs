//! Files the owner sends from Matrix (photos, videos, voice notes, documents) land in
//! `~/inbox/<YYYY-MM-DD>/` (UTC), the folder the web chat's attachments use (web/server/uploads.ts),
//! so zenbot reads them with its own tools and the nightly upkeep clears them after 30 days.
//! The room gets the path back; a captioned file goes to the session as the web chat sends
//! attachments: the caption, then `(attached ~/inbox/…)`.
//!
//! Safety, as in uploads.ts: the name is cut to its last segment and to plain characters (no dot
//! files, no `..`, no separators), the file is created exclusively (never overwrites; a clash gets
//! `-2`, `-3`…) with mode 600, and the day folder must stay inside the inbox once symlinks are
//! followed.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Largest file the bridge saves: it is downloaded (and decrypted) in memory first.
pub const MAX_FILE: u64 = 200 * 1024 * 1024;

/// The inbox: `~/inbox`.
pub fn dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join("inbox")
}

/// A safe file name from what the client sent, or None when nothing usable is left.
pub fn clean_name(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = last
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c.is_alphanumeric() || " ._()+-".contains(c) { c } else { '_' })
        .collect();
    name = name.trim().trim_start_matches(['.', ' ']).split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }
    if name.chars().count() > 120 {
        let ext: String = ext_of(&name).chars().take(16).collect();
        let stem: String = name.chars().take(120 - ext.chars().count()).collect();
        name = format!("{stem}{ext}");
    }
    Some(name)
}

/// `.jpg` of `photo.jpg`; empty for none (a leading dot is not an extension).
fn ext_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i..],
        _ => "",
    }
}

/// A name for a file sent without one, from its kind and MIME type: `image.jpg`, `file`.
pub fn fallback_name(kind: &str, mime: Option<&str>) -> String {
    let ext = match mime.unwrap_or("") {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/heic" => ".heic",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "audio/ogg" => ".ogg",
        "audio/mpeg" => ".mp3",
        "audio/mp4" => ".m4a",
        "application/pdf" => ".pdf",
        _ => "",
    };
    format!("{kind}{ext}")
}

/// Today's day folder name, UTC (as the web app's `toISOString().slice(0, 10)`).
pub fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    day_of(secs)
}

fn day_of(unix: u64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Write `bytes` as `name` in the inbox's folder for `day`; returns the path written.
pub fn save(inbox: &Path, day: &str, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let name = clean_name(name).context("bad file name")?;
    if bytes.is_empty() {
        bail!("empty file");
    }
    if bytes.len() as u64 > MAX_FILE {
        bail!("file too large (over {} MB)", MAX_FILE / 1024 / 1024);
    }
    let dir = inbox.join(day);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let real_inbox = inbox.canonicalize()?;
    let real_dir = dir.canonicalize()?;
    if real_dir != real_inbox.join(day) {
        bail!("the inbox's day folder is not a plain folder");
    }
    let ext = ext_of(&name);
    let stem = &name[..name.len() - ext.len()];
    let stem = if stem.is_empty() { "file" } else { stem };
    for i in 1..1000 {
        let full = real_dir.join(if i == 1 { name.clone() } else { format!("{stem}-{i}{ext}") });
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(&full) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
                    let _ = std::fs::remove_file(&full);
                    return Err(e).with_context(|| format!("writing {}", full.display()));
                }
                return Ok(full);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", full.display())),
        }
    }
    bail!("too many files with that name today")
}

/// `~/inbox/…` for a path under the home folder, as prompts and notes show it.
pub fn tilde(path: &Path) -> String {
    let p = path.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && p.starts_with(&format!("{h}/")) => format!("~{}", &p[h.len()..]),
        _ => p,
    }
}

/// The prompt with its attachments named under it, as the web chat sends them.
pub fn with_attachments(text: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return text.to_string();
    }
    let list = paths.iter().map(|p| format!("(attached {p})")).collect::<Vec<_>>().join("\n");
    if text.trim().is_empty() {
        list
    } else {
        format!("{text}\n\n{list}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("IMG_0001.jpg").as_deref(), Some("IMG_0001.jpg"));
        assert_eq!(clean_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(clean_name("a\\b\\..\\.ssh").as_deref(), Some("ssh"));
        assert_eq!(clean_name("..").as_deref(), None);
        assert_eq!(clean_name("  ").as_deref(), None);
        assert_eq!(clean_name("relatório final?.pdf").as_deref(), Some("relatório final_.pdf"));
        assert_eq!(clean_name("x\u{0}y\n.txt").as_deref(), Some("xy.txt"));
        let long = format!("{}.jpeg", "a".repeat(200));
        let c = clean_name(&long).unwrap();
        assert_eq!(c.chars().count(), 120);
        assert!(c.ends_with(".jpeg"));
    }

    #[test]
    fn days_are_utc_dates() {
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(1_795_392_000), "2026-11-23");
        assert_eq!(day_of(951_782_400), "2000-02-29");
    }

    #[test]
    fn saving_never_overwrites_and_stays_inside() {
        let root = std::env::temp_dir().join(format!("zen-matrix-inbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a = save(&root, "2026-11-23", "photo.jpg", b"one").unwrap();
        let b = save(&root, "2026-11-23", "photo.jpg", b"two").unwrap();
        assert!(a.ends_with("2026-11-23/photo.jpg"));
        assert!(b.ends_with("2026-11-23/photo-2.jpg"));
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert!(save(&root, "2026-11-23", "x.txt", b"").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);
            // A day folder that is a symlink out of the inbox is refused.
            std::os::unix::fs::symlink(std::env::temp_dir(), root.join("2026-11-24")).unwrap();
            assert!(save(&root, "2026-11-24", "y.txt", b"y").is_err());
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn prompts_name_their_attachments() {
        let p = vec!["~/inbox/2026-11-23/a.jpg".to_string(), "~/inbox/2026-11-23/b.pdf".to_string()];
        assert_eq!(with_attachments("look", &p), "look\n\n(attached ~/inbox/2026-11-23/a.jpg)\n(attached ~/inbox/2026-11-23/b.pdf)");
        assert_eq!(with_attachments("", &p[..1]), "(attached ~/inbox/2026-11-23/a.jpg)");
        assert_eq!(with_attachments("plain", &[]), "plain");
        assert_eq!(fallback_name("image", Some("image/jpeg")), "image.jpg");
        assert_eq!(fallback_name("file", None), "file");
    }
}
