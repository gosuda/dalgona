//! Cross-process ownership of a session's journal.

use std::{
    collections::HashMap,
    fs::{self, File, TryLockError},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
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

/// One process-held lock path: every guard taken on it plus whether a
/// live journal owns them.
///
/// A fresh acquire always lands `live: false` (transient — the opening
/// scan/repair, a detached first-append setup, or a mid-release drop, all
/// resolving in bounded time and worth a bounded retry); a journal parks
/// the guard into its state via [`LockGuard::mark_live`] and only then
/// does `live` go up, telling a same-pid `Locked` contender that waiting
/// never pays because a session holds the lock for its whole life.
#[derive(Debug, Default)]
struct Holder {
    count: usize,
    live: bool,
}

fn holders() -> &'static Mutex<HashMap<PathBuf, Holder>> {
    static HOLDERS: OnceLock<Mutex<HashMap<PathBuf, Holder>>> = OnceLock::new();
    HOLDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn holders_map() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Holder>> {
    holders()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn register(path: &Path) {
    holders_map().entry(path.to_path_buf()).or_default().count += 1;
}

fn unregister(path: &Path) {
    let mut map = holders_map();
    if let Some(holder) = map.get_mut(path) {
        holder.count -= 1;
        if holder.count == 0 {
            map.remove(path);
        }
    }
}

/// `true` while a live journal inside this process owns the OS lock for
/// `path` — as opposed to a transient guard mid-acquire or mid-release,
/// which registers too but is worth retrying through. The query path is
/// canonicalized the same way the holder's registry key was, so two
/// stores that reach the lock through symlinked and real spellings of
/// the data root still see the same holder. Production callers use
/// [`live_key`] with a key canonicalized off the async executor.
#[cfg(test)]
pub(crate) fn live_in_process(path: &Path) -> bool {
    live_key(&util::canonical_path(path))
}

/// The same query under a key the caller already canonicalized — a pure
/// map access for callers where filesystem work must not run, e.g. the
/// async executor between bounded retries.
pub(crate) fn live_key(key: &Path) -> bool {
    holders_map().get(key).is_some_and(|holder| holder.live)
}

/// Holds the operating-system lock for one session.
///
/// Keep this guard alive for as long as the session journal is open. Dropping
/// it removes the owner sidecar, then releases the lock; the lock file itself
/// remains in the session directory.
#[derive(Debug)]
#[must_use = "keep the guard alive while the journal is open"]
pub(crate) struct LockGuard {
    _file: File,
    path: PathBuf,
    /// The canonical spelling of `path` under which this guard is
    /// registered — two stores that alias the same data root through a
    /// symlink land on one key, and a drop can recompute nothing.
    key: PathBuf,
}

impl LockGuard {
    /// Opens and exclusively locks `path`, retrying a contended lock briefly before giving up.
    ///
    /// # Errors
    /// Returns [`StoreError::Locked`] when another process owns the lock, with
    /// its pid when the lock file contains a parseable current pid. Returns
    /// [`StoreError::Io`] when opening or updating the lock file fails, or when
    /// another process keeps replacing the lock file for the whole wait bound.
    pub(crate) fn acquire(path: &Path, session: SessionId) -> Result<Self, StoreError> {
        Self::acquire_with(path, session, |_| {})
    }

    /// [`Self::acquire`] with `after_open` called after each open and before
    /// the lock attempt, so a test can replace the lock file in that window.
    fn acquire_with(
        path: &Path,
        session: SessionId,
        mut after_open: impl FnMut(&Path),
    ) -> Result<Self, StoreError> {
        let deadline = Instant::now() + PID_WAIT;
        let mut file = open_lock_file(path)?;
        after_open(path);
        loop {
            match file.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock)
                    if Instant::now() < deadline && !live_key(&util::canonical_path(path)) =>
                {
                    thread::sleep(poll_delay(session));
                    continue;
                }
                Err(TryLockError::WouldBlock) => {
                    let pid = read_pid_until(&owner_path(path))?;
                    return Err(StoreError::Locked {
                        session,
                        pid,
                        path: path.to_path_buf(),
                    });
                }
                Err(TryLockError::Error(source)) => {
                    return Err(util::io_err(path, source));
                }
            }
            // A lock on a file that no longer sits at `path` excludes nobody:
            // the next opener creates and locks a fresh file. Reopen and lock
            // again, within the same wait bound.
            if names_locked_file(&file, path).map_err(|source| util::io_err(path, source))? {
                return Self::publish_owner(file, path);
            }
            if Instant::now() >= deadline {
                return Err(util::io_err(
                    path,
                    io::Error::other(
                        "the lock file was replaced while it was being locked; \
                         open the session again",
                    ),
                ));
            }
            file = open_lock_file(path)?;
            after_open(path);
        }
    }

    /// Writes this process's pid into the freshly locked `file` and its sidecar.
    fn publish_owner(mut file: File, path: &Path) -> Result<Self, StoreError> {
        file.set_len(0)
            .map_err(|source| util::io_err(path, source))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| util::io_err(path, source))?;
        writeln!(file, "{}", std::process::id()).map_err(|source| util::io_err(path, source))?;
        // The owner sidecar repeats the pid outside the locked file:
        // `LockFileEx` makes the held file unreadable to every other
        // handle on Windows, so a contended acquirer reads the pid
        // from the sidecar that the lock never seals.
        fs::write(
            owner_path(path),
            format!("{}\n", std::process::id()).as_bytes(),
        )
        .map_err(|source| util::io_err(path, source))?;
        // Registered last, as transient: the sidecar precedes it so a
        // contender reading our pid always finds the holder entry instead of
        // taking a needless retry pass; a journal flips it to live only once
        // it parks the guard.
        let key = util::canonical_path(path);
        register(&key);
        Ok(Self {
            _file: file,
            path: path.to_path_buf(),
            key,
        })
    }

    /// Marks this held lock as owned by a live journal. Call when the
    /// guard is parked into a journal's state; a same-process `Locked`
    /// contender then reports immediately instead of burning its retry
    /// budget on a lock that outlives it.
    pub(crate) fn mark_live(&self) {
        if let Some(holder) = holders_map().get_mut(&self.key) {
            holder.live = true;
        }
    }

    /// Marks this held lock back as transient. Call when a journal begins
    /// retiring the guard (close, or dropping without close); a contender
    /// reopening the same session then keeps its bounded retry through
    /// the release instead of failing on a `live` flag the guard only
    /// carries for the last instructions it owns.
    pub(crate) fn mark_detached(&self) {
        if let Some(holder) = holders_map().get_mut(&self.key) {
            holder.live = false;
        }
    }

    /// The registry key this guard was acquired under.
    pub(crate) fn registry_key(&self) -> &Path {
        &self.key
    }
}

