//! Owner-run, deterministic migration of flat wiki pages. Preview by default; apply archives exact
//! original bytes and carries every writer and rating into provisional knowledge/source records.
//! Files precede index commits; originals leave the flat index only after a committed record.
//! Retrying never overwrites an existing record or an archive, including after a partial failure.
use super::{hash, records, string};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::path::{Path, PathBuf};

fn id(slug: &str) -> String {
    format!("legacy-wiki-{}", &hash(slug)[..24])
}

fn regular(path: &Path) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "not a regular file"
    );
    Ok(())
}

fn record(
    slug: &str,
    original: &Path,
    archive: &Path,
    text: &str,
    links: Vec<Value>,
) -> Result<Value> {
    let page = crate::wiki::parse(slug, text);
    let mut title = format!("Legacy wiki: {}", page.title);
    while title.len() > 240 {
        title.pop();
    }
    let r = json!({
        "schema_version": 1, "id": id(slug), "store": "knowledge", "kind": "source",
        "title": title, "status": "provisional", "revision": 1,
        "metadata": {
            "tags": ["legacy-wiki"], "aliases": [format!("legacy wiki: {slug}")],
            "evidence": [{"source": archive.display().to_string(), "locator": format!("entire original; sha256:{}", hash(text)), "basis": "external_claim"}],
            "links": links,
            "legacy": {"original_path": original.display().to_string(), "slug": slug,
                "digest": hash(text), "archive": archive.display().to_string(), "kind": page.kind, "aliases": page.aliases}
        },
        "body": text
    });
    records::validate(&r)?;
    Ok(r)
}

fn links(text: &str, slug: &str, names: &[String]) -> Vec<Value> {
    let mut out = vec![];
    let mut rest = text;
    while let Some((_, after)) = rest.split_once("[[") {
        let Some((name, tail)) = after.split_once("]]") else {
            break;
        };
        let target = name
            .split('|')
            .next()
            .unwrap_or("")
            .split('#')
            .next()
            .unwrap_or("")
            .trim();
        if target != slug && names.iter().any(|n| n == target) {
            let link = json!({"relation": "related_to", "target": id(target)});
            if !out.contains(&link) {
                out.push(link);
            }
        }
        rest = tail;
    }
    out
}

async fn provenance(
    db: &PgPool,
    original: &Path,
    old: &str,
    target: &Path,
    new: &str,
) -> Result<()> {
    let floor = crate::sources::file_rating(db, original, old).await?;
    for (writer, risk) in crate::sources::writers_of(db, old).await? {
        crate::sources::register_bytes(db, target, new, Some(writer), risk).await?;
    }
    crate::sources::register_bytes(db, target, new, None, floor).await
}

/// No text or error paths from this owner-only report enter agent instructions.
pub async fn migrate(db: &PgPool, apply: bool) -> Result<Value> {
    migrate_at(db, apply, &crate::wiki::root()).await
}

