//! Cross-process ownership of a session's journal.

use std::{
    fs::{self, File, TryLockError},
    io::{Seek, SeekFrom, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use dal_core::SessionId;

use crate::{
    error::StoreError,
    util::{self, MODE_FILE},
};

const PID_WAIT: Duration = Duration::from_millis(100);
const PID_POLL: Duration = Duration::from_millis(5);

/// Holds the operating-system lock for one session.
///
/// Keep this guard alive for as long as the session journal is open. Dropping
/// it releases the lock; the lock file remains in the session directory.
#[derive(Debug)]
#[must_use = "keep the guard alive while the journal is open"]
pub struct LockGuard {
    _file: File,
}

impl LockGuard {
    /// Opens and exclusively locks `<session_dir>/lock` without waiting.
    ///
    /// # Errors
    /// Returns [`StoreError::Locked`] when another process owns the lock, with
    /// its pid when the lock file contains a parseable current pid. Returns
    /// [`StoreError::Io`] when opening or updating the lock file fails.
    pub fn acquire(session_dir: &Path, session: SessionId) -> Result<Self, StoreError> {
        let path = session_dir.join("lock");
        let mut options = util::open_options();
        options.read(true).write(true).create(true);
        util::with_mode(&mut options, MODE_FILE);
        let mut file = options
            .open(&path)
            .map_err(|source| util::io_err(&path, source))?;

        match file.try_lock() {
            Ok(()) => {
                file.set_len(0)
                    .map_err(|source| util::io_err(&path, source))?;
                file.seek(SeekFrom::Start(0))
                    .map_err(|source| util::io_err(&path, source))?;
                writeln!(file, "{}", std::process::id())
                    .map_err(|source| util::io_err(&path, source))?;
                Ok(Self { _file: file })
            }
            Err(TryLockError::WouldBlock) => {
                let pid = read_pid_until(&path)?;
                Err(StoreError::Locked {
                    session: session.to_string().into_boxed_str(),
                    pid,
                })
            }
            Err(TryLockError::Error(source)) => Err(util::io_err(&path, source)),
        }
    }
}

fn read_pid_until(path: &Path) -> Result<Option<u32>, StoreError> {
    let deadline = Instant::now() + PID_WAIT;
    let mut previous = None;
    loop {
        let bytes = fs::read(path).map_err(|source| util::io_err(path, source))?;
        let current = parse_pid(&bytes);
        if current.is_some() && current == previous {
            return Ok(current);
        }
        previous = current;
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        thread::sleep(PID_POLL.min(deadline - now));
    }
}

fn parse_pid(bytes: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_suffix('\n')?;
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use dal_core::SessionId;

    use super::{LockGuard, parse_pid};
    use crate::error::StoreError;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dal-store-lock-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create lock test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parse_pid_rejects_invalid_utf8() {
        assert_eq!(parse_pid(&[0xff, b'\n']), None);
    }

    #[test]
    fn parse_pid_rejects_empty_text() {
        assert_eq!(parse_pid(b""), None);
    }

    #[test]
    fn parse_pid_rejects_missing_line_ending() {
        assert_eq!(parse_pid(b"123"), None);
    }

    #[test]
    fn parse_pid_rejects_extra_text() {
        assert_eq!(parse_pid(b"123\n456"), None);
    }

    #[test]
    fn concurrent_acquire_reports_owner_pid_without_changing_lock_file() {
        let dir = TestDir::new();
        let id = SessionId::new_v7();
        let _owner = LockGuard::acquire(&dir.0, id).expect("first lock acquisition");
        let path = dir.0.join("lock");
        let before = fs::read(&path).expect("read owner pid");

        let error = match LockGuard::acquire(&dir.0, id) {
            Err(error) => error,
            Ok(_) => panic!("second acquisition must fail"),
        };

        assert!(matches!(
            error,
            StoreError::Locked { session, pid: Some(pid) }
                if session == id.to_string() && pid == std::process::id()
        ));
        assert_eq!(fs::read(path).expect("read unchanged owner pid"), before);
    }

    #[test]
    fn dropping_guard_releases_os_lock_and_leaves_lock_file() {
        let dir = TestDir::new();
        let id = SessionId::new_v7();
        let path = dir.0.join("lock");
        {
            let _guard = LockGuard::acquire(&dir.0, id).expect("first lock acquisition");
        }
        let stale_pid = fs::read(&path).expect("lock file remains");

        let _next = LockGuard::acquire(&dir.0, id).expect("lock released after guard drop");

        assert!(path.exists());
        assert_eq!(
            fs::read(path).expect("new owner pid"),
            format!("{}\n", std::process::id()).as_bytes()
        );
        assert!(!stale_pid.is_empty());
    }
}
