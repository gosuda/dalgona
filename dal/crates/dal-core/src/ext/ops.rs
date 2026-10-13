use super::{
    CallId, Deserialize, Deserializer, Duration, EntryId, JobId, JobOutcome, Name, RawJson,
    Serialize, SessionId, Stop, Tagged, Workspace, de,
};

/// Configuration for starting a child agent session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentStart {
    /// The provider call that requested this child.
    pub call: CallId,
    /// The child session's display name.
    pub name: Box<str>,
    /// The prompt for the child session.
    pub prompt: Box<str>,
    /// An optional model identifier.
    pub model: Option<Box<str>>,
    /// An optional role name.
    pub role: Option<Box<str>>,
    /// An optional explicit system prompt.
    pub system: Option<Box<str>>,
    /// Optional tool names; absence differs from an explicit empty set.
    #[serde(
        default,
        deserialize_with = "super::names::deserialize_optional_tool_names"
    )]
    pub tools: Option<Box<[Name]>>,
    /// An optional child workspace.
    pub workspace: Option<Workspace>,
}

impl AgentStart {
    /// Checks that mutually exclusive role and system options are not combined.
    ///
    /// # Errors
    /// Returns [`AgentsOpError::RoleAndSystem`] when both options are present.
    pub fn validate(&self) -> Result<(), AgentsOpError> {
        if self.role.is_some() && self.system.is_some() {
            return Err(AgentsOpError::RoleAndSystem);
        }
        Ok(())
    }
}

/// The visible lifecycle state of one child agent.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AgentState {
    /// Waiting for a child-session capacity slot.
    Queued,
    /// Admitted and running.
    Running,
    /// Finished with its terminal reason.
    Done(Stop),
}

/// A child agent's identity and current state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    /// The child session.
    pub id: SessionId,
    /// The display name supplied at start.
    pub name: Box<str>,
    /// The child's current lifecycle state.
    pub state: AgentState,
}

/// A completed child's report and its durable journal pointer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentReport {
    /// The child's terminal reason.
    pub stop: Stop,
    /// The final assistant-visible report text.
    pub text: Box<str>,
    /// The completed child session.
    pub session: SessionId,
    /// The journal entry containing the full report.
    pub entry: EntryId,
}

/// How a mailbox message should be delivered to its recipient.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MailMode {
    /// Deliver as an aside without steering the current turn.
    Aside,
    /// Steer the currently running turn.
    Steer,
    /// Queue for the recipient's next turn.
    NextTurn,
}

/// A mailbox message exchanged between two sessions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Mail {
    /// The sending session.
    pub from: SessionId,
    /// The receiving session.
    pub to: SessionId,
    /// The requested delivery mode.
    pub mode: MailMode,
    /// The message text.
    pub text: Box<str>,
    /// The journal entry to which this message replies, if any.
    pub reply_to: Option<EntryId>,
}

/// The receipt returned after a mailbox send attempt.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Receipt {
    /// The message was delivered to the recipient.
    Delivered,
    /// Delivery woke the recipient's session.
    Woken,
    /// The message was buffered for a later turn.
    Buffered,
    /// The recipient's mailbox has no available capacity.
    Full,
    /// The recipient does not exist or has ended.
    Gone,
}

/// An agent-session operation requested by a host service.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentsOp {
    /// Starts a child agent session.
    Start(AgentStart),
    /// Waits for a child session to finish.
    Await {
        /// The child session to await.
        id: SessionId,
        /// The longest wait; absent waits until the child finishes.
        timeout: Option<Duration>,
    },
    /// Cancels a child session.
    Cancel {
        /// The child session to cancel.
        id: SessionId,
    },
    /// Lists child sessions.
    List,
    /// Sends a mailbox message to one session.
    Send {
        /// The recipient session.
        to: SessionId,
        /// The message text.
        text: Box<str>,
        /// The delivery mode.
        mode: MailMode,
        /// The journal entry to reply to, if any.
        reply_to: Option<EntryId>,
    },
    /// Reads mailbox messages after a journal cursor.
    Recv {
        /// The last entry already read, if any.
        after: Option<EntryId>,
        /// Optional maximum wait duration for new mail.
        timeout: Option<Duration>,
    },
    /// Sends one message to each child session.
    Broadcast {
        /// The message text.
        text: Box<str>,
        /// The delivery mode.
        mode: MailMode,
    },
}

