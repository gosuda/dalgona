//! The Store-owned durable ledger of ended top-level reports and the held-job set.
//!
//! One append-only file of JSON lines under the session `jobs/` directory.
//! A report may be offered again after a restart; its stable job id lets
//! consumers reject duplicates. Only matching wake input, once durably
//! materialized, suppresses its id on replay; a mere wake attempt does not.
//!
//! [`Store::job_ledger`](crate::Store::job_ledger) opens the ledger without
//! creating any file or directory. [`JobLedger::append`] is the single
//! exclusive mutation: the whole read-validate-repair-write-sync runs inside
//! one owned blocking operation holding a per-handle serialization guard, so
//! a dropped append future can never overlap a later append.

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fs, io,
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use dal_core::{JobId, JobOutcome, JobReport};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::{private, util::FileMode};

/// The ledger file inside a session `jobs/` directory.
const LEDGER_FILE: &str = "reports.jsonl";

/// One JSON line of the ledger file. The serde shape is the ledger's durable
/// format: the tag, names, and field order here are the bytes on disk.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "e", rename_all = "snake_case")]
enum Line {
    End {
        id: JobId,
        label: Box<str>,
        outcome: JobOutcome,
        text: Box<str>,
    },
    Hold {
        id: JobId,
    },
    Unhold {
        id: JobId,
    },
}

/// Why a job ledger could not be opened or extended.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JobLedgerError {
    /// The file could not be read or written.
    #[error("job ledger {path}: {source}")]
    Io {
        /// The ledger file.
        path: PathBuf,
        /// The failing operation's error.
        #[source]
        source: io::Error,
    },
    /// A newline-terminated line is not a ledger line, or an End line
    /// contradicts the first End of the same job. An unterminated suffix
    /// without its newline may instead be a torn append and is repaired,
    /// never reported here.
    #[error("job ledger {path} is corrupt at line {line}")]
    Corrupt {
        /// The ledger file.
        path: PathBuf,
        /// The 1-based line number: the damaged or contradicting line on
        /// disk, or, for an append refused for contradicting an earlier End,
        /// that earlier End's line (where it would land, if still unwritten).
        line: usize,
    },
}

/// What a reopened ledger recovered.
#[derive(Debug, Default)]
pub struct JobLedgerRecovery {
    /// Every ended report, oldest first.
    pub ended: Vec<JobReport>,
    /// Ended reports no journaled wake delivered, oldest first.
    pub pending: Vec<JobReport>,
    /// The held-job set.
    pub held: HashSet<JobId>,
}

/// One opaque ledger line: an end record, a hold, or an unhold.
///
/// The inner record is never exposed; constructors build each shape. The
/// transparent encoding is identical to the ledger file's line format.
#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct JobLedgerEntry(Arc<Line>);

/// The append-only ledger file, owned by the [`Store`](crate::Store).
///
/// Not [`Clone`]: one handle owns its path and its exclusive append
/// serialization. Concurrent appends on one handle queue behind the same
/// guard instead of overlapping on the file.
#[derive(Debug)]
pub struct JobLedger {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    path: PathBuf,
    floor: PathBuf,
    gate: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    /// Whether the ledger file's directory entry has been synced by this
    /// handle. Set only after a write, its file sync, and its parent sync
    /// all succeed, so a retry after a failed sync owes the sync again
    /// instead of trusting that the entry survived.
    entry_durable: bool,
    /// Whether this handle has established its directory chain. Set only
    /// when the missing-tree helper or the existing-tree re-sync succeeds,
    /// never by mere existence, so a retry after a failed sync owes the
    /// sync again instead of trusting directories a failed call left
    /// behind.
    dirs_durable: bool,
}

