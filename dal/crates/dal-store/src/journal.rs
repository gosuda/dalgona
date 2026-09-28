//! The journal file: one write and one sync per batch, torn-tail repair,
//! structure validation, and rollback on failure.
//!
//! The journal speaks bytes; [`crate::session::Session`] encodes records and
//! publishes blobs before the first byte of a batch is written (D-09).

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use dal_core::{
    BranchError, DecodeError, EntryId, Gen, Header, Record, SessionId, TurnEndStop, TurnId, decode,
};

use crate::error::{AbortedTurn, JournalError, OpenReport, TornTail};

/// The largest one record may be, in bytes (D-08).
pub const MAX_RECORD: u64 = 67_108_864;
/// The backward read window for torn-tail repair (D-14).
const READ_WINDOW: u64 = 65_536;

/// A durability fault, injected by tests at a named point (D-41).
///
/// Production code passes [`Faults::default`]; every field `None` or `false`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// Fail the batch write after this many bytes land.
    pub fail_write_after: Option<usize>,
    /// Fail the file sync after a successful write.
    pub fail_sync: bool,
    /// Fail the truncate-back after a failed batch.
    pub fail_truncate: bool,
    /// Fail the torn-tail side-file write at open.
    pub fail_quarantine: bool,
    /// Fail the directory sync after a rename.
    pub fail_dir_sync: bool,
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
pub struct Opened {
    /// The session header; the file's first record.
    pub header: Header,
    /// Every record with its byte offset, in file order.
    pub records: Vec<(u64, Record)>,
    /// The leaf after the last `leaf` record, else the last tree entry.
    pub leaf: Option<EntryId>,
    /// The generation this open will assign to its boot record.
    pub gen: Gen,
    /// Repairs to append in one batch with the boot record (D-15).
    pub repair: Vec<Record>,
    /// Facts for the agent's notices (D-39).
    pub report: OpenReport,
}

#[derive(Debug)]
enum Health {
    Healthy,
    Damaged,
}

/// One open journal file. Neither `Clone` nor `Sync`; one writer per file.
#[derive(Debug)]
pub struct Journal {
    file: File,
    path: PathBuf,
    end: u64,
    health: Health,
    faults: Faults,
}

