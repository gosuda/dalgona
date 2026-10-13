//! The per-workspace trigram index: its on-disk sections, its builder, its
//! freshness levers, and the indexed find fast path.
//!
//! One directory per canonical workspace lives under the configured index
//! root, named by the first 16 lowercase hex characters of `digest32` of the
//! canonical workspace path. It holds five sections:
//!
//! - `files.bin`: the path table; dense `u32` file ids in path byte order.
//! - `files-extra.bin`: listable paths without postings (directories, binary,
//!   oversize, UTF-16, and unreadable files); it makes an indexed find equal a
//!   full walk.
//! - `postings.bin`: 6-byte postings `(file_id u32, loc_mask u8, next_mask u8)`
//!   grouped by trigram, then the trigram table, then a count footer.
//! - `stamps.bin`: `FileStamp { mtime, size }` per file; unreadable files carry
//!   no stamp.
//! - `meta.bin`: `INDEX_FORMAT_VERSION` as a little-endian `u32`, the build
//!   nonce, and the canonical workspace path bytes.
//!
//! Every section starts with a magic, the format version, and the build nonce.
//! Publication writes each section to a temporary name and renames it, `meta.bin`
//! last; a reader that sees any version, nonce, path, or shape mismatch treats
//! the index as absent. The workspace lint forbids `unsafe`, so readers load a
//! validated section into an immutable shared buffer instead of calling the
//! `unsafe` `memmap2` constructor; a reader keeps its buffer until it drops it,
//! so old readers never observe a later publication.
//!
//! The index is never the truth: candidates are a superset of the true match
//! set over indexable files, and callers verify every candidate against the
//! current bytes.

mod build;
mod query;
mod store;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::watch;

use self::store::{Data, HEX};
use super::find::{self, FindGlob, FindResult};

/// The on-disk format version; any other value reads as an absent index.
pub(crate) const INDEX_FORMAT_VERSION: u32 = 1;
/// The builder arena: 5323840 postings of 12 bytes each.
pub(crate) const ARENA_BYTES: usize = 63_886_080;

const LOCK: &str = "lock";
const LOCK_TRIES: u32 = 3;
const LOCK_BACKOFF: Duration = Duration::from_millis(50);

/// Why the index cannot narrow a query; the caller takes the full-scan fallback.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum IndexError {
    /// Another process holds the builder lock after three tries.
    #[error("index busy in another process")]
    Busy,
    /// `dirty` or `exec_ran` kept landing while the query ran; the caller
    /// takes the fallback, which reads current bytes and is never stale.
    #[error("the index kept changing while the query ran")]
    Churn,
    /// The build or its publication failed.
    #[error("index build failed: {0}")]
    Build(String),
}

impl From<io::Error> for IndexError {
    fn from(error: io::Error) -> Self {
        Self::Build(error.to_string())
    }
}

impl From<crate::search::SearchError> for IndexError {
    fn from(error: crate::search::SearchError) -> Self {
        Self::Build(error.to_string())
    }
}

/// The index hub: per-workspace state, generations, and the exec epoch.
pub(crate) struct Index {
    root: Option<PathBuf>,
    exec_epoch: AtomicU64,
    workspaces: Mutex<HashMap<PathBuf, Arc<WorkspaceIndex>>>,
}

/// One workspace's index state.
///
/// Its phase is `Absent` (no snapshot), `Building(generation)` (a build is in
/// flight), `Ready(generation, epoch)` (the snapshot answers for the current
/// generation and exec epoch), or `Dirty` (a newer generation or epoch exists).
pub(crate) struct WorkspaceIndex {
    canonical: PathBuf,
    /// `None` when no index root is configured; the state stays in memory.
    dir: Option<PathBuf>,
    generation: AtomicU64,
    slot: Mutex<Slot>,
}
impl std::fmt::Debug for WorkspaceIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceIndex")
            .field("canonical", &self.canonical)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Slot {
    ready: Option<Arc<Snapshot>>,
    building: Option<watch::Receiver<Outcome>>,
}