impl JobLedger {
    /// Opens the ledger under `jobs_dir`, replaying what an earlier process wrote.
    ///
    /// Reads only: a missing file replays as empty and no file or directory
    /// is created. Only [`Store::job_ledger`](crate::Store::job_ledger)
    /// calls this; there is no public arbitrary-path constructor.
    pub(crate) async fn open(
        jobs_dir: PathBuf,
        floor: PathBuf,
        delivered: &HashSet<JobId>,
    ) -> Result<(Self, JobLedgerRecovery), JobLedgerError> {
        let path = jobs_dir.join(LEDGER_FILE);
        let bytes = read_all(&path).await?;
        let mut recovered = replay(&bytes).map_err(|line| JobLedgerError::Corrupt {
            path: path.clone(),
            line,
        })?;
        recovered
            .pending
            .retain(|report| !delivered.contains(&report.id));
        Ok((
            Self {
                shared: Arc::new(Shared {
                    path,
                    floor,
                    gate: Arc::new(Mutex::new(State::default())),
                }),
            },
            recovered,
        ))
    }

    /// Builds the end entry of one top-level report.
    #[must_use]
    pub fn end(report: &JobReport) -> JobLedgerEntry {
        JobLedgerEntry(Arc::new(Line::End {
            id: report.id,
            label: report.label.clone(),
            outcome: report.outcome.clone(),
            text: report.text.clone(),
        }))
    }

    /// Builds the hold entry of one job.
    #[must_use]
    pub fn hold(id: JobId) -> JobLedgerEntry {
        JobLedgerEntry(Arc::new(Line::Hold { id }))
    }

    /// Builds the unhold entry of one job.
    #[must_use]
    pub fn unhold(id: JobId) -> JobLedgerEntry {
        JobLedgerEntry(Arc::new(Line::Unhold { id }))
    }

    /// Appends `entries` and syncs them before returning.
    ///
    /// Whatever the batch holds, the file as it stands is replayed first,
    /// a torn final fragment excepted. A newline-terminated invalid line,
    /// a same-id End conflict inside the file, or an End that contradicts
    /// the first durable End of its job or an earlier End of this batch
    /// refuses the whole batch with `Corrupt` before any byte is repaired
    /// or appended. A write is acknowledged only for a ledger `open`
    /// accepts.
    ///
    /// An End identical to one already durable, or earlier in the batch, is
    /// skipped, so a retried batch never grows duplicate physical records;
    /// a batch left with nothing to append is still written and synced, so
    /// an acknowledgment always means the file up to this batch is synced.
    /// Hold and Unhold entries append in order.
    ///
    /// Only an accepted batch repairs a torn final fragment, whether or not
    /// it appends anything: a complete record without its newline is
    /// terminated, otherwise the partial bytes are truncated. Both repairs
    /// sync before the new batch appends, so a crash between repair and
    /// append stays recoverable and a retried batch never fuses with the
    /// tear.
    ///
    /// The whole read-validate-repair-write-sync runs inside one owned
    /// blocking operation holding this handle's serialization guard. A
    /// future dropped while queued starts no I/O; an admitted operation
    /// completes under its guard before any later append is admitted.
    ///
    /// # Errors
    /// Returns [`JobLedgerError::Io`] when the file or its directories
    /// cannot be read, created, written, or synced, and
    /// [`JobLedgerError::Corrupt`] when the file or the batch contradicts
    /// the ledger's replay rules.
    pub async fn append(&mut self, entries: &[JobLedgerEntry]) -> Result<(), JobLedgerError> {
        if entries.is_empty() {
            return Ok(());
        }
        let batch: Vec<Arc<Line>> = entries.iter().map(|entry| Arc::clone(&entry.0)).collect();
        let guard = Arc::clone(&self.shared.gate).lock_owned().await;
        let shared = Arc::clone(&self.shared);
        let error_path = shared.path.clone();
        tokio::task::spawn_blocking(move || append_blocking(&shared, guard, &batch))
            .await
            .map_err(|join| JobLedgerError::Io {
                path: error_path,
                source: io::Error::other(join),
            })?
    }
}

/// Reads the whole ledger file; a missing file reads as empty.
async fn read_all(path: &Path) -> Result<Vec<u8>, JobLedgerError> {
    let path = path.to_path_buf();
    let owned = path.clone();
    tokio::task::spawn_blocking(move || match fs::read(&owned) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(JobLedgerError::Io {
            path: owned.clone(),
            source,
        }),
    })
    .await
    .map_err(|join| JobLedgerError::Io {
        path: path.clone(),
        source: io::Error::other(join),
    })?
}

