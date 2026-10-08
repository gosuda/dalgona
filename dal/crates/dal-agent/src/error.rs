//! Typed failures of the host, the agent operations, services, tools, and schemes.
//!
//! Each variant carries only owned data, and its display text is the product
//! text. Callers match the variant; front ends print
//! [`std::error::Error::to_string`]. Typed failures from lower layers stay
//! typed: store failures keep their [`StoreError`] and its source chain.

use std::path::PathBuf;
use std::{fmt, io};

pub use dal_core::DenyReason;
use dal_core::{
    Answer, BlobId, ClientId, ErrorTriple, Expect, JobId, Question, RequestId, Service, SessionId,
    TurnId,
};
use dal_store::{BlobError, StoreError};

/// The number of wake-started turns in a row the core accepts before it
/// refuses the next wake with [`DenyReason::WakeLimit`].
const WAKE_LIMIT: u32 = 20;

/// The most bytes one sidecar value may hold.
const SIDECAR_VALUE_LIMIT: u64 = 1_048_576;

/// The bounded resource an admission wait was waiting for.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionLimit {
    /// Concurrent provider streams (`limits.streams`).
    Streams,
    /// Concurrent child processes (`limits.processes`).
    Processes,
    /// The file-descriptor budget.
    Fds,
}

impl AdmissionLimit {
    /// Returns the limit's name as it appears in config and error text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Streams => "streams",
            Self::Processes => "processes",
            Self::Fds => "fds",
        }
    }
}

impl fmt::Display for AdmissionLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The turn state a turn-scoped command required.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpectedTurn {
    /// The command required an idle session.
    NoRunningTurn,
    /// The command named this turn.
    Turn(TurnId),
}

impl From<Expect> for ExpectedTurn {
    fn from(expect: Expect) -> Self {
        match expect {
            Expect::Idle => Self::NoRunningTurn,
            Expect::After(turn) => Self::Turn(turn),
        }
    }
}

impl fmt::Display for ExpectedTurn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRunningTurn => formatter.write_str("no running turn"),
            Self::Turn(turn) => write!(formatter, "turn {turn}"),
        }
    }
}

/// The phase of the turn a session actually held.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnPhase {
    /// The turn is generating or dispatching.
    Running,
    /// The turn stopped generating and is settling.
    Settling,
    /// The turn is compacting its context.
    Compacting,
    /// The turn has ended.
    Ended,
}

impl fmt::Display for TurnPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Running => "running",
            Self::Settling => "settling",
            Self::Compacting => "compacting",
            Self::Ended => "ended",
        })
    }
}

/// The turn state a session actually held when a command did not match.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActualTurn {
    /// No turn exists yet and the session is idle.
    Idle,
    /// The latest turn and its phase.
    Turn {
        /// The latest turn.
        turn: TurnId,
        /// Its phase.
        phase: TurnPhase,
    },
}

impl fmt::Display for ActualTurn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => formatter.write_str("no turn (idle)"),
            Self::Turn { turn, phase } => write!(formatter, "turn {turn} ({phase})"),
        }
    }
}

/// A command or answer failed a validation or availability check.
///
/// The message is the complete product text.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ValidationError {
    message: Box<str>,
}

