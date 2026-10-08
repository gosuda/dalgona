use super::{
    AgentInfo, AgentReport, Deserialize, Duration, EntryId, JobId, JobOutcome, Mail, RawJson,
    Receipt, Serialize, SessionId,
};

/// A process execution request passed to a host service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    /// The executable and its argument vector.
    pub argv: Vec<std::ffi::OsString>,
    /// An optional working directory.
    pub cwd: Option<std::path::PathBuf>,
    /// Optional standard-input bytes.
    pub stdin: Option<Vec<u8>>,
    /// Optional process deadline.
    pub timeout: Option<Duration>,
    /// Explicit child environment overrides; an empty list adds no overrides.
    #[serde(default)]
    pub env: Vec<(Box<str>, Box<str>)>,
    /// Maximum stdout prefix bytes retained, including the overflow witness.
    pub stdout_prefix_limit: u32,
}

impl RunRequest {
    /// Validates environment variable names before a host process launch.
    ///
    /// # Errors
    /// Returns [`RunRequestError::InvalidEnvironmentName`] for an empty name
    /// or one containing `=` or NUL.
    pub fn validate_env(&self) -> Result<(), RunRequestError> {
        for (name, _) in &self.env {
            if name.is_empty() || name.contains(['=', '\0']) {
                return Err(RunRequestError::InvalidEnvironmentName { name: name.clone() });
            }
        }
        Ok(())
    }
}

/// An invalid process request value.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RunRequestError {
    /// An environment variable name cannot be passed to the host process API.
    #[error("invalid environment variable name {name:?}")]
    InvalidEnvironmentName {
        /// The rejected name.
        name: Box<str>,
    },
}

/// The HTTP method of an outgoing fetch request.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "UPPERCASE")]
pub enum FetchMethod {
    /// HTTP GET.
    Get,
    /// HTTP POST.
    Post,
    /// HTTP PUT.
    Put,
    /// HTTP DELETE.
    Delete,
    /// HTTP HEAD.
    Head,
    /// HTTP OPTIONS.
    Options,
    /// HTTP PATCH.
    Patch,
}

/// An outgoing HTTP request passed to the host `net` service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FetchRequest {
    /// The HTTP method.
    pub method: FetchMethod,
    /// The absolute HTTP or HTTPS URL.
    pub url: Box<str>,
    /// Request headers as ordered name/value pairs.
    pub headers: Vec<(Box<str>, Box<str>)>,
    /// The request body bytes.
    pub body: Vec<u8>,
}

/// An incoming HTTP response returned by the host `net` service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FetchResponse {
    /// The HTTP status code.
    pub status: u16,
    /// Response headers as ordered name/value pairs.
    pub headers: Vec<(Box<str>, Box<str>)>,
    /// The response body bytes, bounded by the caller.
    pub body: Vec<u8>,
}

impl FetchResponse {
    /// Returns the HTTP status code.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Returns the first header value with a case-insensitive name match.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.as_ref()))
    }

    /// Returns at most `max` leading body bytes without copying.
    #[must_use]
    pub fn read_bytes(&self, max: usize) -> &[u8] {
        let end = max.min(self.body.len());
        &self.body[..end]
    }
}
/// A Model Context Protocol tool-call request passed to the host `mcp` service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct McpRequest {
    /// The session issuing the request.
    pub session: SessionId,
    /// The MCP server name.
    pub server: Box<str>,
    /// The MCP tool name.
    pub tool: Box<str>,
    /// The tool arguments, preserved as raw JSON.
    pub arguments: RawJson,
}

/// A Model Context Protocol tool-call response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct McpResponse {
    /// The response text.
    pub text: Box<str>,
    /// Whether the response reports an error.
    pub is_error: bool,
}