fn io_error(path: &Path, source: io::Error) -> JobLedgerError {
    JobLedgerError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn store_failure(path: &Path, error: crate::StoreError) -> JobLedgerError {
    match error {
        crate::StoreError::Io { path, source } => JobLedgerError::Io {
            path,
            source: *source,
        },
        other => JobLedgerError::Io {
            path: path.to_path_buf(),
            source: io::Error::other(other.to_string()),
        },
    }
}

/// Opens a ledger file that the read saw, for append.
///
/// A file that vanished since the read is recreated through the exclusive
/// private create, never through default creation modes; a file recreated
/// concurrently in between is opened as found.
fn open_after_read(path: &Path) -> Result<(fs::File, bool), JobLedgerError> {
    let open = || fs::OpenOptions::new().append(true).open(path);
    match open() {
        Ok(file) => Ok((file, false)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match private::create_new(path, FileMode::Mode0600) {
                Ok(file) => Ok((file, true)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open()
                    .map(|file| (file, false))
                    .map_err(|source| io_error(path, source)),
                Err(source) => Err(io_error(path, source)),
            }
        }
        Err(source) => Err(io_error(path, source)),
    }
}

/// Re-syncs the directory chain from `jobs_dir` up through the floor's
/// parent, without creating or inspecting anything.
///
/// This is the durable helper's sync half without its create and permission
/// half: pure directory syncs never rewrite legacy permissions, so an
/// existing tree is re-synced after a failed establishment without a
/// migration side effect. Mirrors the helper's floor bounds: nothing above
/// the floor's parent is touched.
fn resync_chain(floor: &Path, jobs_dir: &Path, path: &Path) -> Result<(), JobLedgerError> {
    if jobs_dir != floor && !jobs_dir.starts_with(floor) {
        return Err(store_failure(
            path,
            crate::StoreError::Invalid {
                reason: format!(
                    "durable directory {} is not at or below its floor {}",
                    jobs_dir.display(),
                    floor.display()
                )
                .into(),
            },
        ));
    }
    let floor_parent = floor
        .parent()
        .map(|parent| {
            if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }
        })
        .ok_or_else(|| {
            store_failure(
                path,
                crate::StoreError::Invalid {
                    reason: format!(
                        "durable directory floor {} has no parent to sync",
                        floor.display()
                    )
                    .into(),
                },
            )
        })?;
    let mut cursor = jobs_dir;
    loop {
        crate::util::sync_dir(cursor).map_err(|error| store_failure(path, error))?;
        if cursor == floor_parent {
            return Ok(());
        }
        cursor = cursor
            .parent()
            .map(|parent| {
                if parent.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    parent
                }
            })
            .ok_or_else(|| {
                store_failure(
                    path,
                    crate::StoreError::Invalid {
                        reason: format!(
                            "durable directory {} escaped its floor {}",
                            jobs_dir.display(),
                            floor.display()
                        )
                        .into(),
                    },
                )
            })?;
    }
}