impl ValidationError {
    /// Creates a validation error whose display text is `message`.
    #[must_use]
    pub fn new(message: impl Into<Box<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Borrows the product text.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// A prompt arrived while a compaction job ran (`rejection.compacting`).
    #[must_use]
    pub fn compacting() -> Self {
        Self::new(
            "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.",
        )
    }

    /// An idle-only command arrived during a turn (`rejection.busy_turn`).
    #[must_use]
    pub fn busy_turn(command: &str, turn: TurnId) -> Self {
        Self::new(format!(
            "{command} needs an idle session; turn {turn} is running. Cancel it or wait for it to end."
        ))
    }

    /// A job id names no live job (`rejection.job_unknown`).
    #[must_use]
    pub fn job_not_running(job: JobId) -> Self {
        Self::new(format!("job {job} is not running."))
    }

    /// An answer kind does not fit the open question (`rejection.bad_answer`).
    #[must_use]
    pub fn bad_answer(answer: &Answer, question: &Question) -> Self {
        let answer = match answer {
            Answer::Approve => "approve",
            Answer::ApproveForSession => "approve_for_session",
            Answer::Decline => "decline",
            Answer::Cancel => "cancel",
            Answer::Value(_) => "value",
            _ => "unknown",
        };
        let question = match question {
            Question::Approval { .. } => "approval",
            Question::Grant { .. } => "grant",
            Question::Select { .. } => "select",
            Question::Confirm { .. } => "confirm",
            Question::Text { .. } => "text",
            _ => "unknown",
        };
        Self::new(format!(
            "answer {answer} does not fit a {question} request."
        ))
    }
}

/// A host operation failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// Another process holds the session's one-writer lock (`session.busy`).
    #[error("{}", busy_text(.id, *.pid))]
    SessionBusy {
        /// The session that is open elsewhere.
        id: SessionId,
        /// The pid recorded by the lock holder, when it parses.
        pid: Option<u32>,
    },
    /// A reference, route, or document names nothing.
    #[error("{message}")]
    NotFound {
        /// The product text.
        message: Box<str>,
    },
    /// Configuration forbids the operation.
    #[error("{message}")]
    Config {
        /// The product text.
        message: Box<str>,
    },
    /// A filesystem call made by the host failed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// The path of the failed call.
        path: PathBuf,
        /// The operating-system error.
        source: io::Error,
    },
    /// An admission wait expired.
    #[error("{}", admission_text(*.limit))]
    Admission {
        /// The exhausted resource.
        limit: AdmissionLimit,
    },
    /// The host has shut down.
    #[error("the host is shut down")]
    Closed,
    /// The store failed. The display text is the store text.
    #[error(transparent)]
    Store(StoreError),
}

impl HostError {
    /// A child session asked to start a child past `agents.max_depth` (`agents.depth`).
    #[must_use]
    pub fn max_depth(max_depth: u32) -> Self {
        Self::Config {
            message: format!(
                "child sessions cannot start children here: agents.max_depth = {max_depth}."
            )
            .into(),
        }
    }
}

impl From<StoreError> for HostError {
    /// Maps a lock held by another process to [`HostError::SessionBusy`] and
    /// keeps every other store failure typed.
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Locked { session, pid, .. } => Self::SessionBusy { id: session, pid },
            error => Self::Store(error),
        }
    }
}

fn busy_text(id: &SessionId, pid: Option<u32>) -> String {
    match pid {
        Some(pid) => format!("session {id} is open in process {pid}"),
        None => format!("session {id} is open in another process"),
    }
}

fn admission_text(limit: AdmissionLimit) -> String {
    format!("admission wait expired: no free {limit} slot within limits.admission_wait.")
}

/// An `Agent` operation was rejected without changing session state.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AgentError {
    /// A turn-scoped command named a state the session is not in (`turn.mismatch`).
    #[error("turn mismatch: expected {expected}, found {actual}. Nothing changed.")]
    WrongTurn {
        /// The state the command required.
        expected: ExpectedTurn,
        /// The state the session held.
        actual: ActualTurn,
    },
    /// A request was answered after another client resolved it (`request.resolved`).
    #[error("request {id} was already resolved by {}.", .by.as_str())]
    AlreadyResolved {
        /// The resolved request.
        id: RequestId,
        /// The winning client, `core` for deadlines and cancellation.
        by: ClientId,
    },
    /// The steer queue of the running turn is full (`steer.full`).
    #[error("steer queue is full (16) for turn {turn}; wait for the next step or cancel.")]
    SteerFull {
        /// The running turn.
        turn: TurnId,
    },
    /// The command or answer failed validation.
    #[error(transparent)]
    Invalid(#[from] ValidationError),
    /// The session no longer accepts operations (`session.closed`).
    #[error("session {id} is closed.")]
    SessionClosed {
        /// The closed session.
        id: SessionId,
    },
    /// A blob digest is absent from the session (`blob.not_found`).
    #[error("blob {id} was not found in session {session}.")]
    BlobNotFound {
        /// The missing digest.
        id: BlobId,
        /// The session that was read.
        session: SessionId,
    },
    /// The session directory is gone, so its blobs are unreadable (`session.gone`).
    #[error("session {session} was deleted.")]
    SessionGone {
        /// The deleted session.
        session: SessionId,
    },
}

