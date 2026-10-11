#![expect(dead_code, reason = "shared support compiled per test target")]
#![expect(
    clippy::struct_excessive_bools,
    reason = "fixed durability failpoint switches"
)]
//! Test-side file writer with named durability failpoints over real paths.
//!
//! `FaultSink` performs real filesystem writes and fails at configured
//! points, so recovery tests can stage torn tails and partial batches on
//! real files without touching production fault hooks.

use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Injects durability failures at named points while writing real files.
#[derive(Clone, Debug, Default)]
pub(crate) struct FaultSink {
    /// Fail the next data write after this many bytes land.
    pub(crate) write_after_bytes: Option<usize>,
    /// Fail file sync after a successful write.
    pub(crate) sync_error: bool,
    /// Fail truncation of a partial batch.
    pub(crate) truncate_error: bool,
    /// Fail the torn-tail side-file write.
    pub(crate) quarantine_error: bool,
    /// Fail the session-directory sync.
    pub(crate) directory_sync_error: bool,
}

fn injected(op: &str) -> io::Error {
    io::Error::other(format!("injected fault: {op}"))
}

impl FaultSink {
    /// Creates a sink with no failpoints armed.
    #[must_use]
    pub(crate) fn healthy() -> Self {
        Self::default()
    }

    /// Writes `bytes` to `path`, failing after the configured prefix lands.
    ///
    /// # Errors
    /// Returns an injected error once the failpoint prefix is written.
    pub(crate) fn write_partial(&self, path: &Path, bytes: &[u8]) -> io::Result<usize> {
        let limit = self.write_after_bytes.unwrap_or(bytes.len());
        let prefix = bytes.len().min(limit);
        fs::write(path, &bytes[..prefix])?;
        if prefix < bytes.len() {
            return Err(injected("write"));
        }
        Ok(prefix)
    }

    /// Appends `bytes` to `path`, failing after the configured prefix lands.
    pub(crate) fn append_partial(&self, path: &Path, bytes: &[u8]) -> io::Result<usize> {
        let limit = self.write_after_bytes.unwrap_or(bytes.len());
        let prefix = bytes.len().min(limit);
        let mut file = fs::OpenOptions::new().append(true).open(path)?;
        file.write_all(&bytes[..prefix])?;
        if prefix < bytes.len() {
            return Err(injected("write"));
        }
        file.sync_all().map_err(|_| injected("sync"))?;
        Ok(prefix)
    }

    /// Syncs an open file handle.
    ///
    /// # Errors
    /// Returns an injected error when the sync failpoint is armed.
    pub(crate) fn sync_file(&self, file: &File) -> io::Result<()> {
        if self.sync_error {
            return Err(injected("sync"));
        }
        file.sync_all()
    }

    /// Truncates `path` back to `len`.
    ///
    /// # Errors
    /// Returns an injected error when the truncate failpoint is armed.
    pub(crate) fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        if self.truncate_error {
            return Err(injected("truncate"));
        }
        File::options().write(true).open(path)?.set_len(len)
    }

    /// Durably stages torn-tail bytes in a side file next to the journal.
    ///
    /// # Errors
    /// Returns an injected error when the quarantine failpoint is armed.
    pub(crate) fn quarantine(
        &self,
        journal: &Path,
        offset: u64,
        tail: &[u8],
    ) -> io::Result<PathBuf> {
        if self.quarantine_error {
            return Err(injected("quarantine"));
        }
        let side = journal.with_extension(format!("torn-{offset}.jsonl"));
        fs::write(&side, tail)?;
        // Windows denies FlushFileBuffers on a read-only handle.
        File::options().write(true).open(&side)?.sync_all()?;
        Ok(side)
    }

    /// Syncs the directory that holds `path`.
    ///
    /// # Errors
    /// Returns an injected error when the directory-sync failpoint is armed.
    pub(crate) fn sync_dir(&self, path: &Path) -> io::Result<()> {
        if self.directory_sync_error {
            return Err(injected("directory sync"));
        }
        let dir = path.parent().unwrap_or(Path::new("."));
        #[cfg(unix)]
        {
            File::open(dir)?.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let _ = dir;
        }
        Ok(())
    }
}