/// The single exclusive ledger mutation. Holds `guard` for its whole run so
/// no later append on this handle can overlap it, even if the awaiting
/// future was dropped and re-issued.
fn append_blocking(
    shared: &Shared,
    mut guard: OwnedMutexGuard<State>,
    batch: &[Arc<Line>],
) -> Result<(), JobLedgerError> {
    let path = &shared.path;
    let existing = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(source) => return Err(io_error(path, source)),
    };
    let validate = |bytes: &[u8]| {
        let keep = unwritten(bytes, batch).map_err(|line| JobLedgerError::Corrupt {
            path: path.clone(),
            line,
        })?;
        let mut text = String::new();
        for line in &keep {
            text.push_str(
                &sonic_rs::to_string(&**line)
                    .map_err(|error| io_error(path, io::Error::other(error.to_string())))?,
            );
            text.push('\n');
        }
        Ok::<String, JobLedgerError>(text)
    };
    let mut text = validate(existing.as_deref().unwrap_or(&[]))?;
    let Some(jobs_dir) = path.parent() else {
        return Err(io_error(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "ledger path has no parent"),
        ));
    };
    if !guard.dirs_durable {
        if fs::symlink_metadata(jobs_dir).is_err() {
            crate::util::create_private_dir_all_durable(&shared.floor, jobs_dir)
                .map_err(|error| store_failure(path, error))?;
        } else {
            resync_chain(&shared.floor, jobs_dir, path)?;
        }
        guard.dirs_durable = true;
    }
    let (file, created_new) = match existing {
        Some(bytes) => {
            repair_torn_tail(path, &bytes)?;
            open_after_read(path)?
        }
        None => match private::create_new(path, FileMode::Mode0600) {
            Ok(file) => (file, true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let current = fs::read(path).map_err(|source| io_error(path, source))?;
                text = validate(&current)?;
                repair_torn_tail(path, &current)?;
                open_after_read(path)?
            }
            Err(source) => return Err(io_error(path, source)),
        },
    };
    (&file)
        .write_all(text.as_bytes())
        .map_err(|source| io_error(path, source))?;
    file.sync_data().map_err(|source| io_error(path, source))?;
    drop(file);
    if !guard.entry_durable || created_new {
        crate::util::sync_dir(jobs_dir).map_err(|error| store_failure(path, error))?;
    }
    guard.entry_durable = true;
    Ok(())
}

/// Removes a torn final fragment or terminates a complete final record so
/// the next append starts on its own line.
///
/// `bytes` is the file content read at this operation's start. Only a
/// missing trailing `\n` can hide a tear: every terminated line was synced
/// with its newline, so a file ending in `\n` needs no repair. A missing
/// newline either terminates a complete record (append the newline) or
/// leaves a torn fragment, including a tail cut inside a UTF-8 sequence
/// (truncate it). Both repairs sync before returning so a crash between
/// repair and the next append stays recoverable. A file that vanished since
/// the read needs no repair: the append recreates it.
fn repair_torn_tail(path: &Path, bytes: &[u8]) -> Result<(), JobLedgerError> {
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        return Ok(());
    }
    let tail_start = bytes
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    let complete = std::str::from_utf8(&bytes[tail_start..])
        .ok()
        .and_then(|tail| sonic_rs::from_str::<Line>(tail).ok())
        .is_some();
    if complete {
        let file = match fs::OpenOptions::new().append(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(path, source)),
        };
        (&file)
            .write_all(b"\n")
            .map_err(|source| io_error(path, source))?;
        file.sync_data().map_err(|source| io_error(path, source))?;
    } else {
        let file = match fs::OpenOptions::new().write(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(path, source)),
        };
        let tail_start = u64::try_from(tail_start)
            .map_err(io::Error::other)
            .map_err(|source| io_error(path, source))?;
        file.set_len(tail_start)
            .map_err(|source| io_error(path, source))?;
        file.sync_data().map_err(|source| io_error(path, source))?;
    }
    Ok(())
}

/// Hands every record of `bytes` to `visit` in file order with its 1-based
/// line number.
///
/// Only a missing final `\n` may hide a tear: every `\n`-terminated line must
/// be UTF-8 that parses as a [`Line`], else its number is the error. The
/// unterminated final fragment is a record only when it is valid UTF-8 and
/// parses as a [`Line`]; otherwise it is torn and skipped. `visit` may refuse
/// a record with the line number to report.
fn scan(
    bytes: &[u8],
    mut visit: impl FnMut(Line, usize) -> Result<(), usize>,
) -> Result<(), usize> {
    for (index, chunk) in bytes.split_inclusive(|&byte| byte == b'\n').enumerate() {
        let line_no = index + 1;
        if let Some(raw) = chunk.strip_suffix(b"\n") {
            let text = std::str::from_utf8(raw).map_err(|_| line_no)?;
            let parsed: Line = sonic_rs::from_str(text).map_err(|_| line_no)?;
            visit(parsed, line_no)?;
        } else if let Some(parsed) = std::str::from_utf8(chunk)
            .ok()
            .and_then(|text| sonic_rs::from_str::<Line>(text).ok())
        {
            visit(parsed, line_no)?;
        }
    }
    Ok(())
}

