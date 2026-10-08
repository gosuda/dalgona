//! The journal file: one write and one sync per batch, torn-tail repair,
//! structure validation, and rollback on failure.
//!
//! The journal speaks bytes; its caller encodes records and publishes blobs
//! before the first byte of a batch is written (D-09).

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use dal_core::{
    DecodeError, EntryId, Gen, Header, JobEvent, JobId, Record, TurnEndStop, TurnId, Usage, decode,
};

use crate::error::{AbortedTurn, JournalError, OpenReport, TornTail};

/// The largest one record may be, in bytes (D-08).
pub(crate) const MAX_RECORD: u64 = 67_108_864;
const SCAN_BUFFER: usize = 1_048_576;
/// The backward read window for torn-tail repair (D-14).
const READ_WINDOW: u64 = 65_536;

/// A durability fault, injected by tests at a named point (D-41).
///
/// Production code passes [`Faults::default`]; every field `None` or `false`.
#[derive(Clone, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "these independent switches are the fixed durability fault-injection configuration"
)]
pub(crate) struct Faults {
    /// Fail the batch write after this many bytes land.
    pub(crate) write_after_bytes: Option<usize>,
    /// Fail the file sync after a successful write.
    pub(crate) sync_error: bool,
    /// Fail the truncate-back after a failed batch.
    pub(crate) truncate_error: bool,
    /// Fail the torn-tail side-file write at open.
    pub(crate) quarantine_error: bool,
    /// Fail the session-directory sync after writing the torn-tail sidefile.
    /// Windows has no directory-sync door, so the fault is never injected there.
    #[cfg_attr(windows, expect(dead_code, reason = "directory sync is POSIX-only"))]
    pub(crate) directory_sync_error: bool,
}

/// Proof that one batch is durable. Nothing publishes before this exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// The byte offset where the batch started.
    pub offset: u64,
    /// The batch length in bytes.
    pub len: u64,
}

/// The state of an opened journal: decoded records plus recovery facts.
#[derive(Debug)]
pub(crate) struct Opened {
    /// The session header; the file's first record.
    pub(crate) header: Header,
    /// Every record with its byte offset, in file order.
    pub(crate) records: Vec<(u64, Record)>,
    /// The generation this open will assign to its boot record.
    pub(crate) r#gen: Gen,
    /// Repairs to append in one batch with the boot record (D-15).
    pub(crate) repair: Vec<Record>,
    /// Facts for the agent's notices (D-39).
    pub(crate) report: OpenReport,
    /// The same writable file handle validated by this open.
    pub(crate) journal: Journal,
    /// Structural state for the durable prefix, before planned recovery.
    pub(crate) validator: Validator,
    /// Structural changes to commit after the repair append receipt.
    pub(crate) repair_delta: ValidationDelta,
}

#[derive(Debug)]
enum Health {
    Healthy,
    Damaged,
}

/// One open journal file. Neither `Clone` nor `Sync`; one writer per file.
#[derive(Debug)]
pub(crate) struct Journal {
    file: File,
    path: PathBuf,
    end: u64,
    health: Health,
    faults: Faults,
}

impl Journal {
    /// Creates a new journal: one write, one file sync, and one directory sync.
    ///
    /// # Errors
    /// Returns [`JournalError::Io`] when creation or syncing fails, and
    /// [`JournalError::Damaged`] when a failed create cannot be removed durably.
    pub(crate) fn create(path: &Path, lines: &[u8], faults: &Faults) -> Result<Self, JournalError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .map_err(|source| jio("create", path, source))?;
        let mut journal = Self {
            file,
            path: path.to_path_buf(),
            end: 0,
            health: Health::Healthy,
            faults: faults.clone(),
        };
        let result = journal
            .write_bytes(lines)
            .and_then(|()| journal.sync())
            .and_then(|()| sync_parent_directory(path, "sync"));
        if let Err(error) = result {
            drop(journal);
            return Err(cleanup_created_file(path, error));
        }
        journal.end = u64::try_from(lines.len()).unwrap_or(u64::MAX);
        Ok(journal)
    }

    /// Opens a journal: validate its complete prefix, repair a torn tail, and plan recovery.
    ///
    /// # Errors
    /// Returns [`JournalError`] for a failed read or repair step, a failed
    /// structure check (as [`crate::error::StoreError::Damaged`] via the caller),
    /// or an unsupported version (as [`crate::error::StoreError::UnknownVersion`]).
    pub(crate) fn open(path: &Path, faults: &Faults) -> Result<Opened, OpenFailure> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| OpenFailure::Journal(jio("open", path, source)))?;
        let mut journal = Self {
            file,
            path: path.to_path_buf(),
            end: 0,
            health: Health::Healthy,
            faults: faults.clone(),
        };
        let file_len = journal
            .file
            .metadata()
            .map_err(|source| OpenFailure::Journal(jio("read", path, source)))?
            .len();
        let mut stream_buffer = vec![0_u8; SCAN_BUFFER];
        let mut records = Vec::new();
        let (complete_end, validator) = scan_complete_prefix(
            &mut journal.file,
            path,
            file_len,
            &mut stream_buffer,
            |offset, record| records.push((offset, record)),
        )?;
        let header = header_of(&records)?;
        let next_gen = validator
            .last_boot_gen
            .checked_add(1)
            .and_then(core::num::NonZeroU64::new)
            .ok_or_else(|| OpenFailure::Damaged {
                offset: 0,
                reason: "boot generation is exhausted".into(),
            })?;
        let r#gen = Gen::new(next_gen);
        let (repair, aborted) =
            recovery_batch(&records, r#gen, validator.next_entry_id(), validator.leaf())?;
        let repair_delta = validator.prepare_batch(&repair)?;
        let torn = repair_torn_tail(
            &mut journal,
            path,
            faults,
            complete_end,
            file_len,
            &mut stream_buffer,
        )
        .map_err(OpenFailure::Journal)?;
        journal.end = complete_end;
        Ok(Opened {
            header,
            records,
            r#gen,
            repair,
            report: OpenReport {
                r#gen,
                torn,
                aborted,
            },
            journal,
            validator,
            repair_delta,
        })
    }

    /// Appends one batch: one write at the tracked end offset, one sync.
    ///
    /// On failure the file truncates back to the old end and syncs again;
    /// if that repair fails the journal is `Damaged` until reopen (D-13).
    ///
    /// # Errors
    /// Returns [`JournalError::Io`] for the write or sync failure, and
    /// [`JournalError::Damaged`] when rollback failed.
    /// The whole write-and-sync round trip, kept for tests that drive the
    /// journal directly; the shard worker stages the two halves instead.
    #[cfg(test)]
    pub(crate) fn append(&mut self, lines: &[u8]) -> Result<Receipt, JournalError> {
        let receipt = self.append_unsynced(lines)?;
        self.sync().map(|()| receipt)
    }

    /// Writes the batch without syncing it. Pair the returned receipt with
    /// [`Self::stage_sync`] so the durability sync can run off the caller's
    /// thread; the receipt must not surface before that sync lands.
    ///
    /// # Errors
    /// Same as [`Self::append`] minus the sync failure (rollback identical).
    pub(crate) fn append_unsynced(&mut self, lines: &[u8]) -> Result<Receipt, JournalError> {
        if let Health::Damaged = self.health {
            return Err(JournalError::Damaged {
                path: self.path.clone(),
                cause: "journal is damaged".into(),
            });
        }
        let offset = self.end;
        match self.write_bytes(lines) {
            Ok(()) => {
                self.end = offset.saturating_add(lines.len() as u64);
                Ok(Receipt {
                    offset,
                    len: lines.len() as u64,
                })
            }
            Err(error) => {
                if self.faults.truncate_error || self.truncate_back(offset).is_err() {
                    self.health = Health::Damaged;
                    return Err(JournalError::Damaged {
                        path: self.path.clone(),
                        cause: format!("{error}; truncate-back also failed").into(),
                    });
                }
                Err(error)
            }
        }
    }

    /// Prepares an off-thread sync for a receipt from
    /// [`Self::append_unsynced`]: the cloned handle is a dup fd to this
    /// journal, so its `sync_all` flushes the same pages and the caller can
    /// run it on another thread. The injected sync fault is evaluated here,
    /// before the hand-off, so test paths keep their deterministic error.
    ///
    /// # Errors
    /// Returns [`JournalError::Io`] when the injected fault fires or the
    /// handle cannot be duplicated.
    pub(crate) fn stage_sync(
        &self,
        receipt: Receipt,
    ) -> Result<(File, PathBuf, Receipt), JournalError> {
        if self.faults.sync_error {
            return Err(jio(
                "sync",
                &self.path,
                io::Error::other("injected sync failure"),
            ));
        }
        self.file
            .try_clone()
            .map(|file| (file, self.path.clone(), receipt))
            .map_err(|source| jio("sync", &self.path, source))
    }

    /// Rolls the file and the tracked end offset back to `receipt.offset`:
    /// the same repair [`Self::append`] runs on a sync failure, applied when
    /// a staged sync fails before its receipt can surface.
    ///
    /// # Errors
    /// Returns [`JournalError::Damaged`] when truncate-back itself fails.
    pub(crate) fn roll_back(&mut self, receipt: Receipt) -> Result<(), JournalError> {
        if self.faults.truncate_error || self.truncate_back(receipt.offset).is_err() {
            self.health = Health::Damaged;
            return Err(JournalError::Damaged {
                path: self.path.clone(),
                cause: "truncate-back after staged-sync failure also failed".into(),
            });
        }
        self.end = receipt.offset;
        Ok(())
    }

    /// Marks the journal damaged: an off-thread durability sync failed and
    /// the bytes it covered can no longer be rolled back in place, so the
    /// journal needs reopen repair instead of more appends.
    pub(crate) fn mark_damaged(&mut self) {
        self.health = Health::Damaged;
    }

    fn sync(&mut self) -> Result<(), JournalError> {
        if self.faults.sync_error {
            return Err(jio(
                "sync",
                &self.path,
                io::Error::other("injected sync failure"),
            ));
        }
        self.file
            .sync_all()
            .map_err(|source| jio("sync", &self.path, source))
    }

    /// The tracked end offset: the next batch starts here.
    #[must_use]
    pub(crate) fn end(&self) -> u64 {
        self.end
    }

    /// Replaces injected durability faults in tests.
    #[cfg(test)]
    pub(crate) fn set_faults(&mut self, faults: Faults) {
        self.faults = faults;
    }

    fn truncate_back(&mut self, offset: u64) -> Result<(), JournalError> {
        self.file
            .set_len(offset)
            .map_err(|source| jio("truncate", &self.path, source))?;
        self.file
            .sync_all()
            .map_err(|source| jio("sync", &self.path, source))
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), JournalError> {
        self.file
            .seek(SeekFrom::Start(self.end))
            .map_err(|source| jio("write", &self.path, source))?;
        match self.faults.write_after_bytes {
            Some(limit) => {
                let cut = limit.min(bytes.len());
                self.file
                    .write_all(&bytes[..cut])
                    .map_err(|source| jio("write", &self.path, source))?;
                if cut < bytes.len() {
                    return Err(jio(
                        "write",
                        &self.path,
                        io::Error::other("injected partial write"),
                    ));
                }
            }
            None => self
                .file
                .write_all(bytes)
                .map_err(|source| jio("write", &self.path, source))?,
        }
        Ok(())
    }
}