fn open_lock_file(path: &Path) -> Result<File, StoreError> {
    let mut options = util::open_options();
    options.read(true).write(true).create(true);
    util::with_mode(&mut options, MODE_FILE);
    options
        .open(path)
        .map_err(|source| util::io_err(path, source))
}

/// Whether `path` still names the file `file` holds open.
///
/// A missing path counts as replaced. The inode check is Unix-only; Windows
/// keeps the replacement gap.
#[cfg(unix)]
fn names_locked_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let held = file.metadata()?;
    let named = match fs::metadata(path) {
        Ok(named) => named,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(source),
    };
    Ok(held.dev() == named.dev() && held.ino() == named.ino())
}

#[cfg(not(unix))]
#[expect(clippy::unnecessary_wraps, reason = "matches the Unix signature")]
fn names_locked_file(_file: &File, _path: &Path) -> io::Result<bool> {
    Ok(true)
}

/// Re-marks registry keys live when dropped while armed — a scopeguard
/// for the window where a journal has detached its guards for retirement
/// but an `await` can still cancel or fail the close: no code resumes on
/// a cancelled future, so the restore has to ride `Drop`. Disarm once
/// retirement succeeded so the transient flag carries into the guard
/// drop itself.
pub(crate) struct ReliveOnDrop(Vec<PathBuf>);

impl ReliveOnDrop {
    /// Arms a restore for each registry key.
    pub(crate) fn arm(keys: impl IntoIterator<Item = PathBuf>) -> Self {
        Self(keys.into_iter().collect())
    }

    /// Retirement finished: the detach is final, nothing restores.
    pub(crate) fn disarm(mut self) {
        self.0.clear();
    }
}