/// A host service call failed.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServiceError {
    /// A capability, availability, or scope check refused the call.
    #[error("{}", DenyText(.0))]
    Denied(DenyReason),
    /// The user declined the request.
    #[error("the user declined the request")]
    Declined,
    /// The request was cancelled.
    #[error("the request was cancelled")]
    Cancelled,
    /// The service ran and failed. The message is the complete product text.
    #[error("{message}")]
    Failed {
        /// The script service that failed; `None` for Rust-only operations.
        service: Option<Service>,
        /// The product text.
        message: Box<str>,
    },
    /// A command handler failed with a typed triple; the dispatcher renders
    /// it unchanged as the `error { what, why, fix }` reply.
    #[error("{}: {} ({})", .0.what, .0.why, .0.fix)]
    Command(ErrorTriple),
    /// A session tool names a tool that is already registered.
    #[error("tool \"{name}\" is already registered by \"{held_by}\"")]
    ToolNameInUse {
        /// The rejected tool name.
        name: Box<str>,
        /// The extension holding the name, or the registering extension when
        /// the batch repeats it.
        held_by: Box<str>,
    },
    /// An overlay tool names a declaring extension, skill, or server that
    /// is absent from the current generation.
    #[error(
        "tool \"{tool}\" has an invalid MCP declaration for extension \"{plugin}\" skill \"{skill}\": {reason}"
    )]
    McpToolDeclaration {
        /// The rejected mapped tool name.
        tool: Box<str>,
        /// The declaring plugin name.
        plugin: Box<str>,
        /// The declaring skill name.
        skill: Box<str>,
        /// The missing or malformed declaration component.
        reason: &'static str,
    },
}

impl ServiceError {
    /// Creates a failure of `service` whose display text is `message`.
    #[must_use]
    pub fn failed(service: Option<Service>, message: impl Into<Box<str>>) -> Self {
        Self::Failed {
            service,
            message: message.into(),
        }
    }

    /// A turn operation needs a running turn (`turn.not_running`).
    #[must_use]
    pub fn turn_not_running() -> Self {
        Self::failed(
            Some(Service::Turn),
            "no turn is running; use wake to start one.",
        )
    }

    /// A sidecar name fails the name rule (`sidecar.bad_name`).
    #[must_use]
    pub fn sidecar_bad_name(name: &str) -> Self {
        Self::failed(
            Some(Service::Sidecar),
            format!(
                "sidecar name \"{name}\" is invalid; use a-z, 0-9, dot, dash, and underscore, at most 64 characters, starting with a letter or digit."
            ),
        )
    }

    /// A sidecar value exceeds the size limit (`sidecar.too_large`).
    #[must_use]
    pub fn sidecar_too_large(name: &str, bytes: u64) -> Self {
        Self::failed(
            Some(Service::Sidecar),
            format!(
                "sidecar value for \"{name}\" is {bytes} bytes; the limit is {SIDECAR_VALUE_LIMIT}."
            ),
        )
    }

    /// A headless front end already holds an open question.
    #[must_use]
    pub fn ask_busy() -> Self {
        Self::failed(
            Some(Service::Ask),
            "another question is already open in this front end",
        )
    }
}

/// Renders a [`DenyReason`] from its owned data.
struct DenyText<'a>(&'a DenyReason);

/// Renders a denial reason as product text for command rejection.
///
/// Approval denials minted by the session dispatcher carry complete
/// `Permission denied ...` text in `OutOfScope.what` and render verbatim;
/// every other out-of-scope denial keeps the legacy coverture. The core
/// `DenyReason` vocabulary has no approval-denied variant, so the prefix
/// is the dispatcher's marker until one lands.
pub(crate) fn deny_text(reason: &DenyReason) -> String {
    DenyText(reason).to_string()
}

impl fmt::Display for DenyText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            DenyReason::NotInjected => {
                formatter.write_str("denied: the service is not in this extension's inject list")
            }
            DenyReason::NotGranted => {
                formatter.write_str("denied: the extension's services are not granted")
            }
            DenyReason::NoFrontEnd => formatter.write_str("denied: no front end can answer"),
            DenyReason::Unavailable { what } => write!(formatter, "denied: {what} is unavailable"),
            DenyReason::OutOfScope { what } => {
                if what.starts_with("Permission denied") {
                    formatter.write_str(what)
                } else {
                    write!(formatter, "denied: {what} is outside the approved scope")
                }
            }
            DenyReason::WakeLimit => write!(
                formatter,
                "wake refused: {WAKE_LIMIT} turns in a row started by wake with no user prompt (limit {WAKE_LIMIT}). A user prompt resets the count."
            ),
            _ => formatter.write_str("denied"),
        }
    }
}