/// The result or failure of an agent-session operation.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AgentsOpError {
    /// A start request supplied both a role and a system prompt.
    #[error("role and system cannot both be set")]
    RoleAndSystem,
    /// Agent capacity is currently full.
    #[error("agent capacity is full")]
    Full,
    /// The requested child session is unavailable.
    #[error("agent session is gone")]
    Gone,
    /// The operation is unavailable in the current context.
    #[error("agents operation is unavailable: {what}")]
    Unavailable {
        /// The operation or context that is unavailable.
        what: &'static str,
    },
    /// An operation failed with an owned message.
    #[error("agents operation failed: {message}")]
    Failed {
        /// The failure description.
        message: Box<str>,
    },
}

/// A background-job operation requested by a host service.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum JobsOp {
    /// Starts a named background job with a raw JSON payload.
    Spawn {
        /// The registered job name.
        name: Name,
        /// The job's input payload, preserved as raw JSON.
        payload: RawJson,
        /// The parent job; cancellation cascades through this relation.
        parent: Option<JobId>,
    },
    /// Reads the state of one background job.
    Status {
        /// The job to inspect.
        id: JobId,
    },
    /// Looks one job up without failing when it is unknown.
    Find {
        /// The job to look up.
        id: JobId,
    },
    /// Cancels one background job and every job that runs under it.
    Cancel {
        /// The job to cancel.
        id: JobId,
    },
    /// Awaits the finished outcome of one background job.
    Wait {
        /// The job to await.
        id: JobId,
        /// The longest wait; absence waits until the job ends.
        timeout: Option<Duration>,
    },
    /// Lists the known background jobs.
    List,
    /// Reads the collected output text of one background job.
    Text {
        /// The job to read.
        id: JobId,
    },
    /// Counts the session's jobs by state.
    Counts,
    /// Reads job-end events after a cursor.
    Ends {
        /// The last event sequence already read, if any.
        after: Option<u64>,
        /// The longest wait for a new event.
        timeout: Option<Duration>,
    },
    /// Reads output lines of one job after a cursor.
    Lines {
        /// The job to read.
        id: JobId,
        /// The last line sequence already read, if any.
        after: Option<u64>,
        /// The longest wait for a new line.
        timeout: Option<Duration>,
    },
    /// Takes ended top-level reports that no wake has claimed.
    Take {
        /// The most reports to take.
        limit: u16,
    },
    /// Commits taken reports as delivered; a committed report never returns.
    Commit {
        /// The reports to commit.
        ids: Vec<JobId>,
    },
    /// Returns taken reports so a later take offers them again.
    Release {
        /// The reports to release.
        ids: Vec<JobId>,
    },
    /// Adds a job to the held set kept beside the session lock.
    Hold {
        /// The job to hold.
        id: JobId,
    },
    /// Removes a job from the held set.
    Unhold {
        /// The job to release from the held set.
        id: JobId,
    },
    /// Ends a job started with [`JobsOp::Spawn`] by the extension that spawned
    /// it, through the same end path a process exit takes.
    Settle {
        /// The job to end.
        id: JobId,
        /// The one terminal outcome.
        outcome: JobOutcome,
        /// The report text served by `Text` and taken by `Take`.
        text: Box<str>,
    },
}