impl Drop for ReliveOnDrop {
    fn drop(&mut self) {
        for key in &self.0 {
            if let Some(holder) = holders_map().get_mut(key) {
                holder.live = true;
            }
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Unregister and remove the sidecar before `file` closes and the
        // lock releases: a contender that loses `try_lock` to the next
        // holder must not read a retired pid or a stale live-holder flag
        // during the gap before that holder republishes its own.
        unregister(&self.key);
        let _ = fs::remove_file(owner_path(&self.path));
    }
}

/// The unlocked sibling that carries the holder's pid text, `<lock>.owner`.
fn owner_path(path: &Path) -> std::path::PathBuf {
    let mut text = path.as_os_str().to_os_string();
    text.push(".owner");
    text.into()
}

fn poll_delay(session: SessionId) -> Duration {
    use std::hash::BuildHasher;
    let hash = std::collections::hash_map::RandomState::new().hash_one(session);
    let jitter = Duration::from_millis(hash % u64::from(PID_POLL.subsec_millis()));
    PID_POLL.saturating_add(jitter)
}

fn read_pid_until(path: &Path) -> Result<Option<u32>, StoreError> {
    let deadline = Instant::now() + PID_WAIT;
    loop {
        let current = match fs::read(path) {
            Ok(bytes) => parse_pid(&bytes),
            // A contender can read between the holder's lock acquire and its
            // owner-sidecar write; a missing sidecar is a transient state like
            // unparsable content, so keep polling until the deadline.
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => return Err(util::io_err(path, source)),
        };
        // A parseable `pid\n` line is complete — the single-syscall write
        // cannot tear it — so the first complete read settles the poll. Only
        // missing or unparsable content keeps polling until the deadline.
        if current.is_some() {
            return Ok(current);
        }
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

    use super::{LockGuard, parse_pid, read_pid_until};
    use crate::error::StoreError;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    pub(super) struct TestDir(pub(super) PathBuf);

    impl TestDir {
        pub(super) fn new() -> Self {
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
        let Ok(_owner) = LockGuard::acquire(&dir.0.join("lock"), id) else {
            panic!("first lock acquisition must succeed")
        };
        let path = dir.0.join("lock");

        let Err(error) = LockGuard::acquire(&path, id) else {
            panic!("second acquisition must fail")
        };

        assert!(matches!(
            &error,
            StoreError::Locked { session, pid: Some(pid), .. }
                if *session == id && *pid == std::process::id()
        ));
        // The lock file itself stays sealed to other handles while held on
        // Windows; the owner sidecar carries the readable pid everywhere.
        #[cfg(unix)]
        {
            let before = fs::read(&path).expect("read owner pid");
            assert_ne!(before, [] as [u8; 0]);
            assert_eq!(fs::read(&path).expect("read unchanged owner pid"), before);
        }
    }

    #[test]
    fn contended_lock_without_pid_reports_unknown_process() {
        let dir = TestDir::new();
        let id = SessionId::new_v7();
        let path = dir.0.join("lock");
        let owner = super::owner_path(&path);
        let _owner = LockGuard::acquire(&path, id).expect("first lock acquisition");
        fs::write(&owner, b"").expect("erase lock owner text");
        let before = fs::read(&owner).expect("read empty owner text");

        let Err(error) = LockGuard::acquire(&path, id) else {
            panic!("second acquisition must fail")
        };

        assert_eq!(
            error.to_string(),
            format!(
                "session {id} is open in another process (lock {})",
                path.display()
            )
        );
        assert!(matches!(
            error,
            StoreError::Locked { session, pid: None, .. } if session == id
        ));
        assert_eq!(fs::read(owner).expect("read unchanged owner text"), before);
    }

    #[test]
    fn dropping_guard_releases_os_lock_and_leaves_lock_file() {
        let dir = TestDir::new();
        let id = SessionId::new_v7();
        let path = dir.0.join("lock");
        {
            let _guard =
                LockGuard::acquire(&dir.0.join("lock"), id).expect("first lock acquisition");
        }
        let stale_pid = fs::read(&path).expect("lock file remains");

        {
            let _next = LockGuard::acquire(&dir.0.join("lock"), id)
                .expect("lock released after guard drop");

            assert!(path.exists());
            // The held lock file is unreadable on Windows; POSIX advisory
            // locks still allow reading the new owner while it is held.
            #[cfg(unix)]
            assert_eq!(
                fs::read(path).expect("new owner pid"),
                format!("{}\n", std::process::id()).as_bytes()
            );
        }
        #[cfg(windows)]
        assert_eq!(
            fs::read(&path).expect("new owner pid"),
            format!("{}\n", std::process::id()).as_bytes()
        );
        assert_ne!(stale_pid, [] as [u8; 0]);
    }

    #[test]
    fn dropped_guard_removes_owner_sidecar() {
        let dir = TestDir::new();
        let id = SessionId::new_v7();
        let path = dir.0.join("lock");
        let owner = super::owner_path(&path);
        {
            let _guard = LockGuard::acquire(&path, id).expect("first lock acquisition");
            assert!(owner.exists(), "held lock publishes its owner sidecar");
            // A stale pid left by an older holder must not survive the drop.
            fs::write(&owner, b"999999\n").expect("write stale owner text");
        }
        assert!(
            !owner.exists(),
            "dropped guard removes its owner sidecar, stale pid included"
        );
        let _next = LockGuard::acquire(&path, id).expect("lock released after guard drop");
        assert!(owner.exists(), "the next holder republishes its own pid");
    }

    #[test]
    fn live_in_process_follows_journal_parking() {
        let dir = TestDir::new();
        let id = SessionId::new_v7();
        let path = dir.0.join("lock");
        // The same lock file spelled through a different path — a store
        // whose data root reaches the file another way must still see the
        // holder.
        let alias = dir.0.join("sub").join("..").join("lock");
        fs::create_dir(dir.0.join("sub")).expect("create alias prefix");
        assert!(!super::live_in_process(&path));
        {
            let guard = LockGuard::acquire(&path, id).expect("first lock acquisition");
            // A fresh guard is transient: a detached first-append setup or
            // an in-flight open holds it without a live journal behind it.
            assert!(!super::live_in_process(&path));
            guard.mark_live();
            assert!(super::live_in_process(&path));
            assert!(super::live_in_process(&alias));
            guard.mark_detached();
            assert!(!super::live_in_process(&path));
            guard.mark_live();
            assert!(super::live_in_process(&path));
        }
        assert!(!super::live_in_process(&path));
        let next = LockGuard::acquire(&path, id).expect("lock released after guard drop");
        assert!(!super::live_in_process(&path));
        next.mark_live();
        assert!(super::live_in_process(&path));
    }

    #[test]
    fn read_pid_until_gives_up_as_none_on_a_missing_sidecar() {
        let dir = TestDir::new();
        let path = dir.0.join("session.lock.owner");
        assert_eq!(
            read_pid_until(&path).expect("a missing sidecar is transient, not an error"),
            None,
            "the poll must end at the deadline, not spin forever"
        );
    }

    #[test]
    fn read_pid_until_reads_a_present_sidecar() {
        let dir = TestDir::new();
        let path = dir.0.join("session.lock.owner");
        fs::write(&path, b"4242\n").expect("write the owner sidecar");
        assert_eq!(
            read_pid_until(&path).expect("poll reads the sidecar"),
            Some(4242),
            "the poll must return the pid as soon as it is readable"
        );
    }

    #[test]
    fn read_pid_until_reports_errors_that_are_not_transient() {
        let dir = TestDir::new();
        // A directory is never a parseable pid file and never becomes one;
        // mistaking it for a missing sidecar would poll instead of failing.
        assert!(
            read_pid_until(&dir.0).is_err(),
            "non-NotFound read errors must surface, not be swallowed by the poll"
        );
    }

    #[test]
    fn relive_on_drop_restores_detached_holders_until_disarmed() {
        let dir = TestDir::new();
        let id = SessionId::new_v7();
        let path = dir.0.join("lock");
        let guard = LockGuard::acquire(&path, id).expect("lock acquisition");
        guard.mark_live();
        guard.mark_detached();
        assert!(!super::live_in_process(&path));
        // A cancelled or failed close drops its restore armed: the journal
        // still owns the lock, so the live flag comes back.
        {
            let _restore = super::ReliveOnDrop::arm([guard.registry_key().to_path_buf()]);
        }
        assert!(super::live_in_process(&path));
        // A cleanly retired close disarms instead: the transient flag
        // survives through the guard drop.
        guard.mark_detached();
        let restore = super::ReliveOnDrop::arm([guard.registry_key().to_path_buf()]);
        restore.disarm();
        assert!(!super::live_in_process(&path));
        drop(guard);
        assert!(!super::live_in_process(&path));
    }
}

#[cfg(all(test, unix))]
mod replaced_file_tests {
    use std::{cell::Cell, fs, path::Path};

    use dal_core::SessionId;

    use super::{LockGuard, PID_WAIT, tests::TestDir};
    use crate::error::StoreError;

    fn replace(path: &Path) -> std::io::Result<()> {
        fs::remove_file(path)?;
        fs::write(path, b"")
    }

    #[test]
    fn lock_file_replaced_before_locking_still_excludes_a_second_opener() {
        let dir = TestDir::new();
        let path = dir.0.join("lock");
        let id = SessionId::new_v7();
        let replaced = Cell::new(false);

        let guard = LockGuard::acquire_with(&path, id, |at| {
            if !replaced.replace(true) {
                replace(at).expect("replace the lock file");
            }
        })
        .expect("acquire after the file was replaced");

        let second = LockGuard::acquire(&path, id);
        assert!(
            matches!(second, Err(StoreError::Locked { session, .. }) if session == id),
            "the held lock must be on the file at the path, got {second:?}"
        );
        drop(guard);
    }

    #[test]
    fn lock_file_replaced_after_deadline_is_not_retried() {
        let dir = TestDir::new();
        let path = dir.0.join("lock");
        let attempts = Cell::new(0);

        let result = LockGuard::acquire_with(&path, SessionId::new_v7(), |at| {
            attempts.set(attempts.get() + 1);
            assert_eq!(attempts.get(), 1, "an expired acquisition must not reopen");
            std::thread::sleep(PID_WAIT);
            replace(at).expect("replace the lock file");
        });

        assert!(
            matches!(result, Err(StoreError::Io { .. })),
            "replacement after the deadline must return an I/O error, got {result:?}"
        );
    }
}
