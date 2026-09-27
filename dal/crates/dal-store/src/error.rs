//! Typed failures for sessions, journals, and blobs.
//!
//! Each variant's display text is the product text. Callers match the
//! variant; front ends print [`std::error::Error::to_string`].

use std::path::PathBuf;

use dal_core::EntryId;

/// A session operation failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum StoreError {
    /// Another process holds the session lock.
    #[error("{}", locked_text(.session, .pid))]
    Locked {
        /// The session that is open elsewhere.
        session: Box<str>,
        /// The pid text from the lock file, when it parses.
        pid: Option<u32>,
    },
    /// The journal names a format this build does not read.
    #[error(
        "session file {path} uses journal format {found}; this dalgon reads format 1. Update dal to open it."
    )]
    UnknownVersion {
        /// The journal path.
        path: PathBuf,
        /// The version found.
    found: u64,
    },
    /// No journal exists at the path.
    #[error("no session file at {}", .path.display())]
    NotFound {
        /// The missing journal path.
        path: PathBuf,
    },
    /// The journal failed structure checks. The file was not changed.
    #[error(
        "session file {path} is damaged at byte {offset}: {reason}. dal did not change it. Move the file aside, or truncate it at byte {offset} to keep the earlier records."
    )]
    Damaged {
        /// The journal path.
        path: PathBuf,
        /// The byte offset of the bad record.
        offset: u64,
        /// The structure or decode reason.
        reason: Box<str>,
    },
    /// A batch write failed and the partial bytes were removed.
    #[error(
        "could not write session {id}: {cause}. dal removed the partial record. Remove the cause, then resume the session."
    )]
    WriteFailed {
        /// The session identifier.
        id: Box<str>,
        /// The write failure.
        cause: Box<str>,
    },
    /// A later mutation after a failed rollback.
    #[error(
        "session {id} accepts no more writes after an earlier write error. Resume the session to continue."
    )]
    Broken {
        /// The session identifier.
        id: Box<str>,
    },
    /// The caller asked for a record the store will not write.
    #[error("{reason}")]
    Invalid {
        /// The reason text.
        reason: Box<str>,
    },
    /// A filesystem call failed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// The path of the failed call.
        path: PathBuf,
        /// The operating-system error.
        source: Box<str>,
    },
    /// `--resume` was given an empty argument.
    #[error("--resume needs a session id or name")]
    EmptyRef,
    /// No session in the workspace matches the argument.
    #[error("no session in workspace {workspace} matches \"{arg}\"")]
    NoMatch {
        /// The argument that matched nothing.
        arg: Box<str>,
        /// The workspace key.
        workspace: Box<str>,
    },
    /// More than one session matches the argument.
    #[error(
        "\"{arg}\" matches {count} sessions: {listed}. Use more characters of the id, or the full id."
    )]
    Ambiguous {
        /// The argument that matched more than one session.
        arg: Box<str>,
        /// The number of matches.
        count: usize,
        /// At most five matches, already formatted.
        listed: Box<str>,
    },
    /// The name fails the session-name rule.
    #[error(
        "a session name must have 1 to 64 characters, no control characters, and at least one character other than 0-9, a-f, and -"
    )]
    InvalidName,
    /// Another session in the workspace already has this name.
    #[error("the name \"{name}\" is already used by session {id} in this workspace")]
    NameTaken {
        /// The contested name.
        name: Box<str>,
        /// The session that holds it.
        id: Box<str>,
    },
    /// The entry is not in the session.
    #[error("session {id} has no entry {entry}")]
    UnknownEntry {
        /// The session identifier.
        id: Box<str>,
        /// The missing entry.
        entry: EntryId,
    },
    /// `/fork` was aimed at an entry that is not a user message.
    #[error("entry {entry} is not a user message; /fork starts from a user message")]
    NotUserMessage {
        /// The offending entry.
        entry: EntryId,
    },
    /// Clone was asked of a session with no entries.
    #[error("session {id} has no entries to clone")]
    NothingToClone {
        /// The session identifier.
        id: Box<str>,
    },
    /// `list` was given a limit outside 1..=500.
    #[error("Store.list: limit must be 1 to 500")]
    ListLimit,
    /// `list` was given a cursor that does not parse.
    #[error("Store.list: malformed cursor")]
    MalformedCursor,
    /// The journal layer failed. The display text is the journal text.
    #[error(transparent)]
    Journal(#[from] JournalError),
    /// The blob layer failed. The display text is the blob text.
    #[error(transparent)]
    Blob(#[from] BlobError),
}

fn locked_text(session: &str, pid: &Option<u32>) -> String {
    match pid {
        Some(pid) => format!("session {session} is open in process {pid}"),
        None => format!("session {session} is open in another process"),
    }
}

/// A journal file operation failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum JournalError {
    /// A named file operation failed.
    #[error("{op} {}: {source}", .path.display())]
    Io {
        /// One of `create`, `open`, `write`, `sync`, `truncate`, `read`, `quarantine`.
        op: &'static str,
        /// The path of the failed call.
        path: PathBuf,
        /// The operating-system or injected error.
        source: Box<str>,
    },
    /// A line exceeds the record byte limit.
    #[error("{path}: the record at byte {offset} is longer than 67108864 bytes")]
    TooLong {
        /// The journal path.
        path: PathBuf,
        /// The byte offset where the over-long record starts.
        offset: u64,
    },
    /// Rollback could not remove a partial batch. Later appends fail the same way.
    #[error(
        "{path}: an earlier write failed and dalgon could not remove the partial record ({cause}). Reopen the session to repair it."
    )]
    Damaged {
        /// The journal path.
        path: PathBuf,
        /// The rollback failure.
        cause: Box<str>,
    },
}