type Outcome = Option<Result<(), IndexError>>;

impl Index {
    /// Create the hub. With `index_root = None` no index exists and every
    /// query reports that the index is unavailable.
    #[must_use]
    pub(crate) fn new(index_root: Option<PathBuf>) -> Arc<Index> {
        Arc::new(Index {
            root: index_root,
            exec_epoch: AtomicU64::new(0),
            workspaces: Mutex::new(HashMap::new()),
        })
    }

    /// Bump the workspace index generation before a write to `path` executes.
    ///
    /// Synchronous: when this returns, no query can serve a snapshot built for
    /// an earlier generation; the next query rebuilds fully. Callers that can
    /// write after the bump also call `dirty` again once the rename lands, so a
    /// build that ran between bump and write never serves past the write.
    pub(crate) fn dirty(&self, workspace: &Path, path: &Path) {
        debug_assert!(!path.as_os_str().is_empty(), "dirty names the written path");
        let canonical = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        self.register(&canonical)
            .generation
            .fetch_add(1, Ordering::SeqCst);
    }

    /// Record that a tool other than read, search, or patch ran: the next query
    /// of every workspace stat-walks its stamps and republishes changed files.
    pub(crate) fn exec_ran(&self) {
        self.exec_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Candidate paths for a grep or symbol query, after the freshness barrier.
    ///
    /// `clauses` is an AND of OR-groups of literal byte strings; a literal
    /// shorter than 3 bytes makes its group unconstrained. With `ignore_case`
    /// the literals are ASCII-lowercased. `scope` is a workspace-relative file
    /// or directory. `Ok(None)` means no index is configured. `Ok(Some)` holds
    /// workspace-relative paths in byte order, a superset of the indexable
    /// files under `scope` that contain a match.
    pub(crate) async fn search_candidates(
        &self,
        workspace: &Path,
        clauses: &[Vec<Vec<u8>>],
        ignore_case: bool,
        scope: Option<&Path>,
    ) -> Result<Option<Vec<PathBuf>>, IndexError> {
        let Some(_) = &self.root else {
            return Ok(None);
        };
        let ws = self.workspace(workspace).await?;
        let clauses: Arc<[Vec<Vec<u8>>]> = clauses.into();
        let scope: Option<Arc<Path>> = scope.map(Into::into);
        for attempt in 0..=SERVE_RETRIES {
            let snapshot = self.current(&ws).await?;
            let token = FreshnessToken {
                ws: Some(ws.clone()),
                generation: snapshot.generation,
                epoch: snapshot.epoch,
            };
            let (clauses, scope) = (clauses.clone(), scope.clone());
            let found = tokio::task::spawn_blocking(move || {
                snapshot
                    .data
                    .candidates(&clauses, ignore_case, scope.as_deref())
            })
            .await
            .map_err(|error| IndexError::Build(error.to_string()))?
            .ok_or_else(|| IndexError::Build("index data failed validation".to_owned()))?;
            if self.is_current_token(&token) {
                return Ok(Some(found));
            }
            if attempt == SERVE_RETRIES {
                return Err(IndexError::Churn);
            }
        }
        unreachable!("the retry loop returns inside every branch")
    }

    /// The indexed find: the path table plus the unindexed sidecar, after the
    /// freshness barrier. `Some` equals `find::find_with(workspace/scope, ..)`;
    /// `None` means no index or a scope the index cannot answer (the caller walks).
    pub(crate) async fn find_entries(
        &self,
        workspace: &Path,
        scope: Option<&Path>,
        glob: &FindGlob,
        limit: usize,
    ) -> Result<Option<FindResult>, IndexError> {
        let Some(_) = &self.root else {
            return Ok(None);
        };
        let ws = self.workspace(workspace).await?;
        let glob = Arc::new(glob.clone());
        let scope: Option<Arc<Path>> = scope.map(Into::into);
        for attempt in 0..=SERVE_RETRIES {
            let snapshot = self.current(&ws).await?;
            let token = FreshnessToken {
                ws: Some(ws.clone()),
                generation: snapshot.generation,
                epoch: snapshot.epoch,
            };
            let (glob, scope) = (glob.clone(), scope.clone());
            let found = tokio::task::spawn_blocking(move || {
                snapshot.data.find(scope.as_deref(), &glob, limit)
            })
            .await
            .map_err(|error| IndexError::Build(error.to_string()))?;
            if self.is_current_token(&token) {
                return Ok(found);
            }
            if attempt == SERVE_RETRIES {
                return Err(IndexError::Churn);
            }
        }
        unreachable!("the retry loop returns inside every branch")
    }

    fn is_current_token(&self, token: &FreshnessToken) -> bool {
        let Some(ws) = &token.ws else {
            return true;
        };
        ws.generation.load(Ordering::SeqCst) == token.generation
            && self.exec_epoch.load(Ordering::SeqCst) == token.epoch
    }

    async fn workspace(&self, workspace: &Path) -> Result<Arc<WorkspaceIndex>, IndexError> {
        let given = workspace.to_path_buf();
        let canonical = tokio::task::spawn_blocking(move || fs::canonicalize(&given))
            .await
            .map_err(|error| IndexError::Build(error.to_string()))??;
        Ok(self.register(&canonical))
    }

    fn register(&self, canonical: &Path) -> Arc<WorkspaceIndex> {
        let dir = self.root.as_ref().map(|root| {
            let key = crate::digest32(canonical.as_os_str().as_encoded_bytes());
            let mut name = String::with_capacity(16);
            for byte in &key[..8] {
                name.push(char::from(HEX[usize::from(byte >> 4)]));
                name.push(char::from(HEX[usize::from(byte & 0x0F)]));
            }
            root.join(name)
        });
        let mut map = lock(&self.workspaces);
        map.entry(canonical.to_path_buf())
            .or_insert_with_key(|key| {
                Arc::new(WorkspaceIndex {
                    canonical: key.clone(),
                    dir,
                    generation: AtomicU64::new(1),
                    slot: Mutex::new(Slot::default()),
                })
            })
            .clone()
    }

    /// Capture the freshness of `workspace` now: its generation and the global
    /// exec epoch. Sync and cheap; it works with no index root, where it still
    /// sees `dirty` and `exec_ran` bumps, so a caller can guard a whole result,
    /// including the full-scan fallback, with `is_current`.
    #[must_use]
    pub(crate) fn freshness(&self, workspace: &Path) -> FreshnessToken {
        let canonical = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        let ws = self.register(&canonical);
        let generation = ws.generation.load(Ordering::SeqCst);
        let epoch = self.exec_epoch.load(Ordering::SeqCst);
        FreshnessToken {
            ws: Some(ws),
            generation,
            epoch,
        }
    }

    /// True when no `dirty` or `exec_ran` landed since `token` was captured.
    /// A stale token means the result in hand may not be served.
    #[must_use]
    pub(crate) fn is_current(&self, workspace: &Path, token: &FreshnessToken) -> bool {
        let canonical = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        let Some(ws) = lock(&self.workspaces).get(&canonical).cloned() else {
            return false;
        };
        token
            .ws
            .as_ref()
            .is_some_and(|token_ws| Arc::ptr_eq(token_ws, &ws))
            && ws.generation.load(Ordering::SeqCst) == token.generation
            && self.exec_epoch.load(Ordering::SeqCst) == token.epoch
    }

    /// The freshness barrier: a snapshot for the current generation and exec
    /// epoch, building, reconciling, or joining the in-flight build as needed.
    async fn current(&self, ws: &Arc<WorkspaceIndex>) -> Result<Arc<Snapshot>, IndexError> {
        loop {
            let mut wait = match self.fresh_or_start(ws) {
                Next::Fresh(snapshot) => return Ok(snapshot),
                Next::Wait(wait) => wait,
            };
            let outcome = wait
                .wait_for(Option::is_some)
                .await
                .map(|seen| (*seen).clone());
            match outcome {
                Ok(Some(Err(error))) => return Err(error),
                Ok(_) => {}
                Err(_) => return Err(IndexError::Build("the index build stopped".to_owned())),
            }
        }
    }

    fn fresh_or_start(&self, ws: &Arc<WorkspaceIndex>) -> Next {
        let generation = ws.generation.load(Ordering::SeqCst);
        let epoch = self.exec_epoch.load(Ordering::SeqCst);
        let mut slot = lock(&ws.slot);
        if let Some(ready) = &slot.ready
            && ready.generation == generation
            && ready.epoch == epoch
        {
            return Next::Fresh(ready.clone());
        }
        if let Some(building) = &slot.building {
            return Next::Wait(building.clone());
        }
        let (sender, receiver) = watch::channel(None);
        slot.building = Some(receiver.clone());
        let job = Job {
            prev: slot.ready.clone(),
            generation,
            epoch,
            flight: InFlight {
                ws: ws.clone(),
                sender,
                done: false,
            },
        };
        drop(slot);
        drop(tokio::task::spawn_blocking(move || job.run()));
        Next::Wait(receiver)
    }
}

/// The freshness of one workspace at capture time: its generation and the
/// global exec epoch. Captured before narrowing, checked after formatting.
#[derive(Clone, Debug)]
pub(crate) struct FreshnessToken {
    ws: Option<Arc<WorkspaceIndex>>,
    generation: u64,
    epoch: u64,
}

/// How often a query recomputes because `dirty` or `exec_ran` kept landing
/// while it ran; past the bound the query fails with `IndexError::Churn` so no
/// result is ever served past a bump.
const SERVE_RETRIES: u32 = 8;

enum Next {
    Fresh(Arc<Snapshot>),
    Wait(watch::Receiver<Outcome>),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a snapshot came to be; tests read it to prove which lever ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildKind {
    /// A full rebuild from the walk.
    Full,
    /// Changed files re-extracted after a stat walk, then republished.
    Incremental,
    /// Opened from disk; the stat walk found no change.
    Opened,
    /// The stat walk found no change since the previous snapshot.
    Unchanged,
}

/// An immutable, validated index snapshot for one generation and exec epoch.
pub(crate) struct Snapshot {
    generation: u64,
    epoch: u64,
    #[cfg(test)]
    kind: BuildKind,
    data: Arc<Data>,
}

/// The in-flight build; dropping it without `finish` reports a stopped build.
struct InFlight {
    ws: Arc<WorkspaceIndex>,
    sender: watch::Sender<Outcome>,
    done: bool,
}

impl InFlight {
    fn finish(mut self, outcome: Result<Snapshot, IndexError>) {
        let mut slot = lock(&self.ws.slot);
        slot.building = None;
        let sent = outcome.map(|snapshot| {
            slot.ready = Some(Arc::new(snapshot));
        });
        drop(slot);
        self.sender.send_replace(Some(sent));
        self.done = true;
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        lock(&self.ws.slot).building = None;
        self.sender.send_replace(Some(Err(IndexError::Build(
            "the index build stopped".to_owned(),
        ))));
    }
}

struct Job {
    prev: Option<Arc<Snapshot>>,
    generation: u64,
    epoch: u64,
    flight: InFlight,
}

impl Job {
    fn run(self) {
        let outcome = self.build();
        self.flight.finish(outcome);
    }