/// Why an open failed: a hard version refusal or a journal error.
#[derive(Debug)]
pub(crate) enum OpenFailure {
    /// The file names a format this build does not read. Nothing was written.
    UnknownVersion(u64),
    /// A repair, read, or validation failure.
    Journal(JournalError),
    /// Structure validation failed; the reason is the spec's decode reason.
    Damaged {
        /// The byte offset of the offending record.
        offset: u64,
        /// The reason text.
        reason: Box<str>,
    },
}

impl From<JournalError> for OpenFailure {
    fn from(error: JournalError) -> Self {
        OpenFailure::Journal(error)
    }
}

fn jio(op: &'static str, path: &Path, source: io::Error) -> JournalError {
    JournalError::Io {
        op,
        path: path.to_path_buf(),
        source: Box::new(source),
    }
}

fn header_of(records: &[(u64, Record)]) -> Result<Header, OpenFailure> {
    match records.first() {
        Some((_, Record::Session(header))) => Ok(header.clone()),
        _ => Err(OpenFailure::Damaged {
            offset: 0,
            reason: "the first record is not a session header".into(),
        }),
    }
}

/// Moves a validated torn tail to `torn-<offset>.jsonl`, then truncates and syncs.
fn repair_torn_tail(
    journal: &mut Journal,
    path: &Path,
    faults: &Faults,
    tail_start: u64,
    file_len: u64,
    buffer: &mut [u8],
) -> Result<Option<TornTail>, JournalError> {
    if tail_start == file_len {
        return Ok(None);
    }
    if faults.quarantine_error {
        return Err(jio(
            "quarantine",
            path,
            io::Error::other("injected quarantine failure"),
        ));
    }
    let tail_bytes = file_len - tail_start;
    let kept_at = path.with_file_name(format!("torn-{tail_start}.jsonl"));
    write_side_file(
        &mut journal.file,
        &kept_at,
        tail_start,
        tail_bytes,
        buffer,
        path,
        faults,
    )?;
    journal
        .file
        .set_len(tail_start)
        .map_err(|source| jio("truncate", path, source))?;
    journal
        .file
        .sync_all()
        .map_err(|source| jio("sync", path, source))?;
    Ok(Some(TornTail {
        offset: tail_start,
        bytes: tail_bytes,
        kept_at,
    }))
}

fn write_side_file(
    journal_file: &mut File,
    kept_at: &Path,
    tail_start: u64,
    tail_bytes: u64,
    buffer: &mut [u8],
    journal_path: &Path,
    faults: &Faults,
) -> Result<(), JournalError> {
    #[cfg(windows)]
    let _ = faults;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut kept_file = options
        .open(kept_at)
        .map_err(|source| jio("quarantine", journal_path, source))?;
    journal_file
        .seek(SeekFrom::Start(tail_start))
        .map_err(|source| jio("read", journal_path, source))?;
    let buffer_bytes = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
    let mut remaining = tail_bytes;
    while remaining > 0 {
        let read_len = usize::try_from(remaining.min(buffer_bytes)).unwrap_or(buffer.len());
        let count = journal_file
            .read(&mut buffer[..read_len])
            .map_err(|source| jio("read", journal_path, source))?;
        if count == 0 {
            return Err(jio(
                "read",
                journal_path,
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "journal changed during quarantine",
                ),
            ));
        }
        kept_file
            .write_all(&buffer[..count])
            .map_err(|source| jio("quarantine", journal_path, source))?;
        remaining = remaining.saturating_sub(u64::try_from(count).unwrap_or(u64::MAX));
    }
    kept_file
        .sync_all()
        .map_err(|source| jio("quarantine", journal_path, source))?;
    #[cfg(not(windows))]
    if faults.directory_sync_error {
        return Err(jio(
            "quarantine",
            journal_path,
            io::Error::other("injected directory sync failure"),
        ));
    }
    sync_parent_directory(kept_at, "quarantine").map_err(|error| match error {
        JournalError::Io { source, .. } => jio("quarantine", journal_path, *source),
        other => other,
    })?;
    Ok(())
}