/// A blob read or write failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum BlobError {
    /// The digest file is absent and the session directory is present.
    #[error("blob {hex} is not in this session")]
    NotFound {
        /// The 64-hex digest.
        hex: Box<str>,
    },
    /// The session directory is absent.
    #[error("the session was deleted, so its blobs are gone")]
    Gone,
    /// The value exceeds the blob cap.
    #[error("blob of {bytes} bytes is larger than the limit of 67108864 bytes")]
    TooLarge {
        /// The rejected length.
        bytes: u64,
    },
    /// A filesystem call in the blob store failed.
    #[error("blob store: {source}")]
    Io {
        /// The operating-system error.
        source: Box<str>,
    },
}

/// Facts the store returns when open repairs a file. The agent prints them.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenReport {
    /// A torn tail that was moved aside, when one was present.
    pub torn: Option<TornTail>,
    /// An open turn that recovery closed, when one was present.
    pub aborted: Option<AbortedTurn>,
    /// The boot generation written by this open.
    pub gen: u64,
}

/// A torn tail quarantined at open.
#[derive(Clone, Debug, PartialEq)]
pub struct TornTail {
    /// The byte offset where the unfinished record started.
    pub offset: u64,
    /// The number of quarantined bytes.
    pub bytes: u64,
    /// The side file that holds those bytes.
    pub kept_at: PathBuf,
}

impl TornTail {
    /// The notice text for this quarantine.
    #[must_use]
    pub fn notice(&self) -> String {
        format!(
            "dal found {} bytes of an unfinished record at the end of the session file and moved them to {}.",
            self.bytes,
            self.kept_at.display()
        )
    }
}

/// An open turn closed because the previous process stopped.
#[derive(Clone, Debug, PartialEq)]
pub struct AbortedTurn {
    /// The turn that was open.
    pub turn: dal_core::TurnId,
    /// Unfinished calls that had a `tool_start`.
    pub interrupted: u64,
    /// Unfinished calls that had no `tool_start`.
    pub not_run: u64,
}

impl AbortedTurn {
    /// The notice text for this recovery.
    #[must_use]
    pub fn notice(&self) -> String {
        let marked = self.interrupted.saturating_add(self.not_run);
        if marked == 0 {
            format!("Turn {} did not finish because dalgon stopped.", self.turn)
        } else {
            format!(
                "Turn {} did not finish because dalgon stopped. dal marked {marked} unfinished tool calls.",
                self.turn
            )
        }
    }
}

/// The notice printed when `-c` finds no earlier session.
pub const NO_EARLIER_SESSION: &str =
    "No earlier session in this workspace. dal started a new session.";

/// Text of a tool result written for a call that had started.
pub const INTERRUPTED_CALL: &str = "dalgon stopped while this tool call ran. The outcome is unknown. Inspect the workspace before you run it again.";

/// Text of a tool result written for a call that had not started.
pub const NOT_RUN_CALL: &str =
    "dalgon stopped before this tool call started. It did not run.";

/// Projection-only text for a tool call with no result on the visible branch.
pub const MISSING_ON_BRANCH: &str = "This tool call has no result on this branch.";
