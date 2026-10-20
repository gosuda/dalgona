//! The durable ledger of ended top-level reports and the held-job set.
//!
//! One append-only file of JSON lines under the session `jobs/` directory. A
//! report is offered again after a restart until the journal's wake record
//! names it, so every ended top-level job is delivered exactly once.

use std::collections::HashSet;
use std::io;
use std::{path::PathBuf, sync::Arc};

use dal_core::{JobId, JobOutcome, JobReport};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

const LEDGER_FILE: &str = "reports.jsonl";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "e", rename_all = "snake_case")]
pub(super) enum Line {
    End {
        id: JobId,
        label: Box<str>,
        /// The command the starter supplied; absent on lines written before
        /// the field existed and for jobs started without a command.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Arc<str>>,
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

/// Why a ledger could not be opened or extended.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LedgerError {
    /// The file could not be read or written.
    #[error("job ledger {path}: {source}")]
    Io {
        /// The ledger file.
        path: PathBuf,
        /// The failing operation's error.
        #[source]
        source: io::Error,
    },
    /// A line before the last one is not a ledger line.
    #[error("job ledger {path} is corrupt at line {line}")]
    Corrupt {
        /// The ledger file.
        path: PathBuf,
        /// The 1-based line number.
        line: usize,
    },
}

/// One replayed end line: the public report and the private command text.
#[derive(Debug)]
pub(super) struct Ended {
    /// The report every consumer sees.
    pub(super) report: JobReport,
    /// The command the starter supplied; the report type carries none.
    pub(super) command: Option<Arc<str>>,
}

/// What a reopened ledger recovered.
#[derive(Debug, Default)]
pub(super) struct Recovered {
    /// Every ended report, oldest first.
    pub(super) ended: Vec<Ended>,
    /// Ended reports no journaled wake delivered, oldest first.
    pub(super) pending: Vec<JobReport>,
    /// The held-job set.
    pub(super) held: HashSet<JobId>,
}

/// The append-only ledger file; no path means an ephemeral session.
#[derive(Debug, Default)]
pub(super) struct Ledger {
    path: Option<PathBuf>,
}

impl Ledger {
    /// Opens the ledger under `dir`, replaying what an earlier process wrote.
    pub(super) async fn open(
        dir: PathBuf,
        delivered: &HashSet<JobId>,
    ) -> Result<(Self, Recovered), LedgerError> {
        let path = dir.join(LEDGER_FILE);
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(LedgerError::Io { path, source }),
        };
        let mut recovered = replay(&text).map_err(|line| LedgerError::Corrupt {
            path: path.clone(),
            line,
        })?;
        recovered
            .pending
            .retain(|report| !delivered.contains(&report.id));
        Ok((Self { path: Some(path) }, recovered))
    }

    /// Builds the end line of one top-level report and its starter command.
    pub(super) fn end(report: &JobReport, command: Option<Arc<str>>) -> Line {
        Line::End {
            id: report.id,
            label: report.label.clone(),
            command,
            outcome: report.outcome.clone(),
            text: report.text.clone(),
        }
    }

    /// Builds the hold or unhold line of one job.
    pub(super) fn hold(id: JobId) -> Line {
        Line::Hold { id }
    }

    /// Builds the unhold line of one job.
    pub(super) fn unhold(id: JobId) -> Line {
        Line::Unhold { id }
    }

    /// Appends `lines` and syncs them before returning.
    pub(super) async fn write(&self, lines: &[Line]) -> Result<(), LedgerError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if lines.is_empty() {
            return Ok(());
        }
        let io_error = |source| LedgerError::Io {
            path: path.clone(),
            source,
        };
        let mut text = String::new();
        for line in lines {
            text.push_str(
                &sonic_rs::to_string(line)
                    .map_err(|error| io_error(io::Error::other(error.to_string())))?,
            );
            text.push('\n');
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(io_error)?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .map_err(io_error)?;
        file.write_all(text.as_bytes()).await.map_err(io_error)?;
        file.sync_data().await.map_err(io_error)
    }
}

fn replay(text: &str) -> Result<Recovered, usize> {
    let mut recovered = Recovered::default();
    let total = text.lines().count();
    for (index, raw) in text.lines().enumerate() {
        let Ok(line) = sonic_rs::from_str::<Line>(raw) else {
            if index + 1 == total && !text.ends_with('\n') {
                break;
            }
            return Err(index + 1);
        };
        match line {
            Line::End {
                id,
                label,
                command,
                outcome,
                text,
            } => {
                let report = JobReport {
                    id,
                    label,
                    outcome,
                    text,
                };
                recovered.pending.push(report.clone());
                recovered.ended.push(Ended { report, command });
            }
            Line::Hold { id } => {
                recovered.held.insert(id);
            }
            Line::Unhold { id } => {
                recovered.held.remove(&id);
            }
        }
    }
    Ok(recovered)
}