/// Windows has no directory-sync door; the `Result` is load-bearing on POSIX.
#[cfg_attr(
    windows,
    expect(
        clippy::unnecessary_wraps,
        reason = "directory sync fails only on POSIX"
    )
)]
fn sync_parent_directory(path: &Path, operation: &'static str) -> Result<(), JournalError> {
    #[cfg(windows)]
    {
        let _ = (path, operation);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| jio(operation, path, source))
    }
}

fn cleanup_created_file(path: &Path, error: JournalError) -> JournalError {
    if let Err(source) = fs::remove_file(path) {
        let cleanup = jio("create", path, source);
        return JournalError::Damaged {
            path: path.to_path_buf(),
            cause: format!("{error}; removing the partial journal also failed: {cleanup}").into(),
        };
    }
    if let Err(cleanup) = sync_parent_directory(path, "sync") {
        return JournalError::Damaged {
            path: path.to_path_buf(),
            cause: format!("{error}; syncing the journal removal also failed: {cleanup}").into(),
        };
    }
    error
}

/// Finds the offset just after the last complete `\n`-terminated record.
///
/// Returns `None` for an empty file, `Some(0)` when no newline exists, and
/// `Some(len)` when the file ends cleanly.
fn last_record_end(file: &mut File, len: u64, path: &Path) -> Result<Option<u64>, JournalError> {
    if len == 0 {
        return Ok(None);
    }
    let mut window = vec![0_u8; usize::try_from(READ_WINDOW).unwrap_or(usize::MAX)];
    let mut pos = len;
    loop {
        let start = pos.saturating_sub(READ_WINDOW);
        let count = usize::try_from(pos - start).unwrap_or(usize::MAX);
        file.seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut window[..count]))
            .map_err(|source| jio("read", path, source))?;
        if let Some(index) = window[..count].iter().rposition(|byte| *byte == b'\n') {
            let index = u64::try_from(index)
                .map_err(|_| jio("read", path, io::Error::other("window index exceeds u64")))?;
            return Ok(Some(start.saturating_add(index).saturating_add(1)));
        }
        if start == 0 {
            return Ok(Some(0));
        }
        pos = start;
    }
}

/// Validates the complete prefix and enforces the torn-tail size limit.
fn scan_complete_prefix(
    file: &mut File,
    path: &Path,
    file_len: u64,
    buffer: &mut [u8],
    visit: impl FnMut(u64, Record),
) -> Result<(u64, Validator), OpenFailure> {
    let complete_end = last_record_end(file, file_len, path)
        .map_err(OpenFailure::Journal)?
        .unwrap_or(0);
    if complete_end == 0 {
        return Err(damaged(0, "the file has no complete record"));
    }
    let validator = scan_and_validate(file, path, complete_end, buffer, visit)?;
    if file_len - complete_end >= MAX_RECORD {
        return Err(OpenFailure::Journal(JournalError::TooLong {
            path: path.to_path_buf(),
            offset: complete_end,
        }));
    }
    Ok((complete_end, validator))
}

/// Stateful structural index shared by journal open and store append validation.
#[derive(Debug)]
pub(crate) struct Validator {
    record_count: usize,
    entries: HashSet<EntryId>,
    last_entry_id: Option<u64>,
    next_entry_id: Option<u64>,
    leaf: Option<EntryId>,
    next_turn_id: Option<u64>,
    open_turn: Option<TurnId>,
    jobs: HashMap<JobId, bool>,
    grants: HashSet<JobId>,
    ended_grants: HashSet<JobId>,
    boot_count: u64,
    last_boot_gen: u64,
}

/// Batch-local structural additions, committed only after durable append.
#[derive(Debug)]
pub(crate) struct ValidationDelta {
    record_count: usize,
    entries: HashSet<EntryId>,
    last_entry_id: Option<u64>,
    next_entry_id: Option<u64>,
    leaf: Option<EntryId>,
    next_turn_id: Option<u64>,
    open_turn: Option<TurnId>,
    jobs: HashMap<JobId, bool>,
    grants: HashSet<JobId>,
    ended_grants: HashSet<JobId>,
    boot_count: u64,
    last_boot_gen: u64,
}

impl Validator {
    /// Creates an empty validator for a new session.
    pub(crate) fn new() -> Self {
        Self {
            record_count: 0,
            entries: HashSet::new(),
            last_entry_id: None,
            next_entry_id: Some(1),
            leaf: None,
            next_turn_id: Some(1),
            open_turn: None,
            jobs: HashMap::new(),
            grants: HashSet::new(),
            ended_grants: HashSet::new(),
            boot_count: 0,
            last_boot_gen: 0,
        }
    }

    /// Returns the current tree leaf.
    pub(crate) fn leaf(&self) -> Option<EntryId> {
        self.leaf
    }

    /// Returns the next id after the greatest entry id already seen.
    pub(crate) fn next_entry_id(&self) -> Option<u64> {
        self.next_entry_id
    }

    /// Starts batch-local state without copying the existing index.
    pub(crate) fn begin_batch(&self) -> ValidationDelta {
        ValidationDelta {
            record_count: self.record_count,
            entries: HashSet::new(),
            last_entry_id: self.last_entry_id,
            next_entry_id: self.next_entry_id,
            leaf: self.leaf,
            next_turn_id: self.next_turn_id,
            open_turn: self.open_turn,
            jobs: HashMap::new(),
            grants: HashSet::new(),
            ended_grants: HashSet::new(),
            boot_count: self.boot_count,
            last_boot_gen: self.last_boot_gen,
        }
    }

    /// Validates a batch without mutating this validator.
    pub(crate) fn prepare_batch(&self, records: &[Record]) -> Result<ValidationDelta, OpenFailure> {
        let mut delta = self.begin_batch();
        for record in records {
            self.stage_record(&mut delta, record, 0)?;
        }
        Ok(delta)
    }

    /// Applies a previously validated batch after its append receipt succeeds.
    pub(crate) fn commit(&mut self, delta: ValidationDelta) {
        self.record_count = delta.record_count;
        self.entries.extend(delta.entries);
        self.last_entry_id = delta.last_entry_id;
        self.next_entry_id = delta.next_entry_id;
        self.leaf = delta.leaf;
        self.next_turn_id = delta.next_turn_id;
        self.open_turn = delta.open_turn;
        self.jobs.extend(delta.jobs);
        self.grants.extend(delta.grants);
        self.ended_grants.extend(delta.ended_grants);
        self.boot_count = delta.boot_count;
        self.last_boot_gen = delta.last_boot_gen;
    }

    /// Applies one record to batch-local state.
    pub(crate) fn stage_record(
        &self,
        delta: &mut ValidationDelta,
        record: &Record,
        offset: u64,
    ) -> Result<(), OpenFailure> {
        Self::validate_record_position(delta, record, offset)?;
        self.stage_record_body(delta, record, offset)?;
        Self::finish_record(delta, record, offset)
    }

    fn validate_record_position(
        delta: &ValidationDelta,
        record: &Record,
        offset: u64,
    ) -> Result<(), OpenFailure> {
        if delta.record_count == 0 && !matches!(record, Record::Session(_)) {
            return Err(damaged(offset, "the first record is not a session header"));
        }
        if delta.record_count > 0 && matches!(record, Record::Session(_)) {
            return Err(damaged(offset, "a second session header"));
        }
        Ok(())
    }

