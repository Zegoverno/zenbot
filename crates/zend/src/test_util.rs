//! Scratch directories for tests: remove them even when an assertion panics.

use std::ops::Deref;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub(crate) struct TestDir(PathBuf);

impl TestDir {
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("zend-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&path).unwrap();
        Self(path)
    }
}

impl Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path { &self.0 }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path { &self.0 }
}

impl Drop for TestDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

/// A kernel state with no workers, for database tests of kernel functions that take an `App`
/// (`taint::deny`, `trust`, `hand_over_rating`). Events go nowhere; aborts reach no worker.
pub(crate) fn app(db: sqlx::PgPool) -> crate::AppState {
    use std::collections::{HashMap, HashSet};
    use tokio::sync::Mutex;
    std::sync::Arc::new(crate::App {
        db,
        workers: Vec::new(),
        ignored_workers: Vec::new(),
        routes: Mutex::new(HashMap::new()),
        catalog: Mutex::new(HashMap::new()),
        turns: Mutex::new(HashMap::new()),
        token: "test".into(),
        workspace: std::env::temp_dir(),
        repo: String::new(),
        default_model: "faux/smoke".into(),
        harness: "test".into(),
        hubs: Mutex::new(HashMap::new()),
        updater: std::sync::Arc::new(crate::update::Updater::new(std::env::temp_dir())),
        compacting: Mutex::new(HashSet::new()),
        finishing: Mutex::new(HashSet::new()),
        starting_prompts: Mutex::new(HashSet::new()),
        waiters: Mutex::new(HashMap::new()),
        engine_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(3)),
        child_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(3)),
        tool_order: Mutex::new(HashMap::new()),
        background: std::sync::Arc::default(),
    })
}
