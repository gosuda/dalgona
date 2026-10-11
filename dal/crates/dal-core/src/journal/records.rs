use super::{
    Answer, CallId, ClientId, Entry, EntryId, FileChange, Gen, Header, InferredPurpose, JobEvent,
    JobId, Mail, Owner, RawJson, RequestId, RouteError, TurnEndStop, TurnId, Usage, fmt,
};

/// One journal record. Variants are the 32 format-1 `type` literals.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Record {
    /// The session header; exactly one, first in the file.
    Session(Header),
    /// A process boot; `gen` counts boots within the session.
    Boot {
        /// When the process started.
        at: jiff::Timestamp,
        /// The generation number.
        r#gen: Gen,
        /// The product version string.
        version: Box<str>,
    },
    /// A user message entry.
    User(Entry),
    /// An assistant response entry.
    Assistant(Entry),
    /// A tool result entry.
    ToolResult(Entry),
    /// A reminder entry.
    Reminder(Entry),
    /// A model-change entry.
    Model(Entry),
    /// A thinking-level entry.
    Thinking(Entry),
    /// An approval-mode entry.
    Approval(Entry),
    /// A harness-mode entry.
    Mode(Entry),
    /// A compaction entry.
    Compaction(Entry),
    /// A branch-summary entry.
    BranchSummary(Entry),
    /// The current leaf moved.
    Leaf {
        /// When the move was recorded.
        at: jiff::Timestamp,
        /// The new leaf, or none.
        to: Option<EntryId>,
    },
    /// A label was attached to or removed from an entry.
    Label {
        /// When the label changed.
        at: jiff::Timestamp,
        /// The entry being labelled.
        entry: EntryId,
        /// The label text, or none to clear it.
        label: Option<Box<str>>,
    },
    /// The session was renamed or unnamed.
    Name {
        /// When the name changed.
        at: jiff::Timestamp,
        /// The new name, or none to clear it.
        name: Option<Box<str>>,
    },
    /// The session was archived or unarchived.
    Archive {
        /// When the flag changed.
        at: jiff::Timestamp,
        /// The archived flag.
        archived: bool,
    },
    /// A turn started.
    TurnStart {
        /// When the turn started.
        at: jiff::Timestamp,
        /// The turn counter.
        turn: TurnId,
    },
    /// A tool call began within a turn.
    ToolStart {
        /// When the call started.
        at: jiff::Timestamp,
        /// The owning turn.
        turn: TurnId,
        /// The call identifier.
        call: CallId,
    },
    /// A turn ended.
    TurnEnd {
        /// When the turn ended.
        at: jiff::Timestamp,
        /// The turn counter.
        turn: TurnId,
        /// Why the turn ended.
        stop: TurnEndStop,
        /// The turn's summed usage.
        usage: Option<Usage>,
        /// The turn's file changes.
        changes: Vec<FileChange>,
    },
    /// A rule fired during a turn.
    RuleFired {
        /// When the rule fired.
        at: jiff::Timestamp,
        /// The owning turn.
        turn: TurnId,
        /// The rule name.
        rule: Box<str>,
        /// The reminder entry it created.
        entry: EntryId,
    },
    /// A pending request was answered.
    Resolved {
        /// When the answer was recorded.
        at: jiff::Timestamp,
        /// The request answered.
        request: RequestId,
        /// The answer given.
        answer: Answer,
        /// Who answered.
        by: ClientId,
        /// Whether the answer was the request's default. The member is
        /// written only when it is true.
        was_default: bool,
    },
    /// A client granted a tool always-allowance.
    AllowAlways {
        /// When the allowance was granted.
        at: jiff::Timestamp,
        /// The tool name.
        tool: Box<str>,
        /// Who granted it.
        by: ClientId,
    },
    /// An extension was granted services.
    GrantGiven {
        /// When the grant was given.
        at: jiff::Timestamp,
        /// The extension name.
        ext: Box<str>,
        /// The granted service names.
        set: Vec<Box<str>>,
        /// The grant scope literal.
        scope: Box<str>,
        /// Who granted it.
        by: ClientId,
    },
    /// A call-scoped argv-prefix grant was recorded.
    ScopedGrant {
        /// When the grant was recorded.
        at: jiff::Timestamp,
        /// The call the grant rides on.
        call: CallId,
        /// The argv prefix tokens.
        prefix: Vec<Box<str>>,
        /// The covered roots.
        roots: Vec<Box<str>>,
        /// The job whose end revokes the grant.
        job: JobId,
        /// Who granted it.
        by: ClientId,
    },
    /// A call-scoped grant's job ended.
    ScopedGrantEnded {
        /// When the grant ended.
        at: jiff::Timestamp,
        /// The ended job.
        job: JobId,
    },
    /// A `before_request` hook mutated a field.
    BeforeRequestMut {
        /// When the mutation was recorded.
        at: jiff::Timestamp,
        /// The owning turn.
        turn: TurnId,
        /// The extension that mutated the request.
        ext: Box<str>,
        /// The field name.
        field: Box<str>,
        /// The previous value rendering.
        old: Box<str>,
        /// The new value rendering.
        new: Box<str>,
    },
    /// A tool was promoted for later turns.
    ToolPromoted {
        /// When the promotion was recorded.
        at: jiff::Timestamp,
        /// The tool name.
        tool: Box<str>,
        /// The owning turn, when recorded.
        turn: Option<TurnId>,
        /// The leaf at promotion time, when recorded.
        leaf: Option<EntryId>,
    },
    /// An automatic wake attempt was made.
    WakeAttempt {
        /// When the wake was attempted.
        at: jiff::Timestamp,
        /// The owning turn.
        turn: TurnId,
        /// The wake count within the turn.
        count: u32,
        /// The ended jobs whose reports this wake delivered; the journal write
        /// that starts the turn is what marks them delivered.
        jobs: Vec<JobId>,
    },
    /// A job lifecycle event.
    Job {
        /// When the event was recorded.
        at: jiff::Timestamp,
        /// The job identifier.
        job: JobId,
        /// The event.
        event: JobEvent,
    },
    /// An extension's opaque durable record.
    Ext {
        /// When the record was written.
        at: jiff::Timestamp,
        /// The owning extension.
        ext: Box<str>,
        /// The extension-defined kind.
        kind: Box<str>,
        /// The opaque body.
        body: RawJson,
    },
    /// A mailbox delivery.
    Mail(Mail),
    /// An attributed synthetic inner inference.
    Inferred {
        /// When the inference was recorded.
        at: jiff::Timestamp,
        /// Who ran the inner call.
        who: Owner,
        /// Why it ran.
        purpose: InferredPurpose,
        /// Its normalized usage.
        usage: Usage,
    },
}