    fn stage_record_body(
        &self,
        delta: &mut ValidationDelta,
        record: &Record,
        offset: u64,
    ) -> Result<(), OpenFailure> {
        match record {
            Record::User(entry)
            | Record::Assistant(entry)
            | Record::ToolResult(entry)
            | Record::Reminder(entry)
            | Record::Model(entry)
            | Record::Thinking(entry)
            | Record::Approval(entry)
            | Record::Mode(entry)
            | Record::Compaction(entry)
            | Record::BranchSummary(entry) => self.stage_entry(delta, entry, offset)?,
            Record::Leaf { to: Some(to), .. } | Record::Label { entry: to, .. } => {
                self.stage_reference(delta, *to, offset)?;
            }
            Record::RuleFired { entry, .. } => self.stage_reference(delta, *entry, offset)?,
            Record::ToolPromoted {
                leaf: Some(leaf), ..
            } => {
                self.stage_reference(delta, *leaf, offset)?;
            }
            Record::TurnStart { turn, .. } => {
                if let Some(open) = delta.open_turn {
                    return Err(damaged(
                        offset,
                        format!("turn {turn} starts while turn {open} is open"),
                    ));
                }
                if delta.next_turn_id != Some(turn.get()) {
                    let reason = match delta.next_turn_id {
                        Some(expected) => format!("turn {turn} does not follow turn {expected}"),
                        None => "turn id counter is exhausted".to_owned(),
                    };
                    return Err(damaged(offset, reason));
                }
                delta.next_turn_id = turn.get().checked_add(1);
                delta.open_turn = Some(*turn);
            }
            Record::ToolStart { turn, .. } if delta.open_turn != Some(*turn) => {
                return Err(damaged(
                    offset,
                    format!("tool start for turn {turn} but it is not open"),
                ));
            }
            Record::TurnEnd { turn, .. } => match delta.open_turn {
                Some(open) if open == *turn => delta.open_turn = None,
                _ => {
                    return Err(damaged(
                        offset,
                        format!("turn {turn} ends but it is not open"),
                    ));
                }
            },
            Record::Job { job, event, .. } => self.stage_job(delta, *job, event, offset)?,
            Record::ScopedGrant { job, .. } => {
                if self.grants.contains(job) || !delta.grants.insert(*job) {
                    return Err(damaged(
                        offset,
                        format!("job {job} has more than one scoped grant"),
                    ));
                }
            }
            Record::ScopedGrantEnded { job, .. } => {
                if !self.grants.contains(job) && !delta.grants.contains(job) {
                    return Err(damaged(
                        offset,
                        format!("job {job} has no scoped grant to end"),
                    ));
                }
                if self.ended_grants.contains(job) || !delta.ended_grants.insert(*job) {
                    return Err(damaged(
                        offset,
                        format!("job {job} has more than one scoped grant end"),
                    ));
                }
            }
            Record::Boot { r#gen, .. } => {
                let Some(count) = delta.boot_count.checked_add(1) else {
                    return Err(damaged(offset, "boot count is exhausted"));
                };
                if r#gen.get() <= delta.last_boot_gen {
                    return Err(damaged(
                        offset,
                        format!(
                            "boot generation {} does not follow generation {}",
                            r#gen.get(),
                            delta.last_boot_gen
                        ),
                    ));
                }
                delta.boot_count = count;
                delta.last_boot_gen = r#gen.get();
            }
            _ => {}
        }
        Ok(())
    }

    fn finish_record(
        delta: &mut ValidationDelta,
        record: &Record,
        offset: u64,
    ) -> Result<(), OpenFailure> {
        match record {
            Record::Leaf { to, .. } => delta.leaf = *to,
            _ => {
                if let Some(entry) = record.entry() {
                    delta.leaf = Some(entry.id);
                }
            }
        }
        delta.record_count = delta
            .record_count
            .checked_add(1)
            .ok_or_else(|| damaged(offset, "record count is exhausted"))?;
        Ok(())
    }

    fn stage_entry(
        &self,
        delta: &mut ValidationDelta,
        entry: &dal_core::Entry,
        offset: u64,
    ) -> Result<(), OpenFailure> {
        if self.entries.contains(&entry.id) || delta.entries.contains(&entry.id) {
            return Err(damaged(offset, format!("duplicate entry id {}", entry.id)));
        }
        if let Some(last) = delta.last_entry_id
            && entry.id.get() <= last
        {
            return Err(damaged(
                offset,
                format!(
                    "entry id {} is not greater than earlier entry id {last}",
                    entry.id
                ),
            ));
        }
        if let Some(parent) = entry.parent {
            self.stage_reference(delta, parent, offset)?;
        }
        match &entry.kind {
            dal_core::EntryKind::BranchSummary { from, .. } => {
                self.stage_reference(delta, *from, offset)?;
            }
            dal_core::EntryKind::Compaction {
                first_kept: Some(first_kept),
                ..
            } => self.stage_reference(delta, *first_kept, offset)?,
            _ => {}
        }
        delta.entries.insert(entry.id);
        delta.last_entry_id = Some(entry.id.get());
        delta.next_entry_id = entry.id.get().checked_add(1);
        delta.leaf = Some(entry.id);
        Ok(())
    }

    fn stage_reference(
        &self,
        delta: &ValidationDelta,
        entry: EntryId,
        offset: u64,
    ) -> Result<(), OpenFailure> {
        if self.entries.contains(&entry) || delta.entries.contains(&entry) {
            return Ok(());
        }
        Err(damaged(
            offset,
            format!("record refers to entry {entry}, which does not exist"),
        ))
    }

    fn stage_job(
        &self,
        delta: &mut ValidationDelta,
        job: JobId,
        event: &JobEvent,
        offset: u64,
    ) -> Result<(), OpenFailure> {
        if let JobEvent::Started { .. } = event {
            if self.jobs.contains_key(&job) || delta.jobs.contains_key(&job) {
                return Err(damaged(offset, format!("job {job} starts more than once")));
            }
            delta.jobs.insert(job, false);
            return Ok(());
        }

        let terminal = delta
            .jobs
            .get(&job)
            .or_else(|| self.jobs.get(&job))
            .copied()
            .ok_or_else(|| damaged(offset, format!("job {job} ends but it did not start")))?;
        if terminal {
            return Err(damaged(
                offset,
                format!("job {job} has more than one terminal event"),
            ));
        }
        delta.jobs.insert(job, true);
        Ok(())
    }
}

fn damaged(offset: u64, reason: impl Into<Box<str>>) -> OpenFailure {
    OpenFailure::Damaged {
        offset,
        reason: reason.into(),
    }
}

/// Reads complete lines in bounded chunks, decoding and validating before visiting.
fn scan_and_validate(
    file: &mut File,
    path: &Path,
    complete_end: u64,
    buffer: &mut [u8],
    mut visit: impl FnMut(u64, Record),
) -> Result<Validator, OpenFailure> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| OpenFailure::Journal(jio("read", path, source)))?;
    let max_record = usize::try_from(MAX_RECORD).unwrap_or(usize::MAX);
    let buffer_bytes = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
    let mut line = Vec::with_capacity(buffer.len());
    let mut validator = Validator::new();
    let mut delta = validator.begin_batch();
    let mut scanned = 0_u64;
    let mut line_offset = 0_u64;
    while scanned < complete_end {
        let read_len =
            usize::try_from((complete_end - scanned).min(buffer_bytes)).unwrap_or(buffer.len());
        let count = file
            .read(&mut buffer[..read_len])
            .map_err(|source| OpenFailure::Journal(jio("read", path, source)))?;
        if count == 0 {
            return Err(OpenFailure::Journal(jio(
                "read",
                path,
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "journal changed during preflight",
                ),
            )));
        }
        scanned = scanned.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        let mut start = 0;
        while start < count {
            let newline = buffer[start..count].iter().position(|byte| *byte == b'\n');
            let end = newline.map_or(count, |index| start + index);
            append_record_bytes(
                &mut line,
                &buffer[start..end],
                max_record,
                path,
                line_offset,
            )?;
            let Some(_) = newline else {
                break;
            };
            if line.is_empty() {
                return Err(damaged(line_offset, "empty record"));
            }
            let line_bytes = line.len() + 1;
            if line_bytes > max_record {
                return Err(OpenFailure::Journal(JournalError::TooLong {
                    path: path.to_path_buf(),
                    offset: line_offset,
                }));
            }
            let record = decode_line(&line, line_offset)?;
            validator.stage_record(&mut delta, &record, line_offset)?;
            visit(line_offset, record);
            line_offset = line_offset.saturating_add(u64::try_from(line_bytes).unwrap_or(u64::MAX));
            line.clear();
            start = end + 1;
        }
    }
    if !line.is_empty() || line_offset != complete_end {
        return Err(damaged(line_offset, "the file has no complete record"));
    }
    validator.commit(delta);
    Ok(validator)
}

