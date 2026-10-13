#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
//! Unique temporary directories for filesystem tests. Removed on drop.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// A fresh unique standard-library temporary directory, removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    /// Creates a uniquely named directory under the process temp dir.
    #[must_use]
    pub(crate) fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("dal-store-{tag}-{}-{serial}", std::process::id()));
        fs::create_dir(&path).expect("create isolated test directory");
        Self(path)
    }

    /// Borrows the directory path.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