/// The collected output of a process execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RunOutput {
    /// The process exit status.
    pub status: ExitStatusKind,
    /// The retained tail of standard output.
    pub stdout_tail: Vec<u8>,
    /// The retained stdout prefix requested by the caller.
    pub stdout_prefix: Vec<u8>,
    /// Whether stdout continued beyond the retained prefix.
    pub stdout_prefix_overflowed: bool,
    /// The retained tail of standard error.
    pub stderr_tail: Vec<u8>,
    /// An optional path to the complete process log.
    pub log: Option<std::path::PathBuf>,
}

/// The portable exit state of a process.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ExitStatusKind {
    /// The process exited with a numeric status.
    Exited(i32),
    /// The process was terminated by a signal number.
    Signaled(i32),
    /// The configured process deadline elapsed.
    TimedOut,
    /// The process was aborted by its host.
    Aborted,
}

/// The result of an agent-session operation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentsReply {
    /// A child session was started.
    Started {
        /// The new child session.
        id: SessionId,
    },
    /// A child session finished with its report.
    Await {
        /// The completed child report and journal pointer.
        report: AgentReport,
    },
    /// A child session was cancelled.
    Cancelled {
        /// The cancelled child session.
        id: SessionId,
    },
    /// The child sessions currently known to the host.
    Listed(Vec<AgentInfo>),
    /// A child session was still running when the wait ended.
    Pending {
        /// The child session that has not finished.
        id: SessionId,
    },
    /// The receipt for a mailbox send.
    Delivered(Receipt),
    /// Messages read from a mailbox.
    Received {
        /// The mailbox messages returned after the requested cursor.
        mail: Vec<Mail>,
        /// The cursor to use for the next mailbox read.
        next: Option<EntryId>,
    },
}

/// The result of a background-job operation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum JobsReply {
    /// A background job was started.
    Spawned {
        /// The new job identity.
        id: JobId,
    },
    /// The current state of a background job.
    Status(JobStatus),
    /// A background job was cancelled.
    Cancelled {
        /// The cancelled job identity.
        id: JobId,
    },
    /// A background job finished with its journal outcome.
    Waited {
        /// The finished job identity.
        id: JobId,
        /// The job's recorded outcome.
        outcome: JobOutcome,
    },
    /// The background jobs currently known to the host.
    Listed(Vec<JobStatus>),
    /// The collected output text of a background job.
    Text {
        /// The job identity.
        id: JobId,
        /// The collected output text.
        text: Box<str>,
    },
    /// One job looked up by id; absent when the session knows no such job.
    Found(Option<JobStatus>),
    /// The session's jobs counted by state.
    Counts(JobCounts),
    /// Job-end events after a cursor.
    Ended(JobEnds),
    /// Output lines of one job after a cursor.
    Lines(JobLines),
    /// Ended top-level reports taken for one delivery.
    Taken(Vec<JobReport>),
    /// The reports a commit newly marked delivered.
    Committed {
        /// The committed ids; a repeated commit returns none.
        ids: Vec<JobId>,
    },
    /// The reports a release returned to the ended queue.
    Released {
        /// The released ids.
        ids: Vec<JobId>,
    },
    /// The held-job set after a hold or unhold.
    Held(Vec<JobId>),
    /// A job ended by its owner.
    Settled {
        /// The ended job.
        id: JobId,
    },
    /// The operation was refused and changed nothing.
    Refused(JobsError),
    /// The request could not be answered; no job state is implied.
    Unavailable {
        /// The machine-readable reason.
        reason: Box<str>,
    },
}