/// Metadata from a read-only validation of a journal's complete prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PrefixSummary {
    /// The current journal file length, including any torn tail.
    pub(crate) file_len: u64,
    /// The offset after the last complete record.
    pub(crate) complete_end: u64,
    /// The leaf after the complete prefix.
    pub(crate) leaf: Option<EntryId>,
    /// The number of boot records in the complete prefix.
    pub(crate) boot_count: u64,
}

/// Visits each validated complete record without repairing or writing the file.
///
/// # Errors
/// Returns [`OpenFailure`] for a damaged prefix, unsupported version, or I/O
/// failure. Valid records before an error have already been passed to `visit`.
pub(crate) fn scan_prefix(
    path: &Path,
    visit: impl FnMut(u64, Record),
) -> Result<PrefixSummary, OpenFailure> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|source| OpenFailure::Journal(jio("open", path, source)))?;
    let file_len = file
        .metadata()
        .map_err(|source| OpenFailure::Journal(jio("read", path, source)))?
        .len();
    let mut buffer = vec![0_u8; SCAN_BUFFER];
    let (complete_end, validator) =
        scan_complete_prefix(&mut file, path, file_len, &mut buffer, visit)?;
    Ok(PrefixSummary {
        file_len,
        complete_end,
        leaf: validator.leaf(),
        boot_count: validator.boot_count,
    })
}

fn append_record_bytes(
    line: &mut Vec<u8>,
    bytes: &[u8],
    max_record: usize,
    path: &Path,
    offset: u64,
) -> Result<(), OpenFailure> {
    if line
        .len()
        .checked_add(bytes.len())
        .is_none_or(|len| len >= max_record)
    {
        return Err(OpenFailure::Journal(JournalError::TooLong {
            path: path.to_path_buf(),
            offset,
        }));
    }
    line.extend_from_slice(bytes);
    Ok(())
}

fn decode_line(line: &[u8], offset: u64) -> Result<Record, OpenFailure> {
    match decode(line) {
        Ok(decoded) => Ok(decoded.record),
        Err(DecodeError::UnsupportedVersion { found }) => Err(OpenFailure::UnknownVersion(found)),
        Err(DecodeError::MissingVersion) => Err(damaged(offset, "record has no format version")),
        Err(DecodeError::UnknownRecordKind { kind }) => {
            Err(damaged(offset, format!("unknown record type {kind:?}")))
        }
        Err(DecodeError::Invalid { message, .. }) => {
            Err(damaged(offset, format!("invalid JSON: {message}")))
        }
    }
}

fn recovery_batch(
    records: &[(u64, Record)],
    r#gen: Gen,
    next_entry_id: Option<u64>,
    leaf: Option<EntryId>,
) -> Result<(Vec<Record>, Option<AbortedTurn>), OpenFailure> {
    let at = jiff::Timestamp::now();
    let mut repair = Vec::new();
    let mut started_jobs = Vec::new();
    let mut ended_jobs = HashSet::new();
    let mut grants = Vec::new();
    let mut ended_grants = HashSet::new();
    for (_, record) in records {
        match record {
            Record::Job { job, event, .. } => match event {
                JobEvent::Started { .. } => started_jobs.push(*job),
                _ => {
                    ended_jobs.insert(*job);
                }
            },
            Record::ScopedGrant { job, .. } => grants.push(*job),
            Record::ScopedGrantEnded { job, .. } => {
                ended_grants.insert(*job);
            }
            _ => {}
        }
    }
    for job in started_jobs {
        if !ended_jobs.contains(&job) {
            repair.push(Record::Job {
                at,
                job,
                event: JobEvent::Orphaned,
            });
        }
    }
    for job in grants {
        if !ended_grants.contains(&job) {
            repair.push(Record::ScopedGrantEnded { at, job });
        }
    }
    let aborted = aborted_turn(records, next_entry_id, leaf, at, &mut repair)?;
    repair.push(Record::Boot {
        at,
        r#gen,
        version: env!("CARGO_PKG_VERSION").into(),
    });
    Ok((repair, aborted))
}

fn open_turn(records: &[(u64, Record)]) -> Option<(usize, TurnId)> {
    let mut open = None;
    for (index, (_, record)) in records.iter().enumerate() {
        match record {
            Record::TurnStart { turn, .. } => open = Some((index, *turn)),
            Record::TurnEnd { .. } => open = None,
            _ => {}
        }
    }
    open
}

fn aborted_turn(
    records: &[(u64, Record)],
    mut next_entry_id: Option<u64>,
    leaf: Option<EntryId>,
    at: jiff::Timestamp,
    repair: &mut Vec<Record>,
) -> Result<Option<AbortedTurn>, OpenFailure> {
    let Some((open_at, turn)) = open_turn(records) else {
        return Ok(None);
    };
    let start_offset = records[open_at].0;
    let (usage, changes) = turn_totals(&records[open_at + 1..])?;
    let mut started_calls = HashSet::new();
    let mut finished_calls = HashSet::new();
    for (_, record) in &records[open_at + 1..] {
        match record {
            Record::ToolStart { call, .. } => {
                started_calls.insert(call.clone());
            }
            Record::ToolResult(entry) => {
                if let dal_core::EntryKind::ToolResult { call, .. } = &entry.kind {
                    finished_calls.insert(call.clone());
                }
            }
            _ => {}
        }
    }
    let mut reported_calls = HashSet::new();
    let mut interrupted = 0_u32;
    let mut not_run = 0_u32;
    let mut parent = leaf;
    for (_, record) in &records[open_at + 1..] {
        let Some(entry) = record.entry() else {
            continue;
        };
        let dal_core::EntryKind::Assistant { content, .. } = &entry.kind else {
            continue;
        };
        for block in content {
            let dal_core::Block::ToolCall { id, name, .. } = block else {
                continue;
            };
            if finished_calls.contains(id) || !reported_calls.insert(id.clone()) {
                continue;
            }
            let ran = started_calls.contains(id);
            let count = if ran { &mut interrupted } else { &mut not_run };
            *count = count
                .checked_add(1)
                .ok_or_else(|| damaged(start_offset, "too many unfinished tool calls"))?;
            let next = next_entry_id
                .ok_or_else(|| damaged(start_offset, "entry id counter is exhausted"))?;
            let entry_id = EntryId::new(
                core::num::NonZeroU64::new(next)
                    .ok_or_else(|| damaged(start_offset, "entry id counter is exhausted"))?,
            );
            next_entry_id = next.checked_add(1);
            repair.push(tool_result(
                id.clone(),
                name.clone(),
                ran,
                at,
                entry_id,
                parent,
            ));
            parent = Some(entry_id);
        }
    }
    repair.push(Record::TurnEnd {
        at,
        turn,
        stop: TurnEndStop::Aborted,
        usage,
        changes,
    });
    Ok(Some(AbortedTurn {
        turn,
        interrupted,
        not_run,
    }))
}