impl Journal {
    /// Creates a new journal: one write and one sync for the whole batch.
    ///
    /// # Errors
    /// Returns [`JournalError::Io`] when the file cannot be created or synced.
    pub fn create(path: &Path, lines: &[u8], faults: &Faults) -> Result<Self, JournalError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|source| jio("create", path, source))?;
        let journal = Self {
            file,
            path: path.to_path_buf(),
            end: 0,
            health: Health::Healthy,
            faults: faults.clone(),
        };
        let mut this = journal;
        this.write_batch(lines)?;
        this.end = lines.len() as u64;
        Ok(this)
    }

    /// Opens a journal: repair a torn tail, validate structure, plan recovery.
    ///
    /// # Errors
    /// Returns [`JournalError`] for a failed repair step, a failed structure
    /// check (as [`crate::error::StoreError::Damaged`] via the caller), or an
    /// unsupported version (as [`crate::error::StoreError::UnknownVersion`]).
    pub fn open(path: &Path, faults: &Faults) -> Result<Opened, OpenFailure> {
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
        let torn = repair_torn_tail(&mut journal, path, faults)
            .map_err(OpenFailure::Journal)?;
        let (records, leaf, gen) = scan_and_validate(&mut journal, path)?;
        journal.end = journal
            .file
            .metadata()
            .map_err(|source| OpenFailure::Journal(jio("read", path, source)))?
            .len();
        let (repair, aborted) = recovery_batch(&records, gen);
        let report = OpenReport {
            torn,
            aborted,
            gen: u64::from(gen.get()),
        };
        Ok(Opened {
            header: header_of(&records, path)?,
            records,
            leaf,
            gen,
            repair,
            report,
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
    pub fn append(&mut self, lines: &[u8]) -> Result<Receipt, JournalError> {
        if let Health::Damaged = self.health {
            return Err(JournalError::Damaged {
                path: self.path.clone(),
                cause: "journal is damaged".into(),
            });
        }
        let offset = self.end;
        match self.write_batch(lines) {
            Ok(()) => {
                self.end = offset.saturating_add(lines.len() as u64);
                Ok(Receipt {
                    offset,
                    len: lines.len() as u64,
                })
            }
            Err(error) => {
                if self.faults.fail_truncate || self.truncate_back(offset).is_err() {
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

    /// The tracked end offset: the next batch starts here.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.end
    }

    fn truncate_back(&mut self, offset: u64) -> Result<(), JournalError> {
        self.file
            .set_len(offset)
            .map_err(|source| jio("truncate", &self.path, source))?;
        self.file
            .sync_all()
            .map_err(|source| jio("sync", &self.path, source))
    }

    fn write_batch(&mut self, bytes: &[u8]) -> Result<(), JournalError> {
        self.file
            .seek(SeekFrom::Start(self.end))
            .map_err(|source| jio("write", &self.path, source))?;
        match self.faults.fail_write_after {
            Some(limit) => {
                let cut = limit.min(bytes.len());
                self.file
                    .write_all(&bytes[..cut])
                    .map_err(|source| jio("write", &self.path, source))?;
                if cut < bytes.len() {
                    return Err(jio(
                        "write",
                        &self.path,
                        io::Error::new(io::ErrorKind::Other, "injected partial write"),
                    ));
                }
            }
            None => self
                .file
                .write_all(bytes)
                .map_err(|source| jio("write", &self.path, source))?,
        }
        if self.faults.fail_sync {
            return Err(jio(
                "sync",
                &self.path,
                io::Error::new(io::ErrorKind::Other, "injected sync failure"),
            ));
        }
        self.file
            .sync_all()
            .map_err(|source| jio("sync", &self.path, source))
    }
}

/// Why an open failed: a hard version refusal or a journal error.
#[derive(Debug)]
pub enum OpenFailure {
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
        source: format!("{source}").into(),
    }
}

fn header_of(records: &[(u64, Record)], path: &Path) -> Result<Header, OpenFailure> {
    match records.first() {
        Some((_, Record::Session(header))) => Ok(header.clone()),
        _ => Err(OpenFailure::Damaged {
            offset: 0,
            reason: "the first record is not a session header".into(),
        })
        .map_err(|_| OpenFailure::Damaged {
            offset: 0,
            reason: "the first record is not a session header".into(),
        }),
    }
}

/// Moves a torn tail to `torn-<offset>.jsonl`, truncates, and syncs (D-14).
fn repair_torn_tail(
    journal: &mut Journal,
    path: &Path,
    faults: &Faults,
) -> Result<Option<TornTail>, JournalError> {
    let len = journal
        .file
        .metadata()
        .map_err(|source| jio("read", path, source))?
        .len();
    let Some(tail_start) = last_record_end(&mut journal.file, len, path)? else {
        return Ok(None);
    };
    if tail_start == len {
        return Ok(None);
    }
    if faults.fail_quarantine {
        return Err(jio(
            "quarantine",
            path,
            io::Error::new(io::ErrorKind::Other, "injected quarantine failure"),
        ));
    }
    let mut bytes = Vec::new();
    journal
        .file
        .seek(SeekFrom::Start(tail_start))
        .and_then(|_| journal.file.read_to_end(&mut bytes))
        .map_err(|source| jio("read", path, source))?;
    let kept_at = path.with_file_name(format!("torn-{tail_start}.jsonl"));
    write_side_file(&kept_at, &bytes, path, faults)?;
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
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        kept_at,
    }))
}

fn write_side_file(
    kept_at: &Path,
    bytes: &[u8],
    journal_path: &Path,
    faults: &Faults,
) -> Result<(), JournalError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(kept_at)
        .map_err(|source| jio("quarantine", journal_path, source))?;
    file.write_all(bytes)
        .map_err(|source| jio("quarantine", journal_path, source))?;
    file.sync_all()
        .map_err(|source| jio("quarantine", journal_path, source))?;
    if faults.fail_dir_sync {
        return Err(jio(
            "quarantine",
            journal_path,
            io::Error::new(io::ErrorKind::Other, "injected directory sync failure"),
        ));
    }
    let dir = kept_at.parent().unwrap_or(Path::new("."));
    #[cfg(not(windows))]
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|source| jio("quarantine", journal_path, source))?;
    Ok(())
}

/// Finds the offset just after the last complete `\n`-terminated record.
///
/// Returns `None` for an empty file, `Some(0)` when no newline exists, and
/// `Some(len)` when the file ends cleanly.
fn last_record_end(file: &mut File, len: u64, path: &Path) -> Result<Option<u64>, JournalError> {
    if len == 0 {
        return Ok(None);
    }
    let mut window = vec![0_u8; READ_WINDOW as usize];
    let mut pos = len;
    loop {
        let start = pos.saturating_sub(READ_WINDOW);
        let count = usize::try_from(pos - start).unwrap_or(usize::MAX);
        file.seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut window[..count]))
            .map_err(|source| jio("read", path, source))?;
        if let Some(index) = window[..count].iter().rposition(|byte| *byte == b'\n') {
            return Ok(Some(start.saturating_add(index as u64).saturating_add(1)));
        }
        if start == 0 {
            return Ok(Some(0));
        }
        pos = start;
    }
}