/// The parsed record of one journal line.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    /// The record the line held.
    pub record: Record,
}

/// A line that is not a valid format-1 record.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum DecodeError {
    /// The record carries no `"v"` member.
    #[error("record has no format version")]
    MissingVersion,
    /// The `"v"` member names a format this build does not read.
    #[error("unsupported record format version {found}; this dalgon reads format 1")]
    UnsupportedVersion {
        /// The version found.
        found: u64,
    },
    /// The `"type"` member is missing, not a string, or not a format-1 kind.
    #[error("unknown record type {kind:?}")]
    UnknownRecordKind {
        /// The type literal found.
        kind: Box<str>,
    },
    /// A known record's member failed to decode, or a member this format
    /// does not know appeared.
    #[error("invalid record at byte {offset}: {message}")]
    Invalid {
        /// The byte offset of the offending member in the line.
        offset: usize,
        /// The decode failure.
        message: Box<str>,
    },
}

/// A record that cannot be serialized.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum EncodeError {
    /// The JSON writer rejected a value.
    #[error("cannot encode record: {message}")]
    Json {
        /// The encoder failure.
        message: Box<str>,
    },
    /// A reported cost cannot live in an integer micro-dollar member.
    ///
    /// [`Usage::cost_usd`] holds provider-reported dollars; the journal
    /// stores micro-dollars. A non-finite or negative cost, or one above
    /// the `u64` range, has no integer spelling, and writing `null` would
    /// lie that no cost was reported.
    #[error("cannot encode a non-finite, negative, or out-of-range cost")]
    InvalidCost,
    /// A tree record's variant does not match its entry kind.
    ///
    /// `Record::User` must carry `EntryKind::User`, and so on; a mismatch
    /// is a caller bug the codec reports rather than encodes wrong.
    #[error("record variant does not match its entry kind")]
    MismatchedKind,
    /// A synthetic or harness route id breaks its closed grammar.
    ///
    /// The decoder rejects such an id, so writing it would leave a line
    /// no reader accepts.
    #[error("cannot encode model route: {0}")]
    InvalidRoute(RouteError),
    /// An assistant or API model record names an empty model id.
    ///
    /// The decoder rejects an empty `model`, and no provider route can
    /// replay one, so the record is refused instead of written.
    #[error("cannot encode an empty model id")]
    EmptyModel,
}

/// The fixed prefix of a tree record, read without allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScannedHead {
    /// The entry identifier.
    pub id: EntryId,
    /// The parent identifier.
    pub parent: Option<EntryId>,
    /// The record's tree kind.
    pub kind: TreeKind,
}

/// The ten tree-record tags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TreeKind {
    /// `user`.
    User,
    /// `assistant`.
    Assistant,
    /// `tool_result`.
    ToolResult,
    /// `reminder`.
    Reminder,
    /// `model`.
    Model,
    /// `thinking`.
    Thinking,
    /// `approval`.
    Approval,
    /// `mode`.
    Mode,
    /// `compaction`.
    Compaction,
    /// `branch_summary`.
    BranchSummary,
}

impl TreeKind {
    /// The format-1 `type` literal of this kind.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::ToolResult => "tool_result",
            Self::Reminder => "reminder",
            Self::Model => "model",
            Self::Thinking => "thinking",
            Self::Approval => "approval",
            Self::Mode => "mode",
            Self::Compaction => "compaction",
            Self::BranchSummary => "branch_summary",
        }
    }

    pub(super) fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool_result" => Self::ToolResult,
            "reminder" => Self::Reminder,
            "model" => Self::Model,
            "thinking" => Self::Thinking,
            "approval" => Self::Approval,
            "mode" => Self::Mode,
            "compaction" => Self::Compaction,
            "branch_summary" => Self::BranchSummary,
            _ => return None,
        })
    }
}

impl fmt::Display for TreeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.tag())
    }
}
