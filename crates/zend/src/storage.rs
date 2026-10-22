//! Kernel-owned SQLite connections: durable WAL writes, bounded lock waits, native
//! UUID blobs, bundled vector operations, and immediate read/modify/write transactions.
//! Extension loading is never enabled. Security ancestry is expressed by schema views.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use sqlx::{
    Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{str::FromStr, time::Duration};

pub static MIGRATOR: std::sync::LazyLock<sqlx::migrate::Migrator> =
    std::sync::LazyLock::new(|| {
        let mut migrations = sqlx::migrate!("./sqlite-migrations");
        // Expand-only migrations allow a previous SQLite kernel to run after rollback.
        migrations.set_ignore_missing(true);
        migrations
    });

/// Open a private local database. An explicit URL must name SQLite, never a
/// legacy PostgreSQL source (migration is an explicit offline operation).
pub async fn connect(url: &str) -> Result<SqlitePool> {
    anyhow::ensure!(
        url.starts_with("sqlite:"),
        "DATABASE_URL must be a sqlite: URL; migrate PostgreSQL before starting this kernel"
    );
    let options = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(Duration::from_secs(15));
    let memory = url
        .split('?')
        .next()
        .is_some_and(|part| part.ends_with(":memory:"))
        || url
            .split_once('?')
            .is_some_and(|(_, query)| query.split('&').any(|part| part == "mode=memory"));
    if !memory {
        private_database(options.get_filename())?;
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .after_connect(|connection, _| {
            Box::pin(async move {
                let mut handle = connection.lock_handle().await?;
                // SAFETY: SQLx locks its worker for the lifetime of this guard. The
                // bundled initializer and scalar callbacks retain no connection pointer.
                unsafe {
                    register(handle.as_raw_handle().as_ptr())?;
                }
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .context("open SQLite database")?;
    MIGRATOR
        .run(&pool)
        .await
        .context("migrate SQLite database")?;
    Ok(pool)
}

/// Create missing directories privately and open without following database or
/// directory symlinks. Permission changes apply through the opened descriptor.
fn private_database(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "SQLite path must not contain symlinks: {}",
                ancestor.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .context("create private SQLite directory")?;
    let open = |create| {
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600);
        if create {
            options.create_new(true);
        }
        options.open(path)
    };
    let file = match open(true) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => open(false)?,
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "SQLite database must be a regular file"
    );
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Reserve the writer before reading state that the transaction will change.
pub async fn begin(pool: &SqlitePool) -> std::result::Result<Transaction<'_, Sqlite>, sqlx::Error> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

unsafe extern "C" {
    fn sqlite3_vec_init(
        db: *mut libsqlite3_sys::sqlite3,
        error: *mut *mut std::ffi::c_char,
        api: *const std::ffi::c_void,
    ) -> std::ffi::c_int;
}

unsafe fn register(db: *mut libsqlite3_sys::sqlite3) -> std::result::Result<(), sqlx::Error> {
    use libsqlite3_sys::*;
    let mut error = std::ptr::null_mut();
    let result = unsafe { sqlite3_vec_init(db, &mut error, std::ptr::null()) };
    if result != SQLITE_OK {
        let message = if error.is_null() {
            "sqlite-vec initialization failed".into()
        } else {
            let text = unsafe { std::ffi::CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            unsafe { sqlite3_free(error.cast()) };
            text
        };
        return Err(sqlx::Error::Protocol(message));
    }
    for (name, callback) in [
        (c"sha256_hex", sha256_hex as unsafe extern "C" fn(_, _, _)),
        (c"uuid_blob", uuid_blob as unsafe extern "C" fn(_, _, _)),
    ] {
        let code = unsafe {
            sqlite3_create_function_v2(
                db,
                name.as_ptr(),
                1,
                SQLITE_UTF8 | SQLITE_DETERMINISTIC | SQLITE_INNOCUOUS,
                std::ptr::null_mut(),
                Some(callback),
                None,
                None,
                None,
            )
        };
        if code != SQLITE_OK {
            return Err(sqlx::Error::Protocol(format!(
                "register SQLite function: {code}"
            )));
        }
    }
    Ok(())
}

unsafe extern "C" fn sha256_hex(
    context: *mut libsqlite3_sys::sqlite3_context,
    _: i32,
    values: *mut *mut libsqlite3_sys::sqlite3_value,
) {
    use libsqlite3_sys::*;
    unsafe {
        let value = *values;
        if sqlite3_value_type(value) == SQLITE_NULL {
            sqlite3_result_null(context);
            return;
        }
        let size = sqlite3_value_bytes(value) as usize;
        let pointer = sqlite3_value_blob(value).cast::<u8>();
        let bytes = if size == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(pointer, size)
        };
        let digest = format!("{:x}", Sha256::digest(bytes));
        sqlite3_result_text(
            context,
            digest.as_ptr().cast(),
            digest.len() as i32,
            SQLITE_TRANSIENT(),
        );
    }
}

unsafe extern "C" fn uuid_blob(
    context: *mut libsqlite3_sys::sqlite3_context,
    _: i32,
    values: *mut *mut libsqlite3_sys::sqlite3_value,
) {
    use libsqlite3_sys::*;
    unsafe {
        let value = *values;
        if sqlite3_value_type(value) == SQLITE_NULL {
            sqlite3_result_null(context);
            return;
        }
        let size = sqlite3_value_bytes(value) as usize;
        let pointer = sqlite3_value_text(value);
        let bytes = if size == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(pointer, size)
        };
        match std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
        {
            Some(id) => sqlite3_result_blob(
                context,
                id.as_bytes().as_ptr().cast(),
                16,
                SQLITE_TRANSIENT(),
            ),
            None => sqlite3_result_null(context),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn schema_enforces_provenance_and_dependency_cycles() -> Result<()> {
        let db = connect("sqlite::memory:").await?;
        let owner = uuid::Uuid::new_v4();
        let child = uuid::Uuid::new_v4();
        for id in [owner, child] {
            sqlx::query("INSERT INTO sessions(id,model,risk) VALUES(?, 'faux/smoke',3)")
                .bind(id)
                .execute(&db)
                .await?;
        }
        sqlx::query("INSERT INTO tape_events(session_id,kind,payload,origin,risk) VALUES(?,'message',?, 'owner',0)")
            .bind(owner).bind(serde_json::json!({"role":"user","content":"Remember this"})).execute(&db).await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO memories(text,session_id,risk) VALUES('Remember this',?,0) RETURNING id",
        )
        .bind(owner)
        .fetch_one(&db)
        .await?;
        let (risk, reason): (i16, String) =
            sqlx::query_as("SELECT risk,rating_reason FROM memories WHERE id=?")
                .bind(id)
                .fetch_one(&db)
                .await?;
        assert_eq!((risk, reason.as_str()), (0, "owner"));
        sqlx::query("UPDATE memories SET text='Model assertion',session_id=? WHERE id=?")
            .bind(child)
            .bind(id)
            .execute(&db)
            .await?;
        let risk: i16 = sqlx::query_scalar("SELECT risk FROM memories WHERE id=?")
            .bind(id)
            .fetch_one(&db)
            .await?;
        assert_eq!(risk, 3);
        for (reader, writer) in [(owner, child), (child, owner)] {
            sqlx::query("INSERT INTO session_inputs VALUES(?,?)")
                .bind(reader)
                .bind(writer)
                .execute(&db)
                .await?;
        }
        sqlx::query("INSERT INTO contamination(cause,affected) VALUES(?,?)")
            .bind(owner)
            .bind(owner)
            .execute(&db)
            .await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM zen_contaminated_sessions")
            .fetch_one(&db)
            .await?;
        assert_eq!(count, 2);
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM zen_contaminated_memories WHERE id=?")
                .bind(id)
                .fetch_one(&db)
                .await?;
        assert_eq!(count, 1);
        sqlx::query("UPDATE contamination SET cleared_at='2026-01-01T00:00:00Z'")
            .execute(&db)
            .await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM zen_contaminated_memories")
            .fetch_one(&db)
            .await?;
        assert_eq!(count, 0);
        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn vector_fts_functions_and_rollback() -> Result<()> {
        let db = connect("sqlite::memory:").await?;
        let cosine: f64 = sqlx::query_scalar("SELECT vec_distance_cosine('[1,0]','[0,1]')")
            .fetch_one(&db)
            .await?;
        assert_eq!(cosine, 1.0);
        let hash: String = sqlx::query_scalar("SELECT sha256_hex('abc')")
            .fetch_one(&db)
            .await?;
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let id = uuid::Uuid::new_v4();
        let decoded: uuid::Uuid = sqlx::query_scalar("SELECT uuid_blob(?)")
            .bind(id.to_string())
            .fetch_one(&db)
            .await?;
        assert_eq!(decoded, id);
        let mut tx = begin(&db).await?;
        sqlx::query("INSERT INTO search_docs(kind,ref,body) VALUES('memory','m1','Ação multilingual token')").execute(&mut *tx).await?;
        tx.rollback().await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM search_docs_fts WHERE search_docs_fts MATCH 'acao'",
        )
        .fetch_one(&db)
        .await?;
        assert_eq!(count, 0);
        sqlx::query("INSERT INTO search_docs(kind,ref,body) VALUES('memory','m1','Ação multilingual token')").execute(&db).await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM search_docs_fts WHERE search_docs_fts MATCH 'acao'",
        )
        .fetch_one(&db)
        .await?;
        assert_eq!(count, 1);
        db.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn transactions_rollback_queues_and_one_run_constraint() -> Result<()> {
        let db = connect("sqlite::memory:").await?;
        let session = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO sessions(id,model) VALUES(?,'faux/smoke')")
            .bind(session)
            .execute(&db)
            .await?;
        let mut tx = begin(&db).await?;
        sqlx::query("INSERT INTO tape_events(session_id,kind,payload) VALUES(?,'message','{}')")
            .bind(session)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO memories(text,session_id) VALUES('temporary',?)")
            .bind(session)
            .execute(&mut *tx)
            .await?;
        tx.rollback().await?;
        for table in [
            "tape_events",
            "tape_exports",
            "memories",
            "brain_memory_inputs",
            "search_queue",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&db)
                .await?;
            assert_eq!(count, 0, "rollback must cover {table}");
        }
        let job: i64 = sqlx::query_scalar("INSERT INTO jobs(name,kind,schedule,tz,created_by) VALUES('atomic','system','every 1h','UTC','kernel') RETURNING id").fetch_one(&db).await?;
        let run: i64 = sqlx::query_scalar(
            "INSERT INTO job_runs(job_id,trigger,status) VALUES(?,'owner','running') RETURNING id",
        )
        .bind(job)
        .fetch_one(&db)
        .await?;
        assert!(
            sqlx::query(
                "INSERT INTO job_runs(job_id,trigger,status) VALUES(?,'schedule','running')"
            )
            .bind(job)
            .execute(&db)
            .await
            .is_err()
        );
        sqlx::query("UPDATE job_runs SET status='ok' WHERE id=?")
            .bind(run)
            .execute(&db)
            .await?;
        sqlx::query("INSERT INTO job_runs(job_id,trigger,status) VALUES(?,'schedule','running')")
            .bind(job)
            .execute(&db)
            .await?;
        db.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn private_files_and_legacy_url_rejection() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!("zen-storage-{}", uuid::Uuid::new_v4()));
        let file = root.join("private/data.sqlite3");
        private_database(&file)?;
        assert_eq!(
            std::fs::metadata(&file)?.permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(file.parent().unwrap())?
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let alias = root.join("alias");
        symlink(&file, &alias)?;
        assert!(private_database(&alias).is_err());
        let alias_dir = root.join("alias-dir");
        symlink(file.parent().unwrap(), &alias_dir)?;
        assert!(private_database(&alias_dir.join("another.db")).is_err());
        assert!(connect("postgres://localhost/legacy").await.is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
