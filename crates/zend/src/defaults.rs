//! The prompt files and skills a fresh install starts with (`crates/zend/defaults/`). The kernel
//! writes each one that is missing when it starts, and never overwrites one that exists: from then
//! on they are the owner's (prompt files) or the agent's (skills) to change.

use std::path::Path;

/// (path under ~/.zenbot, contents)
const FILES: &[(&str, &str)] = &[
    ("SOUL.md", include_str!("../defaults/SOUL.md")),
    ("AGENTS.md", include_str!("../defaults/AGENTS.md")),
    ("USER.md", include_str!("../defaults/USER.md")),
    ("skills/work/brief/SKILL.md", include_str!("../defaults/skills/work/brief/SKILL.md")),
    ("skills/work/brief/references/template.md", include_str!("../defaults/skills/work/brief/references/template.md")),
    ("skills/work/verify/SKILL.md", include_str!("../defaults/skills/work/verify/SKILL.md")),
];

/// Write the default files missing under `home`. A default skill is written only when its whole
/// folder is missing, so a skill the agent reshaped (or removed a file from) is left alone.
pub fn install(home: &Path) -> Vec<String> {
    let mut written = Vec::new();
    for (rel, text) in FILES {
        let path = home.join(rel);
        if let Some(skill_dir) = skill_dir_of(home, rel) {
            if skill_dir.exists() && !written.iter().any(|w: &String| home.join(w).starts_with(&skill_dir)) {
                continue;
            }
        }
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("creating {}: {e}", parent.display());
                continue;
            }
        }
        match std::fs::write(&path, text) {
            Ok(()) => written.push(rel.to_string()),
            Err(e) => tracing::warn!("writing {}: {e}", path.display()),
        }
    }
    written
}

/// For `skills/<domain>/<name>/…`, the skill's folder.
fn skill_dir_of(home: &Path, rel: &str) -> Option<std::path::PathBuf> {
    let mut parts = rel.split('/');
    (parts.next()? == "skills").then_some(())?;
    let (domain, name) = (parts.next()?, parts.next()?);
    Some(home.join("skills").join(domain).join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_missing_files_and_never_overwrites() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let home = std::env::temp_dir().join(format!("zend-defaults-{nanos}"));
        std::fs::create_dir_all(home.join("skills/work/verify")).unwrap();
        std::fs::write(home.join("skills/work/verify/SKILL.md"), "the agent's own version").unwrap();
        std::fs::write(home.join("USER.md"), "mine").unwrap();
        let written = install(&home);
        assert!(written.contains(&"SOUL.md".to_string()));
        assert!(written.contains(&"skills/work/brief/references/template.md".to_string()), "a new skill is written whole");
        assert_eq!(std::fs::read_to_string(home.join("USER.md")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(home.join("skills/work/verify/SKILL.md")).unwrap(), "the agent's own version");
        assert!(install(&home).is_empty(), "a second run writes nothing");
        // A skill folder the agent emptied of a file stays as it is.
        std::fs::remove_file(home.join("skills/work/brief/references/template.md")).unwrap();
        assert!(install(&home).is_empty());
    }
}