/// A scheme could not resolve a URI.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SchemeError {
    /// No resolver is registered for the scheme.
    #[error("unknown scheme {scheme}")]
    Unknown {
        /// The scheme name without `://`.
        scheme: Box<str>,
    },
    /// The resolver knows the scheme but has no document at the URI.
    #[error("no document at {uri}")]
    NotFound {
        /// The full URI.
        uri: Box<str>,
    },
    /// The resolver knows the scheme but has no document at the URI, and a
    /// nearby URI exists for did-you-mean hints.
    #[error("no document at {uri}")]
    Near {
        /// The full URI.
        uri: Box<str>,
        /// The nearby URI.
        nearest: Box<str>,
    },
    /// The resolver failed. The message is the complete product text.
    #[error("{message}")]
    Failed {
        /// The product text.
        message: Box<str>,
    },
    /// The store failed. The display text is the store text.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<BlobError> for SchemeError {
    fn from(error: BlobError) -> Self {
        Self::Store(StoreError::Blob(error))
    }
}

/// A tool's process, scheme, or own operation failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// An admission wait expired before a permit was free.
    #[error("{}", admission_text(*.limit))]
    Admission {
        /// The exhausted resource.
        limit: AdmissionLimit,
    },
    /// The process could not start.
    #[error("cannot start {}: {source}", .path.display())]
    Spawn {
        /// The program or working directory that failed.
        path: PathBuf,
        /// The operating-system error.
        source: io::Error,
    },
    /// The call was cancelled (`tool.interrupted`).
    #[error("Tool call interrupted by user.")]
    Cancelled,
    /// A scope or availability check refused the call before any effect.
    #[error("{}", DenyText(.0))]
    Denied(DenyReason),
    /// A scheme read failed.
    #[error(transparent)]
    Scheme(#[from] SchemeError),
    /// A tool or handler returned exact user-facing error text.
    #[error("{message}")]
    Message {
        /// The exact text to show.
        message: Box<str>,
    },
    /// The tool's own operation failed. The display text is the tool's text.
    #[error(transparent)]
    Failed(Box<dyn std::error::Error + Send + Sync + 'static>),
}
impl ToolError {
    /// Creates a tool error with exact model-visible text.
    #[must_use]
    pub fn message(message: impl Into<Box<str>>) -> Self {
        Self::Message {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::num::NonZeroU64;

    use super::*;

    fn turn(value: u64) -> TurnId {
        TurnId::new(NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN))
    }

    #[test]
    fn wrong_turn_renders_both_sides() {
        let running = AgentError::WrongTurn {
            expected: Expect::Idle.into(),
            actual: ActualTurn::Turn {
                turn: turn(1),
                phase: TurnPhase::Running,
            },
        };
        assert_eq!(
            running.to_string(),
            "turn mismatch: expected no running turn, found turn 1 (running). Nothing changed."
        );
        let ended = AgentError::WrongTurn {
            expected: Expect::After(turn(1)).into(),
            actual: ActualTurn::Turn {
                turn: turn(2),
                phase: TurnPhase::Ended,
            },
        };
        assert_eq!(
            ended.to_string(),
            "turn mismatch: expected turn 1, found turn 2 (ended). Nothing changed."
        );
        let idle = AgentError::WrongTurn {
            expected: ExpectedTurn::Turn(turn(1)),
            actual: ActualTurn::Idle,
        };
        assert_eq!(
            idle.to_string(),
            "turn mismatch: expected turn 1, found no turn (idle). Nothing changed."
        );
    }

    #[test]
    fn agent_errors_render_product_text() {
        let id = RequestId::new_v7();
        let session = SessionId::new_v7();
        let job = JobId::new_v7();
        let cases = [
            (
                AgentError::AlreadyResolved {
                    id,
                    by: ClientId::new("tui"),
                },
                format!("request {id} was already resolved by tui."),
            ),
            (
                AgentError::SteerFull { turn: turn(1) },
                "steer queue is full (16) for turn 1; wait for the next step or cancel.".to_owned(),
            ),
            (
                AgentError::SessionClosed { id: session },
                format!("session {session} is closed."),
            ),
            (
                ValidationError::compacting().into(),
                "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.".to_owned(),
            ),
            (
                ValidationError::busy_turn("compact", turn(3)).into(),
                "compact needs an idle session; turn 3 is running. Cancel it or wait for it to end.".to_owned(),
            ),
            (
                ValidationError::job_not_running(job).into(),
                format!("job {job} is not running."),
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn bad_answer_names_answer_and_question_tags() {
        let question = Question::Confirm { text: "ok?".into() };
        assert_eq!(
            ValidationError::bad_answer(&Answer::ApproveForSession, &question).to_string(),
            "answer approve_for_session does not fit a confirm request."
        );
    }

    #[test]
    fn service_errors_render_product_text() {
        let cases = [
            (
                ServiceError::Denied(DenyReason::WakeLimit),
                "wake refused: 20 turns in a row started by wake with no user prompt (limit 20). A user prompt resets the count.",
            ),
            (
                ServiceError::turn_not_running(),
                "no turn is running; use wake to start one.",
            ),
            (
                ServiceError::sidecar_bad_name("../x"),
                "sidecar name \"../x\" is invalid; use a-z, 0-9, dot, dash, and underscore, at most 64 characters, starting with a letter or digit.",
            ),
            (
                ServiceError::sidecar_too_large("notes", 2_097_152),
                "sidecar value for \"notes\" is 2097152 bytes; the limit is 1048576.",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
        assert_eq!(
            ServiceError::ask_busy(),
            ServiceError::Failed {
                service: Some(Service::Ask),
                message: "another question is already open in this front end".into(),
            }
        );
    }

    #[test]
    fn host_errors_render_product_text() {
        let id = SessionId::new_v7();
        assert_eq!(
            HostError::SessionBusy { id, pid: Some(42) }.to_string(),
            format!("session {id} is open in process 42")
        );
        assert_eq!(
            HostError::max_depth(1).to_string(),
            "child sessions cannot start children here: agents.max_depth = 1."
        );
    }

    #[test]
    fn store_lock_becomes_session_busy_and_other_failures_stay_typed() {
        let id = SessionId::new_v7();
        let locked = HostError::from(StoreError::Locked {
            session: id,
            pid: Some(7),
            path: PathBuf::from("/s/lock"),
        });
        assert!(matches!(
            locked,
            HostError::SessionBusy { id: busy, pid: Some(7) } if busy == id
        ));

        let version = HostError::from(StoreError::UnknownVersion {
            path: PathBuf::from("/s/journal.jsonl"),
            found: 9,
        });
        assert!(matches!(
            version,
            HostError::Store(StoreError::UnknownVersion { found: 9, .. })
        ));
        assert_eq!(
            version.to_string(),
            "session file /s/journal.jsonl uses journal format 9; this dalgon reads format 1. Update dalgon to open it."
        );
    }

    #[test]
    fn io_sources_chain_to_the_os_error() {
        let host = HostError::Io {
            path: PathBuf::from("/w"),
            source: io::Error::new(io::ErrorKind::NotFound, "gone"),
        };
        let source = host.source().and_then(|e| e.downcast_ref::<io::Error>());
        assert_eq!(source.map(io::Error::kind), Some(io::ErrorKind::NotFound));

        let tool = ToolError::Spawn {
            path: PathBuf::from("/bin/x"),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "no"),
        };
        assert_eq!(tool.to_string(), "cannot start /bin/x: no");
        let source = tool.source().and_then(|e| e.downcast_ref::<io::Error>());
        assert_eq!(
            source.map(io::Error::kind),
            Some(io::ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn scheme_errors_keep_store_text_and_type() {
        let blob = SchemeError::from(BlobError::Gone);
        assert!(matches!(
            blob,
            SchemeError::Store(StoreError::Blob(BlobError::Gone))
        ));
        assert_eq!(
            blob.to_string(),
            "the session was deleted, so its blobs are gone"
        );

        let tool = ToolError::from(SchemeError::Unknown {
            scheme: "foo".into(),
        });
        assert_eq!(tool.to_string(), "unknown scheme foo");
        assert!(matches!(
            tool,
            ToolError::Scheme(SchemeError::Unknown { .. })
        ));
    }

    #[test]
    fn tool_message_preserves_exact_handler_text() {
        assert_eq!(
            ToolError::message("tool failed: exact").to_string(),
            "tool failed: exact"
        );
    }

    #[test]
    fn tool_denial_keeps_the_typed_reason() {
        let denied = ToolError::Denied(DenyReason::OutOfScope {
            what: "/etc/passwd".into(),
        });
        assert_eq!(
            denied.to_string(),
            "denied: /etc/passwd is outside the approved scope"
        );
        assert!(matches!(
            denied,
            ToolError::Denied(DenyReason::OutOfScope { .. })
        ));
        assert_eq!(
            ToolError::Cancelled.to_string(),
            "Tool call interrupted by user."
        );
    }
}