async fn migrate_at(db: &PgPool, apply: bool, root: &Path) -> Result<Value> {
    if !root.exists() {
        return Ok(json!({"apply": apply, "pages": [], "migrated": 0, "held": 0}));
    }
    ensure!(
        !std::fs::symlink_metadata(root)?.file_type().is_symlink(),
        "wiki root is a symlink"
    );
    let mut names: Vec<String> = crate::wiki::page_times(root)
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    names.sort();
    let mut targets = names.clone();
    // Already migrated targets remain resolvable on a later partial retry.
    for entry in std::fs::read_dir(root.join("sources"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            if let Ok(r) = records::parse(&text) {
                if let Some(slug) = r["metadata"]["legacy"]["slug"].as_str() {
                    targets.push(slug.into());
                }
            }
        }
    }
    let mut rows = vec![];
    let (mut migrated, mut held) = (0, 0);
    for slug in names {
        let source = root.join(format!("{slug}.md"));
        let result: Result<PathBuf> = async {
            regular(&source)?;
            ensure!(
                std::fs::metadata(&source)?.len() <= 64 * 1024,
                "page exceeds record body limit; split it manually"
            );
            let old = tokio::fs::read_to_string(&source).await?;
            let digest = hash(&old);
            let archive = root
                .join("_legacy")
                .join(format!("{}-{digest}.md", id(&slug)));
            let dest = root.join("sources").join(format!("{}.md", id(&slug)));
            for dir in [dest.parent().unwrap(), archive.parent().unwrap()] {
                if dir.exists() {
                    ensure!(
                        !std::fs::symlink_metadata(dir)?.file_type().is_symlink(),
                        "destination folder is a symlink"
                    );
                }
            }
            let r = record(&slug, &source, &archive, &old, links(&old, &slug, &targets))?;
            let mut text = records::render(&r)?;
            // Serialize with record puts and other migration calls. Preview also detects conflicts.
            let mut tx = db.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(704,4)")
                .execute(&mut *tx)
                .await?;
            if dest.exists() {
                regular(&dest)?;
                text = tokio::fs::read_to_string(&dest).await?;
                let prior = records::parse(&text)?;
                ensure!(
                    string(&prior, "id") == id(&slug)
                        && prior["metadata"]["legacy"]["digest"] == digest
                        && prior["metadata"]["legacy"]["original_path"]
                            == source.display().to_string(),
                    "existing record conflicts; left both files unchanged"
                );
            }
            if archive.exists() {
                regular(&archive)?;
                ensure!(
                    tokio::fs::read_to_string(&archive).await? == old,
                    "archive differs; left unchanged"
                );
            }
            if !apply {
                return Ok(dest);
            }
            if !archive.exists() {
                provenance(db, &source, &old, &archive, &old).await?;
                records::private_write(&archive, &old).await?;
            }
            if !dest.exists() {
                provenance(db, &source, &old, &dest, &text).await?;
                records::private_write(&dest, &text).await?;
            }
            // The index is rebuildable; the two file copies are not. Flush both files and
            // directory entries before committing/removing the flat original, including retry
            // copies written before a previous crash.
            for file in [&archive, &dest] {
                std::fs::File::open(file)?.sync_all()?;
                std::fs::File::open(file.parent().unwrap())?.sync_all()?;
            }
            std::fs::File::open(root)?.sync_all()?;
            records::index_file(&mut tx, &dest, "knowledge", &text).await?;
            sqlx::query("DELETE FROM search_docs WHERE kind='wiki' AND ref=$1")
                .bind(&slug)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            ensure!(
                tokio::fs::read_to_string(&source).await? == old,
                "original changed during migration; original retained"
            );
            tokio::fs::remove_file(&source)
                .await
                .context("record committed, but original could not be removed; retry safely")?;
            std::fs::File::open(root)?.sync_all()?;
            Ok(dest)
        }
        .await;
        match result {
            Ok(dest) => {
                if apply {
                    migrated += 1;
                }
                rows.push(json!({"slug": slug, "path": dest.display().to_string(), "status": if apply {"migrated"} else {"ready"}}));
            }
            Err(e) => {
                held += 1;
                rows.push(json!({"slug": slug, "status": "held", "error": format!("{e:#}")}));
            }
        }
    }
    Ok(json!({"apply": apply, "pages": rows, "migrated": migrated, "held": held}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_preserves_raw_notes_and_does_not_assert_their_truth() {
        let text = "---\ntype: person\naliases: [Ada]\n---\n# Ada\n\nUnstructured paragraph.\nSee [[math|Math]] and [[missing]].\n---\n## Timeline\n- **2026-10-01** | x (web) — A claim.\n";
        let r = record(
            "ada",
            Path::new("ada.md"),
            Path::new("_legacy/ada.md"),
            text,
            links(text, "ada", &["ada".into(), "math".into()]),
        )
        .unwrap();
        assert_eq!(r["body"], text);
        assert_eq!(r["kind"], "source");
        assert_eq!(r["status"], "provisional");
        assert_eq!(
            r["metadata"]["links"],
            json!([{"relation":"related_to", "target": id("math")}])
        );
        assert_eq!(r["metadata"]["legacy"]["aliases"], json!(["Ada"]));
        assert_eq!(r["metadata"]["evidence"][0]["basis"], "external_claim");
        assert_eq!(
            records::parse(&records::render(&r).unwrap()).unwrap()["body"],
            text.trim()
        );
    }
    /// Real database test, invoked by the native source e2e scenario.
    #[tokio::test]
    #[ignore]
    async fn legacy_migration_database() {
        let db = PgPool::connect(&std::env::var("SOURCE_TEST_DB_URL").unwrap())
            .await
            .unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        let dir = crate::test_util::TestDir::new("legacy-migration");
        let source = dir.join("page.md");
        let original = "# Legacy meeting\n\nTentative: the launch is Friday.\n";
        std::fs::write(&source, original).unwrap();
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        for writer in [a, b] {
            sqlx::query("INSERT INTO sessions(id,title,model,risk) VALUES($1,'migration writer','faux/smoke',4)")
                .bind(writer).execute(&db).await.unwrap();
            crate::sources::register_bytes(&db, &source, original, Some(writer), 4)
                .await
                .unwrap();
        }
        assert_eq!(
            migrate_at(&db, false, &dir).await.unwrap()["pages"][0]["status"],
            "ready"
        );
        assert!(!dir.join("sources").exists());
        assert_eq!(migrate_at(&db, true, &dir).await.unwrap()["migrated"], 1);
        let dest = dir.join("sources").join(format!("{}.md", id("page")));
        let text = std::fs::read_to_string(&dest).unwrap();
        let writers = crate::sources::writers_of(&db, &text).await.unwrap();
        assert!(writers.iter().any(|(w, _)| *w == a) && writers.iter().any(|(w, _)| *w == b));
        assert_eq!(
            crate::sources::file_rating(&db, &dest, &text)
                .await
                .unwrap(),
            4
        );
        let r = records::parse(&text).unwrap();
        assert_eq!(
            std::fs::read_to_string(r["metadata"]["legacy"]["archive"].as_str().unwrap()).unwrap(),
            original
        );
        let app = crate::test_util::app(db.clone());
        crate::taint::deny(&app, a, "migration provenance check")
            .await
            .unwrap();
        assert_eq!(
            crate::sources::file_rating(&db, &dest, &text)
                .await
                .unwrap(),
            5,
            "denial must survive migration for every original writer"
        );
        assert_eq!(migrate_at(&db, true, &dir).await.unwrap()["migrated"], 0);
    }
}