fn replay(bytes: &[u8]) -> Result<JobLedgerRecovery, usize> {
    let mut recovered = JobLedgerRecovery::default();
    let mut seen: HashSet<JobId> = HashSet::new();
    scan(bytes, |line, line_no| {
        push_line(&mut recovered, &mut seen, line, line_no)
    })?;
    Ok(recovered)
}

/// Picks the entries of `batch` that still have to be appended to the ledger
/// holding `bytes`, or names the line that makes the append refusable.
///
/// The file is checked exactly as `open` checks it, so an acknowledged
/// append never lands on a ledger `open` rejects, whether `batch` holds Ends
/// or only Hold and Unhold entries. An End is dropped when it equals the
/// first durable End of its job or an earlier End of this batch; one that
/// differs from either refuses the whole batch, before the first fresh line
/// is written. The caller's bytes are neither modified nor repaired here.
fn unwritten(bytes: &[u8], batch: &[Arc<Line>]) -> Result<Vec<Arc<Line>>, usize> {
    let mut durable: HashMap<JobId, (usize, Line)> = HashMap::new();
    let mut count = 0;
    scan(bytes, |line, line_no| {
        count = line_no;
        let Line::End { id, .. } = &line else {
            return Ok(());
        };
        match durable.entry(*id) {
            Entry::Occupied(first) if first.get().1 != line => Err(line_no),
            Entry::Occupied(_) => Ok(()),
            Entry::Vacant(slot) => {
                slot.insert((line_no, line));
                Ok(())
            }
        }
    })?;
    let mut next = count + 1;
    let mut seen: HashMap<JobId, (usize, &Line)> = HashMap::new();
    let mut keep: Vec<Arc<Line>> = Vec::with_capacity(batch.len());
    for entry in batch {
        let line: &Line = entry;
        if let Line::End { id, .. } = line {
            if let Some((first_line, first)) = durable.get(id) {
                if first != line {
                    return Err(*first_line);
                }
                continue;
            }
            match seen.entry(*id) {
                Entry::Occupied(earlier) => {
                    let &(first_line, first) = earlier.get();
                    if first != line {
                        return Err(first_line);
                    }
                    continue;
                }
                Entry::Vacant(slot) => {
                    slot.insert((next, line));
                }
            }
        }
        keep.push(Arc::clone(entry));
        next += 1;
    }
    Ok(keep)
}

/// Records one parsed line, keeping the first terminal report per id.
///
/// A file may hold the same End line twice, as a retried flush that appended
/// its whole batch again leaves. The first record wins in file order and an
/// identical repeat is skipped. A duplicate id with a different report is
/// corruption rather than a silently hidden state.
fn push_line(
    recovered: &mut JobLedgerRecovery,
    seen: &mut HashSet<JobId>,
    line: Line,
    line_no: usize,
) -> Result<(), usize> {
    match line {
        Line::End {
            id,
            label,
            outcome,
            text,
        } => {
            let incoming = JobReport {
                id,
                label,
                outcome,
                text,
            };
            if seen.insert(id) {
                recovered.pending.push(incoming.clone());
                recovered.ended.push(incoming);
            } else {
                let first = recovered
                    .ended
                    .iter()
                    .find(|report| report.id == id)
                    .ok_or(line_no)?;
                if first.label != incoming.label
                    || first.outcome != incoming.outcome
                    || first.text != incoming.text
                {
                    return Err(line_no);
                }
            }
            Ok(())
        }
        Line::Hold { id } => {
            recovered.held.insert(id);
            Ok(())
        }
        Line::Unhold { id } => {
            recovered.held.remove(&id);
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "job_ledger/tests.rs"]
mod tests;