#[derive(Deserialize)]
pub(super) struct SpawnJobFields {
    pub(super) name: Name,
    pub(super) payload: RawJson,
    #[serde(default)]
    pub(super) parent: Option<JobId>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct JobFields {
    pub(super) id: Option<JobId>,
    pub(super) ids: Option<Vec<JobId>>,
    pub(super) after: Option<u64>,
    pub(super) timeout: Option<Duration>,
    pub(super) limit: Option<u16>,
    pub(super) outcome: Option<JobOutcome>,
    pub(super) text: Option<Box<str>>,
}

fn required<T, E: de::Error>(value: Option<T>, field: &'static str) -> Result<T, E> {
    value.ok_or_else(|| E::missing_field(field))
}

impl<'de> Deserialize<'de> for JobsOp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(
            deserializer,
            "type",
            &[
                "spawn", "status", "find", "cancel", "wait", "list", "text", "counts", "ends",
                "lines", "take", "commit", "release", "hold", "unhold", "settle",
            ],
        )?;
        if tagged.kind() == "spawn" {
            let wire: SpawnJobFields =
                sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
            return Ok(Self::Spawn {
                name: wire.name,
                payload: wire.payload,
                parent: wire.parent,
            });
        }
        let wire: JobFields = sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
        match tagged.kind() {
            "status" => Ok(Self::Status {
                id: required(wire.id, "id")?,
            }),
            "find" => Ok(Self::Find {
                id: required(wire.id, "id")?,
            }),
            "cancel" => Ok(Self::Cancel {
                id: required(wire.id, "id")?,
            }),
            "wait" => Ok(Self::Wait {
                id: required(wire.id, "id")?,
                timeout: wire.timeout,
            }),
            "list" => Ok(Self::List),
            "text" => Ok(Self::Text {
                id: required(wire.id, "id")?,
            }),
            "counts" => Ok(Self::Counts),
            "ends" => Ok(Self::Ends {
                after: wire.after,
                timeout: wire.timeout,
            }),
            "lines" => Ok(Self::Lines {
                id: required(wire.id, "id")?,
                after: wire.after,
                timeout: wire.timeout,
            }),
            "take" => Ok(Self::Take {
                limit: required(wire.limit, "limit")?,
            }),
            "commit" => Ok(Self::Commit {
                ids: required(wire.ids, "ids")?,
            }),
            "release" => Ok(Self::Release {
                ids: required(wire.ids, "ids")?,
            }),
            "hold" => Ok(Self::Hold {
                id: required(wire.id, "id")?,
            }),
            "unhold" => Ok(Self::Unhold {
                id: required(wire.id, "id")?,
            }),
            "settle" => Ok(Self::Settle {
                id: required(wire.id, "id")?,
                outcome: required(wire.outcome, "outcome")?,
                text: required(wire.text, "text")?,
            }),
            other => Err(de::Error::custom(format!("unknown jobs op `{other}`"))),
        }
    }
}

/// A turn operation requested by a host service.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TurnOp {
    /// Cancels the active turn.
    Cancel,
    /// Adds steering text to the active turn.
    Steer {
        /// The text to add.
        text: Box<str>,
    },
    /// Wakes a turn with collected text and source metadata.
    Wake {
        /// The text to add to the turn.
        text: Box<str>,
        /// Descriptions of the wake sources.
        sources: Vec<Box<str>>,
        /// Background jobs associated with the wake.
        job_ids: Vec<JobId>,
    },
    /// Checks whether the active turn is idle.
    IsIdle,
}

/// A sidecar read or write operation.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SidecarOp {
    /// Reads a named sidecar value.
    Read {
        /// The sidecar name.
        name: Name,
    },
    /// Writes bytes to a named sidecar value.
    Write {
        /// The sidecar name.
        name: Name,
        /// The bytes to store.
        bytes: Vec<u8>,
    },
    /// Atomically writes one fixed isolation artifact of one task job.
    Artifact {
        /// The task job the artifact belongs to.
        job: JobId,
        /// Which fixed artifact file to write.
        file: ArtifactFile,
        /// The bytes to store.
        bytes: Vec<u8>,
    },
}

/// The fixed files a task job's isolation directory may hold.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ArtifactFile {
    /// `delta.patch`: the task's recorded diff.
    DeltaPatch,
    /// `retained.json`: why the task's work was kept.
    RetainedJson,
    /// `summary.txt`: the task's one-screen summary.
    SummaryTxt,
}

impl ArtifactFile {
    /// Returns the literal file name under the task's isolation directory.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::DeltaPatch => "delta.patch",
            Self::RetainedJson => "retained.json",
            Self::SummaryTxt => "summary.txt",
        }
    }
}