/// Reads every line, decodes it, and validates the structure (I-3 to I-5).
fn scan_and_validate(
    journal: &mut Journal,
    path: &Path,
) -> Result<(Vec<(u64, Record)>, Option<EntryId>, Gen), OpenFailure> {
    let mut all = Vec::new();
    journal
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|source| OpenFailure::Journal(jio("read", path, source)))?;
    let mut buf = Vec::new();
    journal
        .file
        .read_to_end(&mut buf)
        .map_err(|source| OpenFailure::Journal(jio("read", path, source)))?;
    let mut offset = 0_u64;
    for line in buf.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if line_len.saturating_add(1) > MAX_RECORD {
            return Err(OpenFailure::Journal(JournalError::TooLong {
                path: path.to_path_buf(),
                offset,
            }));
        }
        let record = decode_line(line, offset)?;
        all.push((offset, record));
        offset = offset.saturating_add(line_len).saturating_add(1);
    }
    validate_structure(&all)?;
    let leaf = compute_leaf(&all);
    let gen = all
        .iter()
        .filter(|(_, record)| matches!(record, Record::Boot { .. }))
        .count() as u64;
    Ok((all, leaf, Gen::new(core::num::NonZeroU64::new(gen.max(1)).unwrap_or(core::num::NonZeroU64::MIN))))
}

fn decode_line(line: &[u8], offset: u64) -> Result<Record, OpenFailure> {
    match decode(line) {
        Ok(decoded) => Ok(decoded.record),
        Err(DecodeError::UnsupportedVersion { found }) => Err(OpenFailure::UnknownVersion(found)),
        Err(DecodeError::MissingVersion) => Err(OpenFailure::Damaged {
            offset,
            reason: "record has no format version".into(),
        }),
        Err(DecodeError::UnknownRecordKind { kind }) => Err(OpenFailure::Damaged {
            offset,
            reason: format!("unknown record type {kind:?}").into(),
        }),
        Err(DecodeError::Invalid { message, .. }) => Err(OpenFailure::Damaged {
            offset,
            reason: format!("invalid JSON: {message}").into(),
        }),
    }
}

fn validate_structure(records: &[(u64, Record)]) -> Result<(), OpenFailure> {
    let mut seen = Vec::<EntryId>::new();
    let mut open_turn: Option<TurnId> = None;
    for (offset, record) in records {
        match record {
            Record::Session(_) => {
                if !std::ptr::eq(record, &records[0].1) {
                    return Err(OpenFailure::Damaged {
                        offset: *offset,
                        reason: "a second session header".into(),
                    });
                }
            }
            Record::User(entry)
            | Record::Assistant(entry)
            | Record::ToolResult(entry)
            | Record::Reminder(entry)
            | Record::Model(entry)
            | Record::Thinking(entry)
            | Record::Approval(entry)
            | Record::Compaction(entry)
            | Record::BranchSummary(entry) => {
                if seen.contains(&entry.id) {
                    return Err(OpenFailure::Damaged {
                        offset: *offset,
                        reason: format!("duplicate entry id {}", entry.id),
                    });
                }
                if let Some(parent) = entry.parent {
                    if !seen.contains(&parent) {
                        return Err(OpenFailure::Damaged {
                            offset: *offset,
                            reason: format!(
                                "entry {} names parent {parent}, which is not an earlier entry",
                                entry.id
                            ),
                        });
                    }
                }
                seen.push(entry.id);
            }
            Record::Leaf { to: Some(to), .. } | Record::Label { entry: to, .. } => {
                if !seen.contains(to) {
                    return Err(OpenFailure::Damaged {
                        offset: *offset,
                        reason: format!("record refers to entry {to}, which does not exist"),
                    });
                }
            }
            Record::TurnStart { turn, .. } => {
                if let Some(open) = open_turn {
                    return Err(OpenFailure::Damaged {
                        offset: *offset,
                        reason: format!("turn {turn} starts while turn {open} is open"),
                    });
                }
                open_turn = Some(*turn);
            }
            Record::TurnEnd { turn, .. } => match open_turn {
                Some(open) if open == *turn => open_turn = None,
                _ => {
                    return Err(OpenFailure::Damaged {
                        offset: *offset,
                        reason: format!("turn {turn} ends but it is not open"),
                    });
                }
            },
            _ => {}
        }
    }
    Ok(())
}

