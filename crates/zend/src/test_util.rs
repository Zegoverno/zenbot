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