    fn build(&self) -> Result<Snapshot, IndexError> {
        let ws = &self.flight.ws;
        let (kind, data) = match &ws.dir {
            // No index root: the state is the in-memory generation only.
            None => (BuildKind::Full, Arc::new(Data::empty())),
            Some(dir) => {
                fs::create_dir_all(dir)?;
                match &self.prev {
                    Some(prev) if prev.generation == self.generation => {
                        reconcile(ws, dir, &prev.data, BuildKind::Unchanged)?
                    }
                    // A dirty bump demands a full rebuild, even without a
                    // predecessor; only an untouched generation may open disk.
                    Some(_) => (BuildKind::Full, full(ws, dir)?),
                    None if self.generation > 1 => (BuildKind::Full, full(ws, dir)?),
                    None => match store::open(dir, &ws.canonical) {
                        Some(data) => reconcile(ws, dir, &Arc::new(data), BuildKind::Opened)?,
                        None => (BuildKind::Full, full(ws, dir)?),
                    },
                }
            }
        };
        debug_assert!(
            match kind {
                BuildKind::Unchanged => self.prev.is_some(),
                BuildKind::Opened => self.prev.is_none(),
                BuildKind::Full | BuildKind::Incremental => true,
            },
            "an unchanged snapshot reuses a predecessor; an opened one has none"
        );
        Ok(Snapshot {
            generation: self.generation,
            epoch: self.epoch,
            #[cfg(test)]
            kind,
            data,
        })
    }
}

fn full(ws: &WorkspaceIndex, dir: &Path) -> Result<Arc<Data>, IndexError> {
    let _lock = lock_dir(dir)?;
    prune(dir);
    let listing = find::walk_listing(&ws.canonical)?;
    Ok(Arc::new(build::build_dir(
        dir,
        &ws.canonical,
        None,
        listing,
        ARENA_BYTES,
    )?))
}

/// The exec lever: stat-walk against the stamps; republish only on change.
fn reconcile(
    ws: &WorkspaceIndex,
    dir: &Path,
    prev: &Arc<Data>,
    unchanged: BuildKind,
) -> Result<(BuildKind, Arc<Data>), IndexError> {
    let listing = find::walk_listing(&ws.canonical)?;
    if prev.matches(&listing) {
        return Ok((unchanged, prev.clone()));
    }
    let _lock = lock_dir(dir)?;
    prune(dir);
    let data = build::build_dir(dir, &ws.canonical, Some(prev), listing, ARENA_BYTES)?;
    Ok((BuildKind::Incremental, Arc::new(data)))
}

/// Take the builder lock with three tries; the lock file handle holds it.
fn lock_dir(dir: &Path) -> Result<File, IndexError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    for attempt in 1..=LOCK_TRIES {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if attempt < LOCK_TRIES => {
                std::thread::sleep(LOCK_BACKOFF);
            }
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    Err(IndexError::Busy)
}

/// Remove spill and temporary files of dead builders. Called with the builder
/// lock held: a live builder always holds the lock, so any such file belongs
/// to a builder that is gone.
fn prune(dir: &Path) {
    let Ok(items) = fs::read_dir(dir) else { return };
    for item in items.flatten() {
        let name = item.file_name();
        let name = name.to_string_lossy();
        let spill = name.starts_with("build-") && name.ends_with(".spill");
        if spill || name.contains(".tmp-") {
            // A file another pruner already removed is not an error.
            drop(fs::remove_file(item.path()));
        }
    }
}

#[cfg(test)]
impl Index {
    /// The phase of one workspace: `(generation, kind)` of its ready snapshot
    /// when it answers for the current generation and epoch.
    fn ready(&self, workspace: &Path) -> Option<(u64, BuildKind)> {
        let canonical = fs::canonicalize(workspace).ok()?;
        let ws = lock(&self.workspaces).get(&canonical)?.clone();
        let generation = ws.generation.load(Ordering::SeqCst);
        let epoch = self.exec_epoch.load(Ordering::SeqCst);
        let slot = lock(&ws.slot);
        let ready = slot.ready.as_ref()?;
        (ready.generation == generation && ready.epoch == epoch)
            .then_some((ready.generation, ready.kind))
    }

    fn dir_of(&self, workspace: &Path) -> PathBuf {
        let canonical = fs::canonicalize(workspace).unwrap();
        lock(&self.workspaces)[&canonical]
            .dir
            .clone()
            .expect("the test uses a rooted index")
    }
}

#[cfg(test)]
mod tests;