/// Sums usage and file changes from records in one open turn.
pub(crate) fn turn_totals(
    records: &[(u64, Record)],
) -> Result<(Option<Usage>, Vec<dal_core::FileChange>), OpenFailure> {
    let mut usage = UsageTotal::new();
    let mut changes: Vec<dal_core::FileChange> = Vec::new();
    let mut change_indexes = HashMap::<&str, usize>::new();
    for (offset, record) in records {
        match record {
            Record::Assistant(entry) => {
                if let dal_core::EntryKind::Assistant {
                    usage: entry_usage, ..
                } = &entry.kind
                {
                    usage.add(*entry_usage, *offset)?;
                }
            }
            Record::Compaction(entry) => {
                if let dal_core::EntryKind::Compaction {
                    usage: Some(entry_usage),
                    ..
                } = &entry.kind
                {
                    usage.add(*entry_usage, *offset)?;
                }
            }
            Record::Inferred {
                usage: entry_usage, ..
            } => usage.add(*entry_usage, *offset)?,
            Record::ToolResult(entry) => {
                if let dal_core::EntryKind::ToolResult {
                    changes: entry_changes,
                    ..
                } = &entry.kind
                {
                    for change in entry_changes {
                        if let Some(index) = change_indexes.get(change.path.as_ref()).copied() {
                            changes[index].added = checked_total(
                                changes[index].added,
                                change.added,
                                *offset,
                                "turn change total overflows",
                            )?;
                            changes[index].removed = checked_total(
                                changes[index].removed,
                                change.removed,
                                *offset,
                                "turn change total overflows",
                            )?;
                        } else {
                            change_indexes.insert(change.path.as_ref(), changes.len());
                            changes.push(change.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok((usage.finish(), changes))
}

struct UsageTotal {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    reasoning_known: bool,
    cache_write_tokens: u64,
    cost_usd: f64,
    cost_known: bool,
    seen: bool,
}

impl UsageTotal {
    fn new() -> Self {
        Self {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            reasoning_known: true,
            cache_write_tokens: 0,
            cost_usd: 0.0,
            cost_known: true,
            seen: false,
        }
    }

    fn add(&mut self, usage: Usage, offset: u64) -> Result<(), OpenFailure> {
        self.seen = true;
        self.input_tokens = checked_total(
            self.input_tokens,
            usage.input_tokens,
            offset,
            "turn usage total overflows",
        )?;
        self.cached_input_tokens = checked_total(
            self.cached_input_tokens,
            usage.cached_input_tokens,
            offset,
            "turn usage total overflows",
        )?;
        self.output_tokens = checked_total(
            self.output_tokens,
            usage.output_tokens,
            offset,
            "turn usage total overflows",
        )?;
        match usage.reasoning_tokens {
            Some(reasoning) => {
                self.reasoning_tokens = checked_total(
                    self.reasoning_tokens,
                    reasoning,
                    offset,
                    "turn usage total overflows",
                )?;
            }
            None => self.reasoning_known = false,
        }
        self.cache_write_tokens = checked_total(
            self.cache_write_tokens,
            usage.cache_write_tokens,
            offset,
            "turn usage total overflows",
        )?;
        match usage.cost_usd {
            Some(cost) => {
                let total = self.cost_usd + cost;
                if !total.is_finite() {
                    return Err(damaged(offset, "turn usage cost total overflows"));
                }
                self.cost_usd = total;
            }
            None => self.cost_known = false,
        }
        Ok(())
    }

    fn finish(self) -> Option<Usage> {
        self.seen.then_some(Usage {
            input_tokens: self.input_tokens,
            cached_input_tokens: self.cached_input_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_known.then_some(self.reasoning_tokens),
            cache_write_tokens: self.cache_write_tokens,
            cost_usd: self.cost_known.then_some(self.cost_usd),
        })
    }
}

fn checked_total(
    total: u64,
    value: u64,
    offset: u64,
    reason: &'static str,
) -> Result<u64, OpenFailure> {
    total
        .checked_add(value)
        .ok_or_else(|| damaged(offset, reason))
}

fn tool_result(
    call: dal_core::CallId,
    name: Box<str>,
    ran: bool,
    at: jiff::Timestamp,
    id: EntryId,
    parent: Option<EntryId>,
) -> Record {
    let text: Box<str> = if ran {
        crate::error::INTERRUPTED_CALL.into()
    } else {
        crate::error::NOT_RUN_CALL.into()
    };
    Record::ToolResult(dal_core::Entry {
        id,
        parent,
        at,
        kind: dal_core::EntryKind::ToolResult {
            call,
            name,
            error: true,
            parts: vec![dal_core::JournalPart::Text { text }],
            changes: Vec::new(),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        num::NonZeroU64,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use crate::error::{INTERRUPTED_CALL, NOT_RUN_CALL};
    use dal_core::{
        AssistantStop, Block, CallId, ClientId, Entry, EntryKind, FileChange, Header, JobEvent,
        JobId, JournalPart, Product, RawJson, SessionId, TurnEndStop, TurnId, Usage, Workspace,
    };

    use super::*;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let executable = std::env::current_exe().expect("test executable path");
            let parent = executable.parent().expect("test executable parent");
            let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("dal-store-journal-{}-{suffix}", std::process::id()));
            fs::create_dir_all(&path).expect("create isolated journal test directory");
            Self(path)
        }

        fn journal(&self, contents: &[u8]) -> PathBuf {
            let path = self.0.join("journal.jsonl");
            fs::write(&path, contents).expect("write journal fixture");
            path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn timestamp() -> jiff::Timestamp {
        "2026-09-28T10:15:30.000Z"
            .parse()
            .expect("valid test timestamp")
    }

    fn entry_id(value: u64) -> EntryId {
        EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
    }

    fn turn_id(value: u64) -> TurnId {
        TurnId::new(NonZeroU64::new(value).expect("nonzero test turn id"))
    }

    fn session_record() -> Record {
        let executable = std::env::current_exe().expect("test executable path");
        let workspace = Workspace::new(
            executable
                .parent()
                .expect("test executable parent")
                .to_path_buf(),
        )
        .expect("absolute test workspace");
        Record::Session(Header {
            id: SessionId::new_v7(),
            at: timestamp(),
            workspace,
            product: Product::Dalgona,
            from: None,
        })
    }

    fn boot_record(generation: u64) -> Record {
        Record::Boot {
            at: timestamp(),
            r#gen: Gen::new(NonZeroU64::new(generation).expect("nonzero test generation")),
            version: "test".into(),
        }
    }

    fn usage(input_tokens: u64) -> Usage {
        Usage {
            input_tokens,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        }
    }

    fn assistant_record(
        id: u64,
        parent: Option<EntryId>,
        calls: &[(&str, &str)],
        input_tokens: u64,
    ) -> Record {
        let content = calls
            .iter()
            .map(|(call, name)| Block::ToolCall {
                id: CallId::new(*call),
                name: (*name).into(),
                input: RawJson::parse("{}").expect("valid test tool input"),
            })
            .collect();
        Record::Assistant(Entry {
            id: entry_id(id),
            parent,
            at: timestamp(),
            kind: EntryKind::Assistant {
                api: dal_core::Family::Chat,
                model: "test-model".into(),
                content,
                usage: usage(input_tokens),
                stop: AssistantStop::ToolUse,
            },
        })
    }

    fn user_record(id: u64, parent: Option<EntryId>) -> Record {
        Record::User(Entry {
            id: entry_id(id),
            parent,
            at: timestamp(),
            kind: EntryKind::User { parts: Vec::new() },
        })
    }

    fn tool_result_entry(
        id: u64,
        parent: Option<EntryId>,
        call: &str,
        path: &str,
        added: u64,
        removed: u64,
    ) -> Record {
        Record::ToolResult(Entry {
            id: entry_id(id),
            parent,
            at: timestamp(),
            kind: EntryKind::ToolResult {
                call: CallId::new(call),
                name: "test-tool".into(),
                error: false,
                parts: Vec::new(),
                changes: vec![FileChange {
                    path: path.into(),
                    added,
                    removed,
                }],
            },
        })
    }

    fn encode_records(records: &[Record]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for record in records {
            bytes.extend_from_slice(&dal_core::encode(record).expect("encode test record"));
        }
        bytes
    }

    fn tool_result_text(record: &Record) -> Option<&str> {
        let Record::ToolResult(entry) = record else {
            return None;
        };
        let EntryKind::ToolResult { parts, .. } = &entry.kind else {
            return None;
        };
        match parts.first()? {
            JournalPart::Text { text } => Some(text),
            _ => None,
        }
    }

    #[test]
    fn unknown_version_with_torn_tail_preserves_file_and_existing_sidefile() {
        let directory = TestDirectory::new();
        let mut prefix = encode_records(&[session_record()]);
        prefix.extend_from_slice(b"{\"v\":2,\"type\":\"future\"}\n");
        let tail = b"unfinished";
        let mut original = prefix.clone();
        original.extend_from_slice(tail);
        let path = directory.journal(&original);
        let kept_at = path.with_file_name(format!(
            "torn-{}.jsonl",
            u64::try_from(prefix.len()).expect("test offset")
        ));
        fs::write(&kept_at, b"preserve this sidefile").expect("write sidefile sentinel");

        assert!(matches!(
            Journal::open(&path, &Faults::default()),
            Err(OpenFailure::UnknownVersion(2))
        ));
        assert_eq!(fs::read(&path).expect("read journal"), original);
        assert_eq!(
            fs::read(kept_at).expect("read sidefile"),
            b"preserve this sidefile"
        );
    }

    #[test]
    fn invalid_complete_prefix_with_torn_tail_is_not_repaired() {
        let directory = TestDirectory::new();
        let mut prefix = encode_records(&[session_record()]);
        prefix.extend_from_slice(&encode_records(&[Record::TurnEnd {
            at: timestamp(),
            turn: turn_id(1),
            stop: TurnEndStop::Done,
            usage: None,
            changes: Vec::new(),
        }]));
        let mut original = prefix.clone();
        original.extend_from_slice(b"unfinished");
        let path = directory.journal(&original);
        let kept_at = path.with_file_name(format!(
            "torn-{}.jsonl",
            u64::try_from(prefix.len()).expect("test offset")
        ));
        fs::write(&kept_at, b"preserve this sidefile").expect("write sidefile sentinel");

        assert!(matches!(
            Journal::open(&path, &Faults::default()),
            Err(OpenFailure::Damaged {
                offset: _,
                reason
            }) if reason.as_ref() == "turn 1 ends but it is not open"
        ));
        assert_eq!(fs::read(&path).expect("read journal"), original);
        assert_eq!(
            fs::read(kept_at).expect("read sidefile"),
            b"preserve this sidefile"
        );
    }

    #[test]
    fn headerless_complete_prefix_with_torn_tail_is_not_repaired() {
        let directory = TestDirectory::new();
        let mut original = encode_records(&[boot_record(1)]);
        let tail_start = u64::try_from(original.len()).expect("test offset");
        original.extend_from_slice(b"unfinished");
        let path = directory.journal(&original);
        let kept_at = path.with_file_name(format!("torn-{tail_start}.jsonl"));

        assert!(matches!(
            Journal::open(&path, &Faults::default()),
            Err(OpenFailure::Damaged {
                offset: 0,
                reason
            }) if reason.as_ref() == "the first record is not a session header"
        ));
        assert_eq!(fs::read(&path).expect("read unchanged journal"), original);
        assert!(!kept_at.exists());
    }

    #[test]
    fn valid_large_torn_tail_is_kept_exactly_and_reopen_is_stable() {
        let directory = TestDirectory::new();
        let prefix = encode_records(&[session_record(), boot_record(1)]);
        let tail = vec![0xff; usize::try_from(READ_WINDOW).expect("window size") + 123];
        let mut original = prefix.clone();
        original.extend_from_slice(&tail);
        let path = directory.journal(&original);
        let offset = u64::try_from(prefix.len()).expect("test offset");
        let kept_at = path.with_file_name(format!("torn-{offset}.jsonl"));
        fs::write(&kept_at, b"replace this sidefile").expect("write old sidefile");

        let opened = Journal::open(&path, &Faults::default()).expect("repair valid tail");
        let torn = opened.report.torn.expect("report repaired tail");
        assert_eq!(torn.offset, offset);
        assert_eq!(torn.bytes, u64::try_from(tail.len()).expect("tail length"));
        assert_eq!(fs::read(&path).expect("read repaired journal"), prefix);
        assert_eq!(
            fs::read(&torn.kept_at).expect("read quarantined tail"),
            tail
        );

        let reopened = Journal::open(&path, &Faults::default()).expect("reopen repaired file");
        assert!(reopened.report.torn.is_none());
    }

    #[test]
    fn empty_and_wholly_torn_files_are_refused_without_torn_zero_sidefile() {
        for original in [Vec::new(), b"unfinished".to_vec()] {
            let directory = TestDirectory::new();
            let path = directory.journal(&original);
            let failure = Journal::open(&path, &Faults::default()).expect_err("no complete record");
            assert!(matches!(
                failure,
                OpenFailure::Damaged {
                    offset: 0,
                    reason
                } if reason.as_ref() == "the file has no complete record"
            ));
            assert_eq!(fs::read(&path).expect("read unchanged journal"), original);
            assert!(!path.with_file_name("torn-0.jsonl").exists());
        }
    }

    #[test]
    fn quarantine_failure_never_truncates_the_journal() {
        let directory = TestDirectory::new();
        let mut original = encode_records(&[session_record()]);
        let tail_start = u64::try_from(original.len()).expect("test offset");
        original.extend_from_slice(b"unfinished");
        let path = directory.journal(&original);
        let kept_at = path.with_file_name(format!("torn-{tail_start}.jsonl"));
        let failure = Journal::open(
            &path,
            &Faults {
                quarantine_error: true,
                ..Faults::default()
            },
        )
        .expect_err("injected quarantine failure");

        assert!(matches!(
            failure,
            OpenFailure::Journal(JournalError::Io {
                op: "quarantine",
                ..
            })
        ));
        assert_eq!(fs::read(&path).expect("read unchanged journal"), original);
        assert!(!kept_at.exists());
    }

    #[cfg(not(windows))]
    #[test]
    fn directory_sync_failure_keeps_the_original_journal_tail() {
        let directory = TestDirectory::new();
        let mut original = encode_records(&[session_record()]);
        let tail_start = u64::try_from(original.len()).expect("test offset");
        let tail = b"unfinished";
        original.extend_from_slice(tail);
        let path = directory.journal(&original);
        let kept_at = path.with_file_name(format!("torn-{tail_start}.jsonl"));
        let failure = Journal::open(
            &path,
            &Faults {
                directory_sync_error: true,
                ..Faults::default()
            },
        )
        .expect_err("injected directory sync failure");

        assert!(matches!(
            failure,
            OpenFailure::Journal(JournalError::Io {
                op: "quarantine",
                ..
            })
        ));
        assert_eq!(fs::read(&path).expect("read unchanged journal"), original);
        assert_eq!(fs::read(&kept_at).expect("read exact sidefile"), tail);
    }

    #[test]
    fn failed_initial_create_removes_the_partial_journal() {
        let directory = TestDirectory::new();
        let path = directory.0.join("journal.jsonl");
        let failure = Journal::create(
            &path,
            b"complete batch",
            &Faults {
                write_after_bytes: Some(1),
                ..Faults::default()
            },
        )
        .expect_err("injected create write failure");

        assert!(matches!(failure, JournalError::Io { op: "write", .. }));
        assert!(!path.exists());
    }

    #[test]
    fn overlong_torn_tail_fails_at_its_start_without_quarantine() {
        let directory = TestDirectory::new();
        let prefix = encode_records(&[session_record()]);
        let path = directory.journal(&prefix);
        let prefix_len = u64::try_from(prefix.len()).expect("test offset");
        let expected_len = prefix_len + MAX_RECORD;
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open sparse journal fixture");
        file.set_len(expected_len)
            .expect("extend sparse journal fixture");
        let kept_at = path.with_file_name(format!("torn-{prefix_len}.jsonl"));

        assert!(matches!(
            Journal::open(&path, &Faults::default()),
            Err(OpenFailure::Journal(JournalError::TooLong { offset, .. }))
                if offset == prefix_len
        ));
        assert_eq!(
            fs::metadata(&path).expect("stat unchanged journal").len(),
            expected_len
        );
        assert!(!kept_at.exists());
    }

    #[test]
    fn sparse_increasing_entry_ids_are_valid_after_open() {
        let directory = TestDirectory::new();
        let records = vec![
            session_record(),
            boot_record(1),
            user_record(3, None),
            user_record(7, Some(entry_id(3))),
        ];
        let path = directory.journal(&encode_records(&records));
        let opened = Journal::open(&path, &Faults::default()).expect("open sparse branch path");
        let ids: Vec<_> = opened
            .records
            .iter()
            .filter_map(|(_, record)| record.entry().map(|entry| entry.id.get()))
            .collect();

        assert_eq!(ids, vec![3, 7]);
        assert_eq!(opened.repair_delta.leaf, Some(entry_id(7)));
        assert_eq!(opened.validator.next_entry_id(), Some(8));
    }

    #[test]
    fn descending_and_duplicate_entry_ids_are_refused_without_mutation() {
        for (next_id, expected_reason) in [
            (2, "entry id 2 is not greater than earlier entry id 3"),
            (3, "duplicate entry id 3"),
        ] {
            let directory = TestDirectory::new();
            let records = vec![
                session_record(),
                user_record(3, None),
                user_record(next_id, Some(entry_id(3))),
            ];
            let original = encode_records(&records);
            let path = directory.journal(&original);

            assert!(matches!(
                Journal::open(&path, &Faults::default()),
                Err(OpenFailure::Damaged { reason, .. })
                    if reason.as_ref() == expected_reason
            ));
            assert_eq!(fs::read(&path).expect("read unchanged journal"), original);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "recovery coverage builds a full open turn with jobs and grants in one test"
    )]
    #[test]
    fn recovery_uses_latest_open_turn_unique_ids_and_actual_totals() {
        let directory = TestDirectory::new();
        let records = vec![
            session_record(),
            boot_record(1),
            Record::TurnStart {
                at: timestamp(),
                turn: turn_id(1),
            },
            assistant_record(1, None, &[("closed", "echo")], 100),
            Record::ToolStart {
                at: timestamp(),
                turn: turn_id(1),
                call: CallId::new("closed"),
            },
            tool_result_entry(2, Some(entry_id(1)), "closed", "src/old.rs", 100, 100),
            Record::TurnEnd {
                at: timestamp(),
                turn: turn_id(1),
                stop: TurnEndStop::Done,
                usage: Some(usage(100)),
                changes: Vec::new(),
            },
            Record::TurnStart {
                at: timestamp(),
                turn: turn_id(2),
            },
            assistant_record(
                3,
                Some(entry_id(2)),
                &[
                    ("done-one", "read"),
                    ("pending", "write"),
                    ("not-run", "delete"),
                    ("done-two", "edit"),
                ],
                11,
            ),
            Record::ToolStart {
                at: timestamp(),
                turn: turn_id(2),
                call: CallId::new("done-one"),
            },
            tool_result_entry(4, Some(entry_id(3)), "done-one", "src/current.rs", 2, 1),
            Record::ToolStart {
                at: timestamp(),
                turn: turn_id(2),
                call: CallId::new("done-two"),
            },
            tool_result_entry(5, Some(entry_id(4)), "done-two", "src/current.rs", 3, 4),
            Record::ToolStart {
                at: timestamp(),
                turn: turn_id(2),
                call: CallId::new("pending"),
            },
        ];
        let original = encode_records(&records);
        let path = directory.journal(&original);
        let mut opened = Journal::open(&path, &Faults::default()).expect("recover open turn");

        assert_eq!(
            opened.report.aborted,
            Some(AbortedTurn {
                turn: turn_id(2),
                interrupted: 1,
                not_run: 1,
            })
        );
        let recovered_entries: Vec<_> = opened
            .repair
            .iter()
            .filter_map(|record| match record {
                Record::ToolResult(entry) => Some((entry.id, entry.parent)),
                _ => None,
            })
            .collect();
        assert_eq!(
            recovered_entries,
            vec![
                (entry_id(6), Some(entry_id(5))),
                (entry_id(7), Some(entry_id(6)))
            ]
        );
        assert_eq!(tool_result_text(&opened.repair[0]), Some(INTERRUPTED_CALL));
        assert_eq!(tool_result_text(&opened.repair[1]), Some(NOT_RUN_CALL));
        let Record::TurnEnd {
            turn,
            stop,
            usage: recovered_usage,
            changes,
            ..
        } = &opened.repair[2]
        else {
            panic!("the repair batch has no aborted turn end");
        };
        assert_eq!(*turn, turn_id(2));
        assert_eq!(*stop, TurnEndStop::Aborted);
        assert_eq!(*recovered_usage, Some(usage(11)));
        assert_eq!(
            changes,
            &[FileChange {
                path: "src/current.rs".into(),
                added: 5,
                removed: 5,
            }]
        );

        assert_eq!(opened.validator.next_entry_id(), Some(6));
        assert_eq!(opened.repair_delta.leaf, Some(entry_id(7)));

        let repair_bytes = encode_records(&opened.repair);
        opened
            .journal
            .append(&repair_bytes)
            .expect("persist open-turn repair");
        opened.validator.commit(opened.repair_delta);
        assert_eq!(opened.validator.next_entry_id(), Some(8));

        let reopened = Journal::open(&path, &Faults::default()).expect("reopen recovered journal");
        assert!(reopened.report.aborted.is_none());
        assert_eq!(reopened.repair.len(), 1);
        assert!(matches!(
            &reopened.repair[0],
            Record::Boot { r#gen, .. } if r#gen.get() == 3
        ));
    }

    #[test]
    fn reopen_ends_orphaned_jobs_and_live_scoped_grants() {
        let directory = TestDirectory::new();
        let job = JobId::new_v7();
        let records = vec![
            session_record(),
            boot_record(1),
            Record::Job {
                at: timestamp(),
                job,
                event: JobEvent::Started {
                    kind: Some("extension".into()),
                },
            },
            Record::ScopedGrant {
                at: timestamp(),
                call: CallId::new("call-1"),
                prefix: vec!["git".into()],
                roots: vec!["/workspace".into()],
                job,
                by: ClientId::new("client"),
            },
        ];
        let path = directory.journal(&encode_records(&records));
        let mut opened = Journal::open(&path, &Faults::default()).expect("repair job and grant");

        assert!(matches!(
            &opened.repair[0],
            Record::Job {
                job: found,
                event: JobEvent::Orphaned,
                ..
            } if *found == job
        ));
        assert!(matches!(
            &opened.repair[1],
            Record::ScopedGrantEnded { job: found, .. } if *found == job
        ));

        let repair_bytes = encode_records(&opened.repair);
        opened
            .journal
            .append(&repair_bytes)
            .expect("persist job and grant repair");

        let reopened = Journal::open(&path, &Faults::default()).expect("reopen repaired job");
        assert_eq!(reopened.repair.len(), 1);
        assert!(matches!(
            &reopened.repair[0],
            Record::Boot { r#gen, .. } if r#gen.get() == 3
        ));
    }
}