fn compute_leaf(records: &[(u64, Record)]) -> Option<EntryId> {
    let mut leaf = None;
    for (_, record) in records {
        match record {
            Record::Leaf { to, .. } => leaf = *to,
            _ => {
                if let Some(entry) = record.entry() {
                    leaf = Some(entry.id);
                }
            }
        }
    }
    leaf
}

/// Builds the open-repair batch: synthetic tool results for dangling calls,
/// an aborted `turn_end`, and the boot record, in one fsync (D-15, D-22).
fn recovery_batch(
    records: &[(u64, Record)],
    gen: Gen,
) -> (Vec<Record>, Option<AbortedTurn>) {
    let mut repair = Vec::new();
    let aborted = aborted_turn(records, &mut repair);
    let at = jiff::Timestamp::now();
    repair.push(Record::Boot {
        at,
        r#gen: Gen::new(core::num::NonZeroU64::new(u64::from(gen.get()).saturating_add(1)).unwrap_or(core::num::NonZeroU64::MIN)),
        version: env!("CARGO_PKG_VERSION").into(),
    });
    (repair, aborted)
}

fn aborted_turn(records: &[(u64, Record)], repair: &mut Vec<Record>) -> Option<AbortedTurn> {
    let open_at = records.iter().rposition(|(_, r)| matches!(r, Record::TurnStart { .. }));
    let end_at = records.iter().rposition(|(_, r)| matches!(r, Record::TurnEnd { .. }));
    let (Some(open_at), None) = (open_at, end_at) else {
        return None;
    };
    if end_at.is_some() && end_at.unwrap() > open_at {
        return None;
    }
    let turn = match &records[open_at].1 {
        Record::TurnStart { turn, .. } => *turn,
        _ => return None,
    };
    let mut interrupted = 0_u64;
    let mut not_run = 0_u64;
    let at = jiff::Timestamp::now();
    let started: Vec<dal_core::CallId> = records[open_at..]
        .iter()
        .filter_map(|(_, r)| match r {
            Record::ToolStart { call, .. } => Some(call.clone()),
            _ => None,
        })
        .collect();
    for (_, record) in &records[open_at..] {
        let Some(entry) = record.entry() else { continue };
        let dal_core::EntryKind::Assistant { content, .. } = &entry.kind else {
            continue;
        };
        for block in content {
            let dal_core::Block::ToolCall { id, name, .. } = block else { continue };
            if has_result(&records[open_at..], id) {
                continue;
            }
            let ran = started.contains(id);
            if ran {
                interrupted = interrupted.saturating_add(1);
            } else {
                not_run = not_run.saturating_add(1);
            }
            repair.push(tool_result(id.clone(), name.clone(), ran, at));
        }
    }
    repair.push(Record::TurnEnd {
        at,
        turn,
        stop: TurnEndStop::Aborted,
        usage: None,
        changes: Vec::new(),
    });
    Some(AbortedTurn {
        turn,
        interrupted,
        not_run,
    })
}

fn has_result(records: &[(u64, Record)], call: &dal_core::CallId) -> bool {
    records.iter().any(|(_, record)| match record {
        Record::ToolResult(entry) => matches!(
            &entry.kind,
            dal_core::EntryKind::ToolResult { call: c, .. } if c == call
        ),
        _ => false,
    })
}

fn tool_result(
    call: dal_core::CallId,
    name: Box<str>,
    ran: bool,
    at: jiff::Timestamp,
) -> Record {
    let text: Box<str> = if ran {
        crate::error::INTERRUPTED_CALL.into()
    } else {
        crate::error::NOT_RUN_CALL.into()
    };
    Record::ToolResult(dal_core::Entry {
        id: EntryId::new(core::num::NonZeroU64::MIN),
        parent: None,
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

/// Maps a branch projection failure onto the store error vocabulary (D-31).
#[must_use]
pub fn branch_error(error: &BranchError) -> crate::error::StoreError {
    match error {
        BranchError::NoEntries => crate::error::StoreError::NothingToClone {
            id: String::new().into(),
        },
        BranchError::NotUserEntry { entry } => crate::error::StoreError::NotUserMessage { entry: *entry },
        BranchError::UnknownEntry { entry } => crate::error::StoreError::UnknownEntry {
            id: String::new().into(),
            entry: *entry,
        },
    }
}

pub(crate) fn session_id_of(header: &Header) -> SessionId {
    header.id
}