/// Why a job operation was refused.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum JobsError {
    /// The session knows no such job.
    #[error("job {id:?} is not known to this session")]
    Unknown {
        /// The requested job.
        id: JobId,
    },
    /// Only the extension that spawned a job may settle it.
    #[error("job {id:?} was spawned by another extension")]
    NotOwner {
        /// The requested job.
        id: JobId,
    },
    /// The job already has its one terminal outcome.
    #[error("job {id:?} already ended")]
    AlreadyEnded {
        /// The requested job.
        id: JobId,
    },
    /// A settle text exceeded the report limit.
    #[error("report text exceeds {limit} bytes")]
    TooLarge {
        /// The byte limit.
        limit: usize,
    },
    /// The job ledger could not record the change.
    #[error("{message}")]
    Ledger {
        /// The ledger failure text.
        message: Box<str>,
    },
    /// Only a job spawned with `Spawn` is ended by `Settle`.
    #[error("job {id:?} is not owner-settled")]
    NotSettleable {
        /// The requested job.
        id: JobId,
    },
}

/// A snapshot of one background job's public state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobStatus {
    /// The job identity.
    pub id: JobId,
    /// A display label for the job.
    pub label: Box<str>,
    /// The job's current state.
    pub state: JobStateView,
    /// An optional path to the job's log.
    pub log: Option<std::path::PathBuf>,
    /// When the job last produced output or changed state.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub last_activity_at: jiff::Timestamp,
}

/// The session's jobs counted by state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobCounts {
    /// Jobs waiting for a process slot.
    pub queued: usize,
    /// Jobs running in the foreground.
    pub running: usize,
    /// Jobs running detached.
    pub detached: usize,
    /// Ended jobs the table still remembers.
    pub done: usize,
    /// Jobs in the held set.
    pub held: usize,
}

/// One job's end, delivered exactly once per job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobEndEvent {
    /// The event's position in the session's end log, from 1.
    pub seq: u64,
    /// The ended job.
    pub id: JobId,
    /// The job's label.
    pub label: Box<str>,
    /// The terminal outcome.
    pub outcome: JobOutcome,
    /// Whether the job ran under no parent.
    pub top_level: bool,
}

/// A read of the job-end log.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobEnds {
    /// The events after the requested cursor, oldest first.
    pub events: Vec<JobEndEvent>,
    /// The cursor to pass as `after` next time.
    pub next: u64,
    /// Events the bounded log discarded before this read reached them.
    pub dropped: u64,
}

/// One output line of a job.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobLine {
    /// The line's position in the job's output, from 1.
    pub seq: u64,
    /// The line text without its terminator.
    pub text: Box<str>,
}

/// A read of one job's bounded output lines.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobLines {
    /// The lines after the requested cursor, oldest first.
    pub lines: Vec<JobLine>,
    /// The cursor to pass as `after` next time.
    pub next: u64,
    /// Lines the bounded ring discarded before this read reached them.
    pub dropped: u64,
    /// Whether the job has ended, so no further line will come.
    pub ended: bool,
}

/// An ended top-level job's report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct JobReport {
    /// The ended job.
    pub id: JobId,
    /// The job's label.
    pub label: Box<str>,
    /// The terminal outcome.
    pub outcome: JobOutcome,
    /// The report text.
    pub text: Box<str>,
}

/// The public state of a background job.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum JobStateView {
    /// The job is running in the foreground.
    Running,
    /// The job continues independently of its foreground owner.
    Detached,
    /// The job completed with its journal outcome.
    Done(JobOutcome),
}

/// The result of a turn operation.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TurnOpReply {
    /// Reports whether the turn was idle.
    Idle(bool),
    /// Steering text was queued.
    Steered,
    /// The turn was woken.
    Woken,
    /// The wake started no turn.
    WakeRefused(WakeError),
    /// The turn was cancelled.
    Cancelled,
}

/// Why a wake started no turn.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WakeError {
    /// A turn or compaction is running, so the session is not idle.
    #[error("the session is busy: a turn or compaction is running")]
    Busy,
    /// The wake record could not be journaled.
    #[error("{message}")]
    Journal {
        /// The journal failure text.
        message: Box<str>,
    },
    /// The core's limit of turns in a row started by wake was reached.
    #[error(
        "wake refused: 20 turns in a row started by wake with no user prompt (limit 20). A user prompt resets the count."
    )]
    Limit,
}
