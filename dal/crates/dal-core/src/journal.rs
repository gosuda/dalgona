//! Versioned journal records, their strict line codec, and the pure
//! session-branch projection.
//!
//! `"v":1` and `"type"`. Encoding writes the declared member order so the
//! canonical vectors round-trip byte for byte. Decoding checks `"v"`
//! before `"type"` and the payload, drops record members this format does
//! not declare, and keeps the closed nested payloads strict.
//!
//! Durable member shapes follow the store's format-1 table rather than
//! the request wire: `who` and `purpose` use externally tagged objects,
//! `answer` is a bare literal or `{"value": ...}`, content parts carry
//! `base64` or `blob` spellings, `event` is one literal string, and the
//! stop members use their journal literals.
//!
//! Money is the integer `cost_micro_usd` member of the usage object,
//! never a float on disk. [`Usage`] holds provider-reported dollars in
//! `cost_usd`, so the codec converts at this boundary: encoding rounds a
//! reported cost to micro-dollars, and rejects a non-finite, negative,
//! or unrepresentable cost with [`EncodeError::InvalidCost`] rather than
//! writing `null`, which the format reserves for "no cost was reported".

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use sonic_rs::{JsonValueTrait, LazyValue};

use crate::config::ApprovalMode;
use crate::ext::MailMode;
use crate::id::{CallId, ClientId, EntryId, Gen, JobId, RequestId, SessionId, TurnId};
use crate::model::{Family, ModelRoute, RouteError, ThinkingLevel, Usage};
use crate::raw::{RawJson, Tagged};
use crate::request::{Answer, Owner};
use crate::view::FileChange;
use crate::workspace::Workspace;

/// The only journal format version this build reads and writes.
pub const VERSION: u16 = 1;

/// The product recorded in a session header.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Product {
    /// The dalgon terminal product.
    Dal,
    /// The dalgona terminal product.
    Dalgona,
}

/// The fork or clone provenance stored in a session header.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Source {
    /// The session this one branched from.
    pub session: SessionId,
    /// The anchor entry in the source session.
    pub entry: Option<EntryId>,
}

/// The session header record: the first record of every journal.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Header {
    /// The session this journal belongs to.
    pub id: SessionId,
    /// When the session was created.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub at: jiff::Timestamp,
    /// The absolute workspace the session ran in.
    pub workspace: Workspace,
    /// The product that created the journal.
    pub product: Product,
    /// The source session for a forked or cloned session.
    pub from: Option<Source>,
}

/// A text or image part in a `user` or `tool_result` record.
///
/// Blob parts name content stored under its digest; `bytes` is the
/// stored length. Inline image bytes are base64 text. Both spellings of
/// one part type share the same `"type"` literal on disk, which is why
/// the journal codec encodes parts by hand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalPart {
    /// Inline UTF-8 text.
    Text {
        /// The text content.
        text: Box<str>,
    },
    /// Text stored under its digest.
    TextBlob {
        /// The 64-hex BLAKE3 digest.
        blob: Box<str>,
        /// The stored byte length.
        bytes: u64,
    },
    /// An inline base64 image.
    Image {
        /// The image media type.
        mime: Box<str>,
        /// Base64-encoded image bytes.
        base64: Box<str>,
    },
    /// An image stored under its digest.
    ImageBlob {
        /// The image media type.
        mime: Box<str>,
        /// The 64-hex BLAKE3 digest.
        blob: Box<str>,
        /// The stored byte length.
        bytes: u64,
    },
    /// Non-text, non-image content stored under its digest, such as a
    /// PDF attachment.
    Blob {
        /// The media type.
        mime: Box<str>,
        /// The 64-hex BLAKE3 digest.
        blob: Box<str>,
        /// The stored byte length.
        bytes: u64,
    },
}

/// One assistant text, reasoning, or tool-call block in a record.
///
/// A `text` block still writes `"replay":null` in format 1; a
/// `reasoning` block writes its provider replay payload.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Assistant-visible text; `replay` is always null in format 1.
    Text {
        /// The generated text.
        text: Box<str>,
    },
    /// Reasoning text with its provider replay payload.
    Reasoning {
        /// The reasoning text.
        text: Box<str>,
        /// The opaque provider replay payload.
        replay: RawJson,
    },
    /// A request to execute a tool.
    ToolCall {
        /// The provider call identifier.
        id: CallId,
        /// The tool name.
        name: Box<str>,
        /// The raw tool arguments.
        input: RawJson,
    },
}

// Blocks and entry kinds decode through the raw tagged carrier: their
// `RawJson` members cannot pass through serde's internally tagged
// content buffering. The wire structs below mirror the `Serialize`
// derive's member names exactly.
#[derive(Deserialize)]
struct TextBlockFields {
    text: Box<str>,
}

#[derive(Deserialize)]
struct ReasoningBlockFields {
    text: Box<str>,
    replay: RawJson,
}

#[derive(Deserialize)]
struct ToolCallBlockFields {
    id: CallId,
    name: Box<str>,
    input: RawJson,
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "type", &["text", "reasoning", "tool_call"])?;
        match tagged.kind() {
            "text" => {
                let wire: TextBlockFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::Text { text: wire.text })
            }
            "reasoning" => {
                let wire: ReasoningBlockFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::Reasoning {
                    text: wire.text,
                    replay: wire.replay,
                })
            }
            "tool_call" => {
                let wire: ToolCallBlockFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::ToolCall {
                    id: wire.id,
                    name: wire.name,
                    input: wire.input,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown block type `{other}`"
            ))),
        }
    }
}

/// Why an assistant response ended, in durable journal literals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantStop {
    /// The response completed normally.
    Done,
    /// The model hit its length limit.
    Length,
    /// A provider content filter stopped the response.
    Filter,
    /// The model asked for tool execution.
    ToolUse,
    /// The response was cancelled.
    Cancelled,
    /// The response failed with a message.
    Failed {
        /// The failure message.
        message: Box<str>,
    },
}

/// Why a turn ended, in durable journal literals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEndStop {
    /// The turn completed.
    Done,
    /// The final response hit the model's length limit.
    Length,
    /// A provider content filter stopped the final response.
    Filter,
    /// The turn reached the `loop.max_steps` tool-round limit.
    MaxSteps,
    /// The user cancelled the turn.
    Cancelled,
    /// The turn aborted because the process stopped.
    Aborted,
    /// The turn failed with a message.
    Failed {
        /// The failure message.
        message: Box<str>,
    },
}

/// One tree entry's discriminator plus its payload members.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
    /// A user message.
    User {
        /// The message parts.
        parts: Vec<JournalPart>,
    },
    /// An assistant response.
    Assistant {
        /// The provider API family.
        api: Family,
        /// The model identifier.
        model: Box<str>,
        /// The response content blocks.
        content: Vec<Block>,
        /// The normalized token usage.
        usage: Usage,
        /// Why the response ended.
        stop: AssistantStop,
    },
    /// A tool call result.
    ToolResult {
        /// The call this result answers.
        call: CallId,
        /// The tool name.
        name: Box<str>,
        /// Whether the call failed.
        error: bool,
        /// The result parts.
        parts: Vec<JournalPart>,
        /// The file changes the call made.
        changes: Vec<FileChange>,
    },
    /// An injected rule or system reminder.
    Reminder {
        /// The reminder source, such as `rule:<name>`.
        source: Box<str>,
        /// The reminder text.
        text: Box<str>,
    },
    /// The active model changed.
    ///
    /// A [`ModelRoute::Api`] route journals as the format-1 `api` and
    /// `model` members; a synthetic or harness route journals as one
    /// externally tagged `route` member instead.
    Model {
        /// The route now selected.
        route: ModelRoute,
    },
    /// The thinking level changed.
    Thinking {
        /// The new level.
        level: ThinkingLevel,
    },
    /// The approval mode changed.
    Approval {
        /// The new mode.
        mode: ApprovalMode,
    },
    /// A context compaction boundary.
    Compaction {
        /// The compaction summary.
        summary: Option<Box<str>>,
        /// The first entry kept in context.
        first_kept: Option<EntryId>,
        /// Tokens present before compaction.
        tokens_before: u64,
        /// The opaque provider replay payload.
        replay: Option<RawJson>,
        /// The compaction call's usage.
        usage: Option<Usage>,
    },
    /// A summary emitted for an abandoned branch.
    BranchSummary {
        /// The entry the summary describes.
        from: EntryId,
        /// The summary text.
        summary: Box<str>,
    },
}

#[derive(Deserialize)]
struct UserEntryFields {
    parts: Vec<JournalPart>,
}

#[derive(Deserialize)]
struct AssistantEntryFields {
    api: Family,
    model: Box<str>,
    content: Vec<Block>,
    usage: Usage,
    stop: AssistantStop,
}

#[derive(Deserialize)]
struct ToolResultEntryFields {
    call: CallId,
    name: Box<str>,
    error: bool,
    parts: Vec<JournalPart>,
    changes: Vec<FileChange>,
}

#[derive(Deserialize)]
struct ReminderEntryFields {
    source: Box<str>,
    text: Box<str>,
}

#[derive(Deserialize)]
struct ModelEntryFields {
    route: ModelRoute,
}

#[derive(Deserialize)]
struct ThinkingEntryFields {
    level: ThinkingLevel,
}

#[derive(Deserialize)]
struct ApprovalEntryFields {
    mode: ApprovalMode,
}

#[derive(Deserialize)]
struct CompactionEntryFields {
    summary: Option<Box<str>>,
    first_kept: Option<EntryId>,
    tokens_before: u64,
    replay: Option<RawJson>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct BranchSummaryEntryFields {
    from: EntryId,
    summary: Box<str>,
}

impl<'de> Deserialize<'de> for EntryKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(
            deserializer,
            "type",
            &[
                "user",
                "assistant",
                "tool_result",
                "reminder",
                "model",
                "thinking",
                "approval",
                "compaction",
                "branch_summary",
            ],
        )?;
        match tagged.kind() {
            "user" => {
                let wire: UserEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::User { parts: wire.parts })
            }
            "assistant" => {
                let wire: AssistantEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                if wire.model.is_empty() {
                    return Err(serde::de::Error::custom(
                        "assistant `model` must not be empty",
                    ));
                }
                Ok(Self::Assistant {
                    api: wire.api,
                    model: wire.model,
                    content: wire.content,
                    usage: wire.usage,
                    stop: wire.stop,
                })
            }
            "tool_result" => {
                let wire: ToolResultEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::ToolResult {
                    call: wire.call,
                    name: wire.name,
                    error: wire.error,
                    parts: wire.parts,
                    changes: wire.changes,
                })
            }
            "reminder" => {
                let wire: ReminderEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::Reminder {
                    source: wire.source,
                    text: wire.text,
                })
            }
            "model" => {
                let wire: ModelEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                if matches!(&wire.route, ModelRoute::Api { model, .. } if model.is_empty()) {
                    return Err(serde::de::Error::custom(
                        "model route `model` must not be empty",
                    ));
                }
                Ok(Self::Model { route: wire.route })
            }
            "thinking" => {
                let wire: ThinkingEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::Thinking { level: wire.level })
            }
            "approval" => {
                let wire: ApprovalEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::Approval { mode: wire.mode })
            }
            "compaction" => {
                let wire: CompactionEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::Compaction {
                    summary: wire.summary,
                    first_kept: wire.first_kept,
                    tokens_before: wire.tokens_before,
                    replay: wire.replay,
                    usage: wire.usage,
                })
            }
            "branch_summary" => {
                let wire: BranchSummaryEntryFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::BranchSummary {
                    from: wire.from,
                    summary: wire.summary,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown entry kind `{other}`"
            ))),
        }
    }
}

/// One tree entry: its identity, parent, time, and kind payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Entry {
    /// The entry identifier, a per-session counter.
    pub id: EntryId,
    /// The previous entry on this branch.
    pub parent: Option<EntryId>,
    /// When the entry was journaled.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub at: jiff::Timestamp,
    /// The entry kind and payload.
    pub kind: EntryKind,
}

/// The operation owned by a durable job.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// A detached process.
    Exec,
    /// A child session.
    Child,
    /// A manual compaction request.
    Compaction,
}

/// A lifecycle event recorded against a durable job.
///
/// The `kind`, `outcome`, and `by` members are optional on disk: a
/// record written before a member existed decodes it as absent, and an
/// absent member is never re-emitted. The terminal literal is `"end"`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum JobEvent {
    /// The job started, with its kind label when present.
    Started {
        /// The job kind.
        kind: Option<Box<str>>,
    },
    /// The job reached a terminal state; its outcome when present.
    Settled {
        /// The terminal outcome.
        outcome: Option<JobOutcome>,
    },
    /// A client cancelled the job, identified when present.
    Cancelled {
        /// Who cancelled it.
        by: Option<ClientId>,
    },
    /// The job was killed.
    Killed,
    /// The job exceeded its time limit.
    TimedOut,
    /// The job outlived the process that started it.
    Orphaned,
}

/// A job's terminal outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobOutcome {
    /// The process exited with a code.
    Exited {
        /// The exit code.
        code: i32,
    },
    /// The job was cancelled.
    Cancelled,
    /// The job failed with a message.
    Failed {
        /// The failure message.
        message: Box<str>,
    },
    /// The job's outcome could not be recovered.
    Lost,
}

/// The purpose recorded for a synthetic inner inference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InferredPurpose {
    /// A synthetic-model inner call.
    Synthetic {
        /// The synthetic model identifier.
        id: Box<str>,
    },
}

/// A mailbox record written once per delivery.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Mail {
    /// When the message was delivered.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub at: jiff::Timestamp,
    /// The sending session.
    pub from: SessionId,
    /// The receiving session.
    pub to: SessionId,
    /// The delivery mode.
    pub mode: MailMode,
    /// The message text.
    pub text: Box<str>,
    /// An opaque cursor naming the message this answers.
    pub reply_to: Option<Box<str>>,
}

/// One journal record. Variants are the 31 format-1 `type` literals.
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

/// The nine tree-record tags.
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
            Self::Compaction => "compaction",
            Self::BranchSummary => "branch_summary",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool_result" => Self::ToolResult,
            "reminder" => Self::Reminder,
            "model" => Self::Model,
            "thinking" => Self::Thinking,
            "approval" => Self::Approval,
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

impl Record {
    /// The format-1 `type` literal of this record.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::Boot { .. } => "boot",
            Self::User(_) => "user",
            Self::Assistant(_) => "assistant",
            Self::ToolResult(_) => "tool_result",
            Self::Reminder(_) => "reminder",
            Self::Model(_) => "model",
            Self::Thinking(_) => "thinking",
            Self::Approval(_) => "approval",
            Self::Compaction(_) => "compaction",
            Self::BranchSummary(_) => "branch_summary",
            Self::Leaf { .. } => "leaf",
            Self::Label { .. } => "label",
            Self::Name { .. } => "name",
            Self::Archive { .. } => "archive",
            Self::TurnStart { .. } => "turn_start",
            Self::ToolStart { .. } => "tool_start",
            Self::TurnEnd { .. } => "turn_end",
            Self::RuleFired { .. } => "rule_fired",
            Self::Resolved { .. } => "resolved",
            Self::AllowAlways { .. } => "allow_always",
            Self::GrantGiven { .. } => "grant_given",
            Self::ScopedGrant { .. } => "scoped_grant",
            Self::ScopedGrantEnded { .. } => "scoped_grant_ended",
            Self::BeforeRequestMut { .. } => "before_request_mut",
            Self::ToolPromoted { .. } => "tool_promoted",
            Self::WakeAttempt { .. } => "wake_attempt",
            Self::Job { .. } => "job",
            Self::Ext { .. } => "ext",
            Self::Mail(_) => "mail",
            Self::Inferred { .. } => "inferred",
        }
    }

    /// The entry payload of a tree record, none otherwise.
    #[must_use]
    pub const fn entry(&self) -> Option<&Entry> {
        match self {
            Self::User(entry)
            | Self::Assistant(entry)
            | Self::ToolResult(entry)
            | Self::Reminder(entry)
            | Self::Model(entry)
            | Self::Thinking(entry)
            | Self::Approval(entry)
            | Self::Compaction(entry)
            | Self::BranchSummary(entry) => Some(entry),
            _ => None,
        }
    }
}

// Encode side: ordered member structs per record, hand-rolled lenses for
// the members whose durable literals differ from the wire shapes.

fn encode_json<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, EncodeError> {
    sonic_rs::to_vec(value).map_err(|error| EncodeError::Json {
        message: error.to_string().into(),
    })
}

/// Timestamps write with exactly three fractional digits. Every
/// canonical vector carries millisecond precision, and the stock
/// `jiff::Timestamp` serializer prints no fraction on integral seconds,
/// which would break the byte round-trip.
struct Millis<'a>(&'a jiff::Timestamp);

impl fmt::Display for Millis<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:.3}", self.0)
    }
}

struct TsWire<'a>(&'a jiff::Timestamp);

impl Serialize for TsWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Millis(self.0))
    }
}

/// The journal `answer` member: a bare literal or `{"value": <json>}`.
struct AnswerWire<'a>(&'a Answer);

impl Serialize for AnswerWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Answer::Approve => serializer.serialize_str("approve"),
            Answer::ApproveForSession => serializer.serialize_str("approve_for_session"),
            Answer::Decline => serializer.serialize_str("decline"),
            Answer::Cancel => serializer.serialize_str("cancel"),
            Answer::Value(value) => {
                #[derive(Serialize)]
                struct ValueMember<'a> {
                    value: &'a RawJson,
                }
                ValueMember { value }.serialize(serializer)
            }
        }
    }
}

/// The journal `who` member: `"core"` or `{"extension":{...}}`.
struct OwnerWire<'a>(&'a Owner);

impl Serialize for OwnerWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Owner::Core => serializer.serialize_str("core"),
            Owner::Extension { name, origin } => {
                #[derive(Serialize)]
                struct ExtensionMember<'a> {
                    extension: ExtensionBody<'a>,
                }
                #[derive(Serialize)]
                struct ExtensionBody<'a> {
                    name: &'a str,
                    origin: &'a str,
                }
                ExtensionMember {
                    extension: ExtensionBody {
                        name: name.as_ref(),
                        origin: origin.as_ref(),
                    },
                }
                .serialize(serializer)
            }
        }
    }
}

/// The journal `purpose` member: `{"synthetic":{"id":..}}`.
struct PurposeWire<'a>(&'a InferredPurpose);

impl Serialize for PurposeWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct PurposeMember<'a> {
            synthetic: SyntheticBody<'a>,
        }
        #[derive(Serialize)]
        struct SyntheticBody<'a> {
            id: &'a str,
        }
        let InferredPurpose::Synthetic { id } = self.0;
        PurposeMember {
            synthetic: SyntheticBody { id: id.as_ref() },
        }
        .serialize(serializer)
    }
}

/// The journal `usage` member in table order. `cost_micro_usd` carries
/// the reported dollar cost rounded to integer micro-dollars; a
/// non-finite, negative, or out-of-range cost fails the whole encode
/// instead of being written as `null` or clamped.
struct UsageWire<'a> {
    usage: &'a Usage,
    cost_micro_usd: Option<u64>,
}

impl<'a> UsageWire<'a> {
    /// Precomputes the micro-dollar member so the `Serialize` impl stays
    /// infallible and `InvalidCost` reaches `encode`'s caller unchanged.
    fn new(usage: &'a Usage) -> Result<Self, EncodeError> {
        Ok(Self {
            usage,
            cost_micro_usd: micro_cost(usage)?,
        })
    }
}

fn micro_cost(usage: &Usage) -> Result<Option<u64>, EncodeError> {
    let Some(cost) = usage.cost_usd else {
        return Ok(None);
    };
    if !cost.is_finite() || cost < 0.0 {
        return Err(EncodeError::InvalidCost);
    }
    let micros = cost.mul_add(1e6, 0.0).round();
    #[expect(
        clippy::cast_precision_loss,
        reason = "u64::MAX rounds to 2^64 as f64, the exclusive conversion bound"
    )]
    let upper_bound = u64::MAX as f64;
    if !micros.is_finite() || micros < 0.0 || micros >= upper_bound {
        return Err(EncodeError::InvalidCost);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range-checked above; the rounded value is finite, nonnegative, and below 2^64"
    )]
    Ok(Some(micros as u64))
}

impl Serialize for UsageWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct UsageMember {
            input: u64,
            output: u64,
            cache_read: u64,
            cache_write: u64,
            reasoning: Option<u64>,
            cost_micro_usd: Option<u64>,
        }
        let usage = self.usage;
        UsageMember {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cached_input_tokens,
            cache_write: usage.cache_write_tokens,
            reasoning: usage.reasoning_tokens,
            cost_micro_usd: self.cost_micro_usd,
        }
        .serialize(serializer)
    }
}

/// The `AssistantStop` member: a literal or `{"failed": "<message>"}`.
struct AssistantStopWire<'a>(&'a AssistantStop);

impl Serialize for AssistantStopWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            AssistantStop::Done => serializer.serialize_str("done"),
            AssistantStop::Length => serializer.serialize_str("length"),
            AssistantStop::Filter => serializer.serialize_str("filter"),
            AssistantStop::ToolUse => serializer.serialize_str("tool_use"),
            AssistantStop::Cancelled => serializer.serialize_str("cancelled"),
            AssistantStop::Failed { message } => {
                #[derive(Serialize)]
                struct FailedMember<'a> {
                    failed: &'a str,
                }
                FailedMember {
                    failed: message.as_ref(),
                }
                .serialize(serializer)
            }
        }
    }
}

/// The `TurnEndStop` member: a literal or `{"failed": "<message>"}`.
struct TurnEndStopWire<'a>(&'a TurnEndStop);

impl Serialize for TurnEndStopWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            TurnEndStop::Done => serializer.serialize_str("done"),
            TurnEndStop::Length => serializer.serialize_str("length"),
            TurnEndStop::Filter => serializer.serialize_str("filter"),
            TurnEndStop::MaxSteps => serializer.serialize_str("max_steps"),
            TurnEndStop::Cancelled => serializer.serialize_str("cancelled"),
            TurnEndStop::Aborted => serializer.serialize_str("aborted"),
            TurnEndStop::Failed { message } => {
                #[derive(Serialize)]
                struct FailedMember<'a> {
                    failed: &'a str,
                }
                FailedMember {
                    failed: message.as_ref(),
                }
                .serialize(serializer)
            }
        }
    }
}

/// The `JobOutcome` member: a literal or `{"<variant>": <payload>}`.
struct JobOutcomeWire<'a>(&'a JobOutcome);

impl Serialize for JobOutcomeWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            JobOutcome::Exited { code } => {
                #[derive(Serialize)]
                struct ExitedMember {
                    exited: i32,
                }
                ExitedMember { exited: *code }.serialize(serializer)
            }
            JobOutcome::Cancelled => serializer.serialize_str("cancelled"),
            JobOutcome::Failed { message } => {
                #[derive(Serialize)]
                struct FailedMember<'a> {
                    failed: &'a str,
                }
                FailedMember {
                    failed: message.as_ref(),
                }
                .serialize(serializer)
            }
            JobOutcome::Lost => serializer.serialize_str("lost"),
        }
    }
}

struct JournalPartsWire<'a>(&'a [JournalPart]);

impl Serialize for JournalPartsWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for part in self.0 {
            seq.serialize_element(&JournalPartWire(part))?;
        }
        seq.end()
    }
}

struct JournalPartWire<'a>(&'a JournalPart);

impl Serialize for JournalPartWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            JournalPart::Text { text } => {
                #[derive(Serialize)]
                struct TextMember<'a> {
                    r#type: &'static str,
                    text: &'a str,
                }
                TextMember {
                    r#type: "text",
                    text: text.as_ref(),
                }
                .serialize(serializer)
            }
            JournalPart::TextBlob { blob, bytes } => {
                #[derive(Serialize)]
                struct TextBlobMember<'a> {
                    r#type: &'static str,
                    blob: &'a str,
                    bytes: u64,
                }
                TextBlobMember {
                    r#type: "text",
                    blob: blob.as_ref(),
                    bytes: *bytes,
                }
                .serialize(serializer)
            }
            JournalPart::Image { mime, base64 } => {
                #[derive(Serialize)]
                struct ImageMember<'a> {
                    r#type: &'static str,
                    mime: &'a str,
                    base64: &'a str,
                }
                ImageMember {
                    r#type: "image",
                    mime: mime.as_ref(),
                    base64: base64.as_ref(),
                }
                .serialize(serializer)
            }
            JournalPart::ImageBlob { mime, blob, bytes } => {
                StoredBlobMember::new("image", mime, blob, *bytes).serialize(serializer)
            }
            JournalPart::Blob { mime, blob, bytes } => {
                StoredBlobMember::new("blob", mime, blob, *bytes).serialize(serializer)
            }
        }
    }
}

/// The shared member order of the digest-backed parts that carry a MIME
/// type: `image` blobs and generic `blob` parts.
#[derive(Serialize)]
struct StoredBlobMember<'a> {
    r#type: &'static str,
    mime: &'a str,
    blob: &'a str,
    bytes: u64,
}

impl<'a> StoredBlobMember<'a> {
    const fn new(r#type: &'static str, mime: &'a str, blob: &'a str, bytes: u64) -> Self {
        Self {
            r#type,
            mime,
            blob,
            bytes,
        }
    }
}

struct AssistantBlocksWire<'a>(&'a [Block]);

impl Serialize for AssistantBlocksWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for block in self.0 {
            seq.serialize_element(&BlockWire(block))?;
        }
        seq.end()
    }
}

struct BlockWire<'a>(&'a Block);

impl Serialize for BlockWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Block::Text { text } => {
                #[derive(Serialize)]
                struct TextMember<'a> {
                    r#type: &'static str,
                    text: &'a str,
                    replay: Option<()>,
                }
                TextMember {
                    r#type: "text",
                    text: text.as_ref(),
                    replay: None,
                }
                .serialize(serializer)
            }
            Block::Reasoning { text, replay } => {
                #[derive(Serialize)]
                struct ReasoningMember<'a> {
                    r#type: &'static str,
                    text: &'a str,
                    replay: &'a RawJson,
                }
                ReasoningMember {
                    r#type: "reasoning",
                    text: text.as_ref(),
                    replay,
                }
                .serialize(serializer)
            }
            Block::ToolCall { id, name, input } => {
                #[derive(Serialize)]
                struct ToolCallMember<'a> {
                    r#type: &'static str,
                    id: &'a CallId,
                    name: &'a str,
                    input: &'a RawJson,
                }
                ToolCallMember {
                    r#type: "tool_call",
                    id,
                    name: name.as_ref(),
                    input,
                }
                .serialize(serializer)
            }
        }
    }
}

// Ordered member structs; serde writes fields in declaration order, so
// each matches the store's format-1 member table exactly.

#[derive(Serialize)]
struct SessionWire<'a> {
    r#type: &'static str,
    id: &'a SessionId,
    at: TsWire<'a>,
    workspace: &'a Workspace,
    product: &'a Product,
    from: Option<SourceWire>,
}

/// The `from` member: `null` or `{"session":"<id>","entry":<int or null>}`.
#[derive(Serialize)]
struct SourceWire {
    session: SessionId,
    entry: Option<EntryId>,
}

#[derive(Serialize)]
struct BootWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    r#gen: Gen,
    version: &'a str,
}

#[derive(Serialize)]
struct UserWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    parts: JournalPartsWire<'a>,
}

#[derive(Serialize)]
struct AssistantWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    api: Family,
    model: &'a str,
    content: AssistantBlocksWire<'a>,
    usage: UsageWire<'a>,
    stop: AssistantStopWire<'a>,
}

#[derive(Serialize)]
struct ToolResultWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    call: &'a CallId,
    name: &'a str,
    error: bool,
    parts: JournalPartsWire<'a>,
    changes: &'a [FileChange],
}

#[derive(Serialize)]
struct ReminderWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    source: &'a str,
    text: &'a str,
}

#[derive(Serialize)]
struct ModelWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    api: Family,
    model: &'a str,
}

#[derive(Serialize)]
struct ModelRouteWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    route: RouteWire<'a>,
}

/// The journal `route` member of a non-API model record:
/// `{"synthetic":{"id":..}}` or `{"harness":{"id":..}}`. API routes keep
/// the format-1 `api` and `model` members, so no family is invented here.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum RouteWire<'a> {
    Synthetic { id: &'a str },
    Harness { id: &'a str },
}

#[derive(Serialize)]
struct ThinkingWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    level: ThinkingLevel,
}

#[derive(Serialize)]
struct ApprovalWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    mode: ModeWire<'a>,
}

/// The `mode` member's journal literals: `ask`, `auto-edit`, `yolo`.
///
/// `config::ApprovalMode` spells the same three states as `ask`,
/// `edits`, and `all` for its own surfaces, so the journal lens maps
/// rather than delegates.
struct ModeWire<'a>(&'a ApprovalMode);

impl Serialize for ModeWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self.0 {
            ApprovalMode::Ask => "ask",
            ApprovalMode::Edits => "auto-edit",
            ApprovalMode::All => "yolo",
        })
    }
}

#[derive(Serialize)]
struct CompactionWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    summary: &'a Option<Box<str>>,
    first_kept: Option<EntryId>,
    tokens_before: u64,
    replay: &'a Option<RawJson>,
    usage: Option<UsageWire<'a>>,
}

#[derive(Serialize)]
struct BranchSummaryWire<'a> {
    r#type: &'static str,
    id: EntryId,
    parent: Option<EntryId>,
    at: TsWire<'a>,
    from: EntryId,
    summary: &'a str,
}

#[derive(Serialize)]
struct LeafWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    to: &'a Option<EntryId>,
}

#[derive(Serialize)]
struct LabelWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    entry: EntryId,
    label: &'a Option<Box<str>>,
}

#[derive(Serialize)]
struct NameWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    name: &'a Option<Box<str>>,
}

#[derive(Serialize)]
struct ArchiveWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    archived: bool,
}

#[derive(Serialize)]
struct TurnStartWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    turn: TurnId,
}

#[derive(Serialize)]
struct ToolStartWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    turn: TurnId,
    call: &'a CallId,
}

#[derive(Serialize)]
struct TurnEndWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    turn: TurnId,
    stop: TurnEndStopWire<'a>,
    usage: Option<UsageWire<'a>>,
    changes: &'a [FileChange],
}

#[derive(Serialize)]
struct RuleFiredWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    turn: TurnId,
    rule: &'a str,
    entry: EntryId,
}

#[derive(Serialize)]
struct ResolvedWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    request: &'a RequestId,
    answer: AnswerWire<'a>,
    by: &'a ClientId,
    #[serde(skip_serializing_if = "is_false")]
    was_default: bool,
}

/// The `was_default` member appears only when it is true.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if predicates take references"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
struct AllowAlwaysWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    tool: &'a str,
    by: &'a ClientId,
}

#[derive(Serialize)]
struct GrantGivenWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    ext: &'a str,
    set: &'a [Box<str>],
    scope: &'a str,
    by: &'a ClientId,
}

#[derive(Serialize)]
struct ScopedGrantWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    call: &'a CallId,
    prefix: &'a [Box<str>],
    roots: &'a [Box<str>],
    job: &'a JobId,
    by: &'a ClientId,
}

#[derive(Serialize)]
struct ScopedGrantEndedWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    job: &'a JobId,
}

#[derive(Serialize)]
struct BeforeRequestMutWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    turn: TurnId,
    ext: &'a str,
    field: &'a str,
    old: &'a str,
    new: &'a str,
}

#[derive(Serialize)]
struct ToolPromotedWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    tool: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn: Option<TurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    leaf: Option<EntryId>,
}

#[derive(Serialize)]
struct WakeAttemptWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    turn: TurnId,
    count: u32,
}

#[derive(Serialize)]
struct JobWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    job: &'a JobId,
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<JobOutcomeWire<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    by: Option<&'a ClientId>,
}

#[derive(Serialize)]
struct ExtWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    ext: &'a str,
    kind: &'a str,
    body: &'a RawJson,
}

#[derive(Serialize)]
struct MailRecordWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    from: &'a SessionId,
    to: &'a SessionId,
    mode: MailMode,
    text: &'a str,
    reply_to: &'a Option<Box<str>>,
}

#[derive(Serialize)]
struct InferredWire<'a> {
    r#type: &'static str,
    at: TsWire<'a>,
    who: OwnerWire<'a>,
    purpose: PurposeWire<'a>,
    usage: UsageWire<'a>,
}

/// Writes one record as a compact line: `"v":1` first, then `"type"`,
/// then the record's declared member order, then a single LF.
///
/// # Errors
/// Returns [`EncodeError::Json`] when a member fails to serialize,
/// [`EncodeError::InvalidCost`] when a usage member carries a reported
/// cost no integer micro-dollar count can hold,
/// [`EncodeError::InvalidRoute`] when a model record's synthetic or
/// harness id breaks its grammar, and [`EncodeError::MismatchedKind`]
/// when a tree record's variant does not match its entry kind.
pub fn encode(record: &Record) -> Result<Vec<u8>, EncodeError> {
    let body = encode_body(record)?;
    let mut line = Vec::with_capacity(body.len() + 8);
    line.extend_from_slice(b"{\"v\":1,");
    line.extend_from_slice(&body[1..]);
    line.push(b'\n');
    Ok(line)
}

#[expect(
    clippy::too_many_lines,
    reason = "one match arm per record kind keeps the wire table readable"
)]
fn encode_body(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::Session(header) => encode_json(&SessionWire {
            r#type: "session",
            id: &header.id,
            at: TsWire(&header.at),
            workspace: &header.workspace,
            product: &header.product,
            from: header.from.map(|source| SourceWire {
                session: source.session,
                entry: source.entry,
            }),
        }),
        Record::Boot { at, r#gen, version } => encode_json(&BootWire {
            r#type: "boot",
            at: TsWire(at),
            r#gen: *r#gen,
            version: version.as_ref(),
        }),
        Record::User(entry) => {
            let EntryKind::User { parts } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&UserWire {
                r#type: "user",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                parts: JournalPartsWire(parts),
            })
        }
        Record::Assistant(entry) => {
            let EntryKind::Assistant {
                api,
                model,
                content,
                usage,
                stop,
            } = &entry.kind
            else {
                return Err(EncodeError::MismatchedKind);
            };
            if model.is_empty() {
                return Err(EncodeError::EmptyModel);
            }
            encode_json(&AssistantWire {
                r#type: "assistant",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                api: *api,
                model: model.as_ref(),
                content: AssistantBlocksWire(content),
                usage: UsageWire::new(usage)?,
                stop: AssistantStopWire(stop),
            })
        }
        Record::ToolResult(entry) => {
            let EntryKind::ToolResult {
                call,
                name,
                error,
                parts,
                changes,
            } = &entry.kind
            else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ToolResultWire {
                r#type: "tool_result",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                call,
                name: name.as_ref(),
                error: *error,
                parts: JournalPartsWire(parts),
                changes,
            })
        }
        Record::Reminder(entry) => {
            let EntryKind::Reminder { source, text } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ReminderWire {
                r#type: "reminder",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                source: source.as_ref(),
                text: text.as_ref(),
            })
        }
        Record::Model(entry) => {
            let EntryKind::Model { route } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_model(entry, route)
        }
        Record::Thinking(entry) => {
            let EntryKind::Thinking { level } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ThinkingWire {
                r#type: "thinking",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                level: *level,
            })
        }
        Record::Approval(entry) => {
            let EntryKind::Approval { mode } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ApprovalWire {
                r#type: "approval",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                mode: ModeWire(mode),
            })
        }
        Record::Compaction(entry) => {
            let EntryKind::Compaction {
                summary,
                first_kept,
                tokens_before,
                replay,
                usage,
            } = &entry.kind
            else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&CompactionWire {
                r#type: "compaction",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                summary,
                first_kept: *first_kept,
                tokens_before: *tokens_before,
                replay,
                usage: usage.as_ref().map(UsageWire::new).transpose()?,
            })
        }
        Record::BranchSummary(entry) => {
            let EntryKind::BranchSummary { from, summary } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&BranchSummaryWire {
                r#type: "branch_summary",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                from: *from,
                summary: summary.as_ref(),
            })
        }
        Record::Leaf { at, to } => encode_json(&LeafWire {
            r#type: "leaf",
            at: TsWire(at),
            to,
        }),
        Record::Label { at, entry, label } => encode_json(&LabelWire {
            r#type: "label",
            at: TsWire(at),
            entry: *entry,
            label,
        }),
        Record::Name { at, name } => encode_json(&NameWire {
            r#type: "name",
            at: TsWire(at),
            name,
        }),
        Record::Archive { at, archived } => encode_json(&ArchiveWire {
            r#type: "archive",
            at: TsWire(at),
            archived: *archived,
        }),
        Record::TurnStart { at, turn } => encode_json(&TurnStartWire {
            r#type: "turn_start",
            at: TsWire(at),
            turn: *turn,
        }),
        Record::ToolStart { at, turn, call } => encode_json(&ToolStartWire {
            r#type: "tool_start",
            at: TsWire(at),
            turn: *turn,
            call,
        }),
        Record::TurnEnd {
            at,
            turn,
            stop,
            usage,
            changes,
        } => encode_json(&TurnEndWire {
            r#type: "turn_end",
            at: TsWire(at),
            turn: *turn,
            stop: TurnEndStopWire(stop),
            usage: usage.as_ref().map(UsageWire::new).transpose()?,
            changes,
        }),
        Record::RuleFired {
            at,
            turn,
            rule,
            entry,
        } => encode_json(&RuleFiredWire {
            r#type: "rule_fired",
            at: TsWire(at),
            turn: *turn,
            rule: rule.as_ref(),
            entry: *entry,
        }),
        Record::Resolved {
            at,
            request,
            answer,
            by,
            was_default,
        } => encode_json(&ResolvedWire {
            r#type: "resolved",
            at: TsWire(at),
            request,
            answer: AnswerWire(answer),
            by,
            was_default: *was_default,
        }),
        Record::AllowAlways { at, tool, by } => encode_json(&AllowAlwaysWire {
            r#type: "allow_always",
            at: TsWire(at),
            tool: tool.as_ref(),
            by,
        }),
        Record::GrantGiven {
            at,
            ext,
            set,
            scope,
            by,
        } => encode_json(&GrantGivenWire {
            r#type: "grant_given",
            at: TsWire(at),
            ext: ext.as_ref(),
            set,
            scope: scope.as_ref(),
            by,
        }),
        Record::ScopedGrant {
            at,
            call,
            prefix,
            roots,
            job,
            by,
        } => encode_json(&ScopedGrantWire {
            r#type: "scoped_grant",
            at: TsWire(at),
            call,
            prefix,
            roots,
            job,
            by,
        }),
        Record::ScopedGrantEnded { at, job } => encode_json(&ScopedGrantEndedWire {
            r#type: "scoped_grant_ended",
            at: TsWire(at),
            job,
        }),
        Record::BeforeRequestMut {
            at,
            turn,
            ext,
            field,
            old,
            new,
        } => encode_json(&BeforeRequestMutWire {
            r#type: "before_request_mut",
            at: TsWire(at),
            turn: *turn,
            ext: ext.as_ref(),
            field: field.as_ref(),
            old: old.as_ref(),
            new: new.as_ref(),
        }),
        Record::ToolPromoted {
            at,
            tool,
            turn,
            leaf,
        } => encode_json(&ToolPromotedWire {
            r#type: "tool_promoted",
            at: TsWire(at),
            tool: tool.as_ref(),
            turn: *turn,
            leaf: *leaf,
        }),
        Record::WakeAttempt { at, turn, count } => encode_json(&WakeAttemptWire {
            r#type: "wake_attempt",
            at: TsWire(at),
            turn: *turn,
            count: *count,
        }),
        Record::Job { at, job, event } => {
            let (literal, kind, outcome, by) = match event {
                JobEvent::Started { kind } => ("start", kind.as_deref(), None, None),
                JobEvent::Settled { outcome } => {
                    ("end", None, outcome.as_ref().map(JobOutcomeWire), None)
                }
                JobEvent::Cancelled { by } => ("cancelled", None, None, by.as_ref()),
                JobEvent::Killed => ("killed", None, None, None),
                JobEvent::TimedOut => ("timed_out", None, None, None),
                JobEvent::Orphaned => ("orphaned", None, None, None),
            };
            encode_json(&JobWire {
                r#type: "job",
                at: TsWire(at),
                job,
                event: literal,
                kind,
                outcome,
                by,
            })
        }
        Record::Ext {
            at,
            ext,
            kind,
            body,
        } => encode_json(&ExtWire {
            r#type: "ext",
            at: TsWire(at),
            ext: ext.as_ref(),
            kind: kind.as_ref(),
            body,
        }),
        Record::Mail(mail) => encode_json(&MailRecordWire {
            r#type: "mail",
            at: TsWire(&mail.at),
            from: &mail.from,
            to: &mail.to,
            mode: mail.mode,
            text: mail.text.as_ref(),
            reply_to: &mail.reply_to,
        }),
        Record::Inferred {
            at,
            who,
            purpose,
            usage,
        } => encode_json(&InferredWire {
            r#type: "inferred",
            at: TsWire(at),
            who: OwnerWire(who),
            purpose: PurposeWire(purpose),
            usage: UsageWire::new(usage)?,
        }),
    }
}

/// Writes a model record. An API route keeps the byte-exact format-1
/// `api`, `model` members; a synthetic or harness route writes one
/// `route` member after its id passes the same grammar the decoder checks.
fn encode_model(entry: &Entry, route: &ModelRoute) -> Result<Vec<u8>, EncodeError> {
    let route = match route {
        ModelRoute::Api { model, .. } if model.is_empty() => {
            return Err(EncodeError::EmptyModel);
        }
        ModelRoute::Api { family, model } => {
            return encode_json(&ModelWire {
                r#type: "model",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                api: *family,
                model: model.as_ref(),
            });
        }
        ModelRoute::Synthetic { id } => {
            if !ModelRoute::is_valid_synthetic_id(id) {
                return Err(EncodeError::InvalidRoute(RouteError::InvalidSyntheticId {
                    id: id.clone(),
                }));
            }
            RouteWire::Synthetic { id }
        }
        ModelRoute::Harness { id } => {
            ModelRoute::harness(&**id).map_err(EncodeError::InvalidRoute)?;
            RouteWire::Harness { id }
        }
    };
    encode_json(&ModelRouteWire {
        r#type: "model",
        id: entry.id,
        parent: entry.parent,
        at: TsWire(&entry.at),
        route,
    })
}

// Decode side: one member scan, `"v"` before `"type"`. Record members a
// format-1 record does not declare are dropped; the closed nested
// payloads (`usage`, parts, blocks, `who`, `purpose`, `answer`, `from`,
// `outcome`, `stop`, `route`) are scanned member by member and stay
// strict.

/// One raw member map: member name to byte offset and its lazy value.
/// The lazy value must be stored whole: its accessors borrow `self`, so
/// the raw member text lives only as long as the value does.
type Members<'a> = BTreeMap<Box<str>, (usize, LazyValue<'a>)>;

fn invalid(offset: usize, message: impl Into<Box<str>>) -> DecodeError {
    DecodeError::Invalid {
        offset,
        message: message.into(),
    }
}

fn need<'a>(
    members: &mut Members<'a>,
    name: &'static str,
) -> Result<(usize, LazyValue<'a>), DecodeError> {
    members
        .remove(name)
        .ok_or_else(|| invalid(0, format!("missing member `{name}`")))
}

fn want<'a>(members: &mut Members<'a>, name: &'static str) -> Option<(usize, LazyValue<'a>)> {
    members.remove(name)
}

fn json_member<T: for<'de> Deserialize<'de>>(
    member: (usize, LazyValue<'_>),
) -> Result<T, DecodeError> {
    let (offset, raw) = member;
    sonic_rs::from_str(raw.as_raw_str()).map_err(|error| invalid(offset, error.to_string()))
}

fn text_member(member: (usize, LazyValue<'_>), name: &str) -> Result<Box<str>, DecodeError> {
    let (offset, raw) = member;
    raw.as_str()
        .map(Into::into)
        .ok_or_else(|| invalid(offset, format!("member `{name}` must be a string")))
}

fn bool_member(member: (usize, LazyValue<'_>), name: &str) -> Result<bool, DecodeError> {
    let (offset, raw) = member;
    raw.as_bool()
        .ok_or_else(|| invalid(offset, format!("member `{name}` must be a boolean")))
}

fn u64_member(member: (usize, LazyValue<'_>), name: &str) -> Result<u64, DecodeError> {
    let (offset, raw) = member;
    raw.as_u64().ok_or_else(|| {
        invalid(
            offset,
            format!("member `{name}` must be an unsigned integer"),
        )
    })
}

fn i32_member(member: (usize, LazyValue<'_>), name: &str) -> Result<i32, DecodeError> {
    let (offset, raw) = member;
    raw.as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid(offset, format!("member `{name}` must be an i32")))
}

fn is_null(member: &(usize, LazyValue<'_>)) -> bool {
    member.1.is_null()
}

fn entry_id_member(member: (usize, LazyValue<'_>), name: &str) -> Result<EntryId, DecodeError> {
    let (offset, raw) = member;
    let value = u64_member((offset, raw), name)?;
    NonZeroU64::new(value).map(EntryId::new).ok_or_else(|| {
        invalid(
            offset,
            format!("member `{name}` must be a positive integer"),
        )
    })
}

fn opt_entry_id_member(
    member: (usize, LazyValue<'_>),
    name: &str,
) -> Result<Option<EntryId>, DecodeError> {
    if is_null(&member) {
        Ok(None)
    } else {
        entry_id_member(member, name).map(Some)
    }
}

fn opt_text_member(
    member: (usize, LazyValue<'_>),
    name: &str,
) -> Result<Option<Box<str>>, DecodeError> {
    if is_null(&member) {
        Ok(None)
    } else {
        text_member(member, name).map(Some)
    }
}

fn opt_raw_json_member(
    member: (usize, LazyValue<'_>),
    name: &str,
) -> Result<Option<RawJson>, DecodeError> {
    let (offset, raw) = member;
    if raw.is_null() {
        Ok(None)
    } else {
        RawJson::parse(raw.as_raw_str())
            .map(Some)
            .map_err(|error| invalid(offset, format!("member `{name}`: {error}")))
    }
}

/// Collects a raw object into the member map. `seen` names the members
/// this object accepts; any other member is rejected at its offset.
fn object_members<'a>(
    raw: &'a str,
    base_offset: usize,
    seen: &[&str],
) -> Result<Members<'a>, DecodeError> {
    let mut members = Members::new();
    for member in sonic_rs::to_object_iter(raw) {
        let (name, value) = member.map_err(|error| invalid(base_offset, error.to_string()))?;
        let offset = base_offset + (value.as_raw_str().as_ptr() as usize - raw.as_ptr() as usize);
        if !seen.contains(&name.as_ref()) {
            return Err(invalid(offset, format!("unknown member `{name}`")));
        }
        if members.contains_key(name.as_ref()) {
            return Err(invalid(offset, format!("duplicate member `{name}`")));
        }
        members.insert(name.as_ref().into(), (offset, value));
    }
    Ok(members)
}

fn parts_member(member: (usize, LazyValue<'_>)) -> Result<Vec<JournalPart>, DecodeError> {
    let (offset, raw) = member;
    let mut parts = Vec::new();
    for item in sonic_rs::to_array_iter(raw.as_raw_str()) {
        let item = item.map_err(|error| invalid(offset, error.to_string()))?;
        let item_offset =
            offset + (item.as_raw_str().as_ptr() as usize - raw.as_raw_str().as_ptr() as usize);
        parts.push(decode_part(&item, item_offset)?);
    }
    Ok(parts)
}

fn decode_part(value: &LazyValue<'_>, offset: usize) -> Result<JournalPart, DecodeError> {
    let mut members = object_members(
        value.as_raw_str(),
        offset,
        &["type", "text", "mime", "base64", "blob", "bytes"],
    )?;
    let ty = want(&mut members, "type")
        .map(|member| text_member(member, "type"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a part needs a `type` member"))?;
    let has_blob = members.contains_key("blob");
    let text = want(&mut members, "text")
        .map(|member| text_member(member, "text"))
        .transpose()?;
    let mime = want(&mut members, "mime")
        .map(|member| text_member(member, "mime"))
        .transpose()?;
    let base64 = want(&mut members, "base64")
        .map(|member| text_member(member, "base64"))
        .transpose()?;
    let blob = want(&mut members, "blob")
        .map(|member| text_member(member, "blob"))
        .transpose()?;
    let bytes = want(&mut members, "bytes")
        .map(|member| u64_member(member, "bytes"))
        .transpose()?;
    match (ty.as_ref(), has_blob) {
        ("text", false) => Ok(JournalPart::Text {
            text: text.ok_or_else(|| invalid(offset, "a text part needs `text`"))?,
        }),
        ("text", true) => Ok(JournalPart::TextBlob {
            blob: blob.ok_or_else(|| invalid(offset, "a text blob part needs `blob`"))?,
            bytes: bytes.ok_or_else(|| invalid(offset, "a text blob part needs `bytes`"))?,
        }),
        ("image", false) => Ok(JournalPart::Image {
            mime: mime.ok_or_else(|| invalid(offset, "an image part needs `mime`"))?,
            base64: base64.ok_or_else(|| invalid(offset, "an image part needs `base64`"))?,
        }),
        ("image", true) => Ok(JournalPart::ImageBlob {
            mime: mime.ok_or_else(|| invalid(offset, "an image blob part needs `mime`"))?,
            blob: blob.ok_or_else(|| invalid(offset, "an image blob part needs `blob`"))?,
            bytes: bytes.ok_or_else(|| invalid(offset, "an image blob part needs `bytes`"))?,
        }),
        ("blob", _) if text.is_some() || base64.is_some() => Err(invalid(
            offset,
            "a blob part accepts only `mime`, `blob`, and `bytes`",
        )),
        ("blob", _) => Ok(JournalPart::Blob {
            mime: mime.ok_or_else(|| invalid(offset, "a blob part needs `mime`"))?,
            blob: blob.ok_or_else(|| invalid(offset, "a blob part needs `blob`"))?,
            bytes: bytes.ok_or_else(|| invalid(offset, "a blob part needs `bytes`"))?,
        }),
        (kind, _) => Err(invalid(offset, format!("unknown part type `{kind}`"))),
    }
}

fn blocks_member(member: (usize, LazyValue<'_>)) -> Result<Vec<Block>, DecodeError> {
    let (offset, raw) = member;
    let mut blocks = Vec::new();
    for item in sonic_rs::to_array_iter(raw.as_raw_str()) {
        let item = item.map_err(|error| invalid(offset, error.to_string()))?;
        let item_offset =
            offset + (item.as_raw_str().as_ptr() as usize - raw.as_raw_str().as_ptr() as usize);
        blocks.push(decode_block(&item, item_offset)?);
    }
    Ok(blocks)
}

fn decode_block(value: &LazyValue<'_>, offset: usize) -> Result<Block, DecodeError> {
    let mut members = object_members(
        value.as_raw_str(),
        offset,
        &["type", "text", "replay", "id", "name", "input"],
    )?;
    let ty = want(&mut members, "type")
        .map(|member| text_member(member, "type"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a content block needs a `type` member"))?;
    let text = want(&mut members, "text")
        .map(|member| text_member(member, "text"))
        .transpose()?;
    let replay = want(&mut members, "replay")
        .map(|member| opt_raw_json_member(member, "replay"))
        .transpose()?
        .flatten();
    match ty.as_ref() {
        "text" => Ok(Block::Text {
            text: text.ok_or_else(|| invalid(offset, "a text block needs `text`"))?,
        }),
        "reasoning" => Ok(Block::Reasoning {
            text: text.ok_or_else(|| invalid(offset, "a reasoning block needs `text`"))?,
            replay: replay.ok_or_else(|| invalid(offset, "a reasoning block needs `replay`"))?,
        }),
        "tool_call" => {
            let id = want(&mut members, "id")
                .map(|member| text_member(member, "id").map(CallId::new))
                .transpose()?
                .ok_or_else(|| invalid(offset, "a tool_call block needs `id`"))?;
            let name = want(&mut members, "name")
                .map(|member| text_member(member, "name"))
                .transpose()?
                .ok_or_else(|| invalid(offset, "a tool_call block needs `name`"))?;
            let input = want(&mut members, "input")
                .map(|member| {
                    RawJson::parse(member.1.as_raw_str())
                        .map_err(|error| invalid(member.0, error.to_string()))
                })
                .transpose()?
                .ok_or_else(|| invalid(offset, "a tool_call block needs `input`"))?;
            Ok(Block::ToolCall { id, name, input })
        }
        kind => Err(invalid(
            offset,
            format!("unknown content block type `{kind}`"),
        )),
    }
}

fn usage_member(member: (usize, LazyValue<'_>)) -> Result<Option<Usage>, DecodeError> {
    if is_null(&member) {
        return Ok(None);
    }
    let (offset, raw) = member;
    let mut members = object_members(
        raw.as_raw_str(),
        offset,
        &[
            "input",
            "output",
            "cache_read",
            "cache_write",
            "reasoning",
            "cost_micro_usd",
        ],
    )?;
    let missing = |name: &'static str| invalid(offset, format!("usage member `{name}` is missing"));
    let input = want(&mut members, "input")
        .map(|member| u64_member(member, "input"))
        .transpose()?
        .ok_or_else(|| missing("input"))?;
    let output = want(&mut members, "output")
        .map(|member| u64_member(member, "output"))
        .transpose()?
        .ok_or_else(|| missing("output"))?;
    let cache_read = want(&mut members, "cache_read")
        .map(|member| u64_member(member, "cache_read"))
        .transpose()?
        .ok_or_else(|| missing("cache_read"))?;
    let cache_write = want(&mut members, "cache_write")
        .map(|member| u64_member(member, "cache_write"))
        .transpose()?
        .ok_or_else(|| missing("cache_write"))?;
    let reasoning = match want(&mut members, "reasoning") {
        Some(member) if is_null(&member) => None,
        Some(member) => Some(u64_member(member, "reasoning")?),
        None => return Err(missing("reasoning")),
    };
    let cost_usd = match want(&mut members, "cost_micro_usd") {
        Some(member) if is_null(&member) => None,
        Some(member) => {
            let micros = u64_member(member, "cost_micro_usd")?;
            // Integer micro-dollars to f64 dollars; values above 2^53
            // micro-dollars lose exactness in the float model.
            #[expect(
                clippy::cast_precision_loss,
                reason = "the float cost model is pinned by the wire surface; the journal stores integer micros"
            )]
            Some(micros as f64 / 1e6)
        }
        None => return Err(missing("cost_micro_usd")),
    };
    Ok(Some(Usage {
        input_tokens: input,
        cached_input_tokens: cache_read,
        output_tokens: output,
        reasoning_tokens: reasoning,
        cache_write_tokens: cache_write,
        cost_usd,
    }))
}

fn owner_member(member: (usize, LazyValue<'_>)) -> Result<Owner, DecodeError> {
    let (offset, raw) = member;
    if let Some(literal) = raw.as_str() {
        return match literal {
            "core" => Ok(Owner::Core),
            other => Err(invalid(offset, format!("unknown owner literal `{other}`"))),
        };
    }
    let mut inner = object_members(raw.as_raw_str(), offset, &["extension"])?;
    let extension = want(&mut inner, "extension")
        .ok_or_else(|| invalid(offset, "a `who` object needs an `extension` member"))?;
    let mut fields = object_members(extension.1.as_raw_str(), extension.0, &["name", "origin"])?;
    let name = want(&mut fields, "name")
        .map(|member| text_member(member, "name"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "who.extension needs `name`"))?;
    let origin = want(&mut fields, "origin")
        .map(|member| text_member(member, "origin"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "who.extension needs `origin`"))?;
    Ok(Owner::Extension { name, origin })
}

fn purpose_member(member: (usize, LazyValue<'_>)) -> Result<InferredPurpose, DecodeError> {
    let (offset, raw) = member;
    let mut outer = object_members(raw.as_raw_str(), offset, &["synthetic"])?;
    let synthetic = want(&mut outer, "synthetic")
        .ok_or_else(|| invalid(offset, "a `purpose` object needs a `synthetic` member"))?;
    let mut fields = object_members(synthetic.1.as_raw_str(), synthetic.0, &["id"])?;
    let id = want(&mut fields, "id")
        .map(|member| text_member(member, "id"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "purpose.synthetic needs `id`"))?;
    Ok(InferredPurpose::Synthetic { id })
}

/// Reads a model record's route: either the format-1 `api` and `model`
/// pair or one `route` member, never both.
fn model_route_members(members: &mut Members<'_>) -> Result<ModelRoute, DecodeError> {
    let route = want(members, "route");
    let api = want(members, "api");
    let model = want(members, "model");
    match (route, api, model) {
        (None, Some(api), Some(model)) => Ok(ModelRoute::Api {
            family: json_member(api)?,
            model: model_id_member(model)?,
        }),
        (Some(route), None, None) => route_member(route),
        (Some((offset, _)), _, _) => Err(invalid(
            offset,
            "member `route` cannot appear with `api` or `model`",
        )),
        (None, None, _) => Err(invalid(0, "missing member `api`")),
        (None, Some(_), None) => Err(invalid(0, "missing member `model`")),
    }
}

/// Reads a `model` id member; an empty id cannot name a provider model.
fn model_id_member(member: (usize, LazyValue<'_>)) -> Result<Box<str>, DecodeError> {
    let offset = member.0;
    let model = text_member(member, "model")?;
    if model.is_empty() {
        return Err(invalid(offset, "member `model` must not be empty"));
    }
    Ok(model)
}

/// Reads `{"synthetic":{"id":..}}` or `{"harness":{"id":..}}` and checks
/// the id against the route's closed grammar.
fn route_member(member: (usize, LazyValue<'_>)) -> Result<ModelRoute, DecodeError> {
    let (offset, raw) = member;
    let outer = object_members(raw.as_raw_str(), offset, &["synthetic", "harness"])?;
    let mut tags = outer.into_iter();
    let (Some((tag, (body_offset, body))), None) = (tags.next(), tags.next()) else {
        return Err(invalid(
            offset,
            "a `route` object needs exactly one `synthetic` or `harness` member",
        ));
    };
    let mut fields = object_members(body.as_raw_str(), body_offset, &["id"])?;
    let (id_offset, id_raw) = want(&mut fields, "id")
        .ok_or_else(|| invalid(body_offset, format!("route.{tag} needs `id`")))?;
    let id = text_member((id_offset, id_raw), "id")?;
    let route = if tag.as_ref() == "synthetic" {
        ModelRoute::synthetic(id)
    } else {
        ModelRoute::harness(id)
    };
    route.map_err(|error| invalid(id_offset, error.to_string()))
}

fn answer_member(member: (usize, LazyValue<'_>)) -> Result<Answer, DecodeError> {
    let (offset, raw) = member;
    if let Some(literal) = raw.as_str() {
        return match literal {
            "approve" => Ok(Answer::Approve),
            "approve_for_session" => Ok(Answer::ApproveForSession),
            "decline" => Ok(Answer::Decline),
            "cancel" => Ok(Answer::Cancel),
            other => Err(invalid(offset, format!("unknown answer literal `{other}`"))),
        };
    }
    let mut inner = object_members(raw.as_raw_str(), offset, &["value"])?;
    let value = want(&mut inner, "value")
        .ok_or_else(|| invalid(offset, "an `answer` object needs a `value` member"))?;
    let json = RawJson::parse(value.1.as_raw_str())
        .map_err(|error| invalid(value.0, error.to_string()))?;
    Ok(Answer::Value(json))
}

fn assistant_stop_member(member: (usize, LazyValue<'_>)) -> Result<AssistantStop, DecodeError> {
    let (offset, raw) = member;
    if let Some(literal) = raw.as_str() {
        return match literal {
            "done" => Ok(AssistantStop::Done),
            "length" => Ok(AssistantStop::Length),
            "filter" => Ok(AssistantStop::Filter),
            "tool_use" => Ok(AssistantStop::ToolUse),
            "cancelled" => Ok(AssistantStop::Cancelled),
            other => Err(invalid(
                offset,
                format!("unknown assistant stop literal `{other}`"),
            )),
        };
    }
    let mut inner = object_members(raw.as_raw_str(), offset, &["failed"])?;
    let failed = want(&mut inner, "failed")
        .map(|member| text_member(member, "failed"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a `stop` object needs a `failed` member"))?;
    Ok(AssistantStop::Failed { message: failed })
}

fn turn_end_stop_member(member: (usize, LazyValue<'_>)) -> Result<TurnEndStop, DecodeError> {
    let (offset, raw) = member;
    if let Some(literal) = raw.as_str() {
        return match literal {
            "done" => Ok(TurnEndStop::Done),
            "length" => Ok(TurnEndStop::Length),
            "filter" => Ok(TurnEndStop::Filter),
            "max_steps" => Ok(TurnEndStop::MaxSteps),
            "cancelled" => Ok(TurnEndStop::Cancelled),
            "aborted" => Ok(TurnEndStop::Aborted),
            other => Err(invalid(
                offset,
                format!("unknown turn stop literal `{other}`"),
            )),
        };
    }
    let mut inner = object_members(raw.as_raw_str(), offset, &["failed"])?;
    let failed = want(&mut inner, "failed")
        .map(|member| text_member(member, "failed"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a `stop` object needs a `failed` member"))?;
    Ok(TurnEndStop::Failed { message: failed })
}

fn job_outcome_member(member: (usize, LazyValue<'_>)) -> Result<JobOutcome, DecodeError> {
    let (offset, raw) = member;
    if let Some(literal) = raw.as_str() {
        return match literal {
            "cancelled" => Ok(JobOutcome::Cancelled),
            "lost" => Ok(JobOutcome::Lost),
            other => Err(invalid(
                offset,
                format!("unknown job outcome literal `{other}`"),
            )),
        };
    }
    let mut inner = object_members(raw.as_raw_str(), offset, &["exited", "failed"])?;
    if let Some(exited) = want(&mut inner, "exited") {
        return Ok(JobOutcome::Exited {
            code: i32_member(exited, "exited")?,
        });
    }
    if let Some(failed) = want(&mut inner, "failed") {
        return Ok(JobOutcome::Failed {
            message: text_member(failed, "failed")?,
        });
    }
    Err(invalid(
        offset,
        "a `outcome` object needs an `exited` or `failed` member",
    ))
}

fn job_event_member(
    event: (usize, LazyValue<'_>),
    members: &mut Members<'_>,
) -> Result<JobEvent, DecodeError> {
    let (offset, raw) = event;
    let literal = raw
        .as_str()
        .ok_or_else(|| invalid(offset, "member `event` must be a string"))?;
    match literal {
        "start" => Ok(JobEvent::Started {
            kind: want(members, "kind")
                .map(|member| text_member(member, "kind"))
                .transpose()?,
        }),
        "end" => Ok(JobEvent::Settled {
            outcome: want(members, "outcome")
                .map(job_outcome_member)
                .transpose()?,
        }),
        "cancelled" => Ok(JobEvent::Cancelled {
            by: want(members, "by")
                .map(json_member::<ClientId>)
                .transpose()?,
        }),
        "killed" => Ok(JobEvent::Killed),
        "timed_out" => Ok(JobEvent::TimedOut),
        "orphaned" => Ok(JobEvent::Orphaned),
        other => Err(invalid(offset, format!("unknown job event `{other}`"))),
    }
}

fn mode_member(member: (usize, LazyValue<'_>)) -> Result<ApprovalMode, DecodeError> {
    let (offset, raw) = member;
    match raw.as_str() {
        Some("ask") => Ok(ApprovalMode::Ask),
        Some("auto-edit") => Ok(ApprovalMode::Edits),
        Some("yolo") => Ok(ApprovalMode::All),
        Some(other) => Err(invalid(offset, format!("unknown approval mode `{other}`"))),
        None => Err(invalid(offset, "member `mode` must be a string")),
    }
}

fn product_member(member: (usize, LazyValue<'_>)) -> Result<Product, DecodeError> {
    let (offset, raw) = member;
    match raw.as_str() {
        Some("dal") => Ok(Product::Dal),
        Some("dalgona") => Ok(Product::Dalgona),
        Some(other) => Err(invalid(offset, format!("unknown product `{other}`"))),
        None => Err(invalid(offset, "member `product` must be a string")),
    }
}

fn source_member(member: (usize, LazyValue<'_>)) -> Result<Option<Source>, DecodeError> {
    let (offset, raw) = member;
    if raw.is_null() {
        return Ok(None);
    }
    let mut inner = object_members(raw.as_raw_str(), offset, &["session", "entry"])?;
    let session = want(&mut inner, "session")
        .map(json_member::<SessionId>)
        .transpose()?
        .ok_or_else(|| invalid(offset, "a `from` object needs `session`"))?;
    let entry = opt_entry_id_member(need(&mut inner, "entry")?, "entry")?;
    Ok(Some(Source { session, entry }))
}

/// Reads the fixed tree-record prefix without allocating.
///
/// The byte string must start exactly `{"v":1,"type":"<tag>","id":<int>,
/// "parent":<int or null>,` with a tree `tag`. Returns `None` for every
/// other shape; callers then decode the whole line.
#[must_use]
pub fn scan_head(line: &[u8]) -> Option<ScannedHead> {
    let prefix = br#"{"v":1,"type":""#;
    if line.len() < prefix.len() + 4 || !line.starts_with(prefix) {
        return None;
    }
    let mut cursor = prefix.len();
    let tag_start = cursor;
    while cursor < line.len() && line[cursor] != b'"' {
        cursor += 1;
    }
    if cursor == tag_start || cursor >= line.len() {
        return None;
    }
    let tag = std::str::from_utf8(&line[tag_start..cursor]).ok()?;
    let kind = TreeKind::from_tag(tag)?;
    let mut rest = &line[cursor..];
    if !rest.starts_with(br#"","id":"#) {
        return None;
    }
    rest = &rest[7..];
    let mut id: u64 = 0;
    let mut digits = 0_usize;
    while let Some(&byte) = rest.first() {
        if byte.is_ascii_digit() {
            id = id.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
            digits += 1;
            rest = &rest[1..];
        } else {
            break;
        }
    }
    if digits == 0 || !rest.starts_with(br#","parent":"#) {
        return None;
    }
    rest = &rest[10..];
    let parent = if rest.starts_with(b"null") {
        rest = &rest[4..];
        None
    } else {
        let mut value: u64 = 0;
        let mut seen = 0_usize;
        while let Some(&byte) = rest.first() {
            if byte.is_ascii_digit() {
                value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
                seen += 1;
                rest = &rest[1..];
            } else {
                break;
            }
        }
        if seen == 0 {
            return None;
        }
        Some(EntryId::new(NonZeroU64::new(value)?))
    };
    if !rest.starts_with(b",") {
        return None;
    }
    Some(ScannedHead {
        id: EntryId::new(NonZeroU64::new(id)?),
        parent,
        kind,
    })
}

/// Decodes one complete journal line.
///
/// The `"v"` member is validated before anything else; `"type"` must then
/// name a format-1 kind. Members the record does not declare are dropped
/// so a reader survives fields a later format adds; the closed nested
/// payloads (`usage`, parts, blocks, `who`, `purpose`, `answer`, `from`,
/// `outcome`, `stop`) still reject unknown members, and `cost_usd` inside
/// `usage` is rejected rather than silently dropped.
///
/// # Errors
/// Returns [`DecodeError`] for a missing or unsupported version, an
/// unknown record kind, malformed JSON, or an invalid member.
pub fn decode(line: &[u8]) -> Result<Decoded, DecodeError> {
    let text = std::str::from_utf8(line.trim_ascii_end())
        .map_err(|_| invalid(0, "journal lines are UTF-8"))?;
    let mut members = Members::new();
    let mut version_member: Option<(usize, LazyValue<'_>)> = None;
    let mut type_member: Option<(usize, LazyValue<'_>)> = None;
    let mut duplicate_member: Option<DecodeError> = None;
    for member in sonic_rs::to_object_iter(text) {
        let (name, value) = member.map_err(|error| invalid(0, error.to_string()))?;
        let offset = value.as_raw_str().as_ptr() as usize - text.as_ptr() as usize;
        match name.as_ref() {
            "v" => {
                if version_member.is_some() {
                    return Err(invalid(offset, "duplicate member `v`"));
                }
                version_member = Some((offset, value));
            }
            "type" => {
                if type_member.is_some() {
                    duplicate_member
                        .get_or_insert_with(|| invalid(offset, "duplicate member `type`"));
                } else {
                    type_member = Some((offset, value));
                }
            }
            other => {
                if members.contains_key(other) {
                    duplicate_member.get_or_insert_with(|| {
                        invalid(offset, format!("duplicate member `{other}`"))
                    });
                } else {
                    members.insert(other.into(), (offset, value));
                }
            }
        }
    }
    let Some((version_offset, version_raw)) = version_member else {
        return Err(DecodeError::MissingVersion);
    };
    let version = version_raw
        .as_u64()
        .ok_or_else(|| invalid(version_offset, "member `v` must be an unsigned integer"))?;
    if version != u64::from(VERSION) {
        return Err(DecodeError::UnsupportedVersion { found: version });
    }
    if let Some(error) = duplicate_member {
        return Err(error);
    }
    let Some((type_offset, type_raw)) = type_member else {
        return Err(invalid(0, "a record needs a `type` member"));
    };
    let kind = type_raw
        .as_str()
        .ok_or_else(|| invalid(type_offset, "member `type` must be a string"))?;
    let record = decode_record(kind, &mut members)?;
    Ok(Decoded { record })
}

/// Pulls the `id`, `parent`, and `at` members every tree entry shares.
fn entry_prefix_members(
    members: &mut Members<'_>,
) -> Result<(EntryId, Option<EntryId>, jiff::Timestamp), DecodeError> {
    let id = entry_id_member(need(members, "id")?, "id")?;
    let parent = opt_entry_id_member(need(members, "parent")?, "parent")?;
    let at = json_member(need(members, "at")?)?;
    Ok((id, parent, at))
}

#[expect(
    clippy::too_many_lines,
    reason = "one match arm per record kind keeps the wire table readable"
)]
fn decode_record(kind: &str, members: &mut Members<'_>) -> Result<Record, DecodeError> {
    if TreeKind::from_tag(kind).is_some() {
        let (id, parent, at) = entry_prefix_members(members)?;
        let kind = decode_entry_kind(kind, members)?;
        let entry = Entry {
            id,
            parent,
            at,
            kind,
        };
        return Ok(match entry.kind {
            EntryKind::User { .. } => Record::User(entry),
            EntryKind::Assistant { .. } => Record::Assistant(entry),
            EntryKind::ToolResult { .. } => Record::ToolResult(entry),
            EntryKind::Reminder { .. } => Record::Reminder(entry),
            EntryKind::Model { .. } => Record::Model(entry),
            EntryKind::Thinking { .. } => Record::Thinking(entry),
            EntryKind::Approval { .. } => Record::Approval(entry),
            EntryKind::Compaction { .. } => Record::Compaction(entry),
            EntryKind::BranchSummary { .. } => Record::BranchSummary(entry),
        });
    }
    match kind {
        "session" => Ok(Record::Session(Header {
            id: json_member(need(members, "id")?)?,
            at: json_member(need(members, "at")?)?,
            workspace: json_member(need(members, "workspace")?)?,
            product: product_member(need(members, "product")?)?,
            from: source_member(need(members, "from")?)?,
        })),
        "boot" => Ok(Record::Boot {
            at: json_member(need(members, "at")?)?,
            r#gen: json_member(need(members, "gen")?)?,
            version: text_member(need(members, "version")?, "version")?,
        }),
        "leaf" => Ok(Record::Leaf {
            at: json_member(need(members, "at")?)?,
            to: opt_entry_id_member(need(members, "to")?, "to")?,
        }),
        "label" => Ok(Record::Label {
            at: json_member(need(members, "at")?)?,
            entry: entry_id_member(need(members, "entry")?, "entry")?,
            label: opt_text_member(need(members, "label")?, "label")?,
        }),
        "name" => Ok(Record::Name {
            at: json_member(need(members, "at")?)?,
            name: opt_text_member(need(members, "name")?, "name")?,
        }),
        "archive" => Ok(Record::Archive {
            at: json_member(need(members, "at")?)?,
            archived: bool_member(need(members, "archived")?, "archived")?,
        }),
        "turn_start" => Ok(Record::TurnStart {
            at: json_member(need(members, "at")?)?,
            turn: json_member(need(members, "turn")?)?,
        }),
        "tool_start" => Ok(Record::ToolStart {
            at: json_member(need(members, "at")?)?,
            turn: json_member(need(members, "turn")?)?,
            call: json_member(need(members, "call")?)?,
        }),
        "turn_end" => Ok(Record::TurnEnd {
            at: json_member(need(members, "at")?)?,
            turn: json_member(need(members, "turn")?)?,
            stop: turn_end_stop_member(need(members, "stop")?)?,
            usage: usage_member(need(members, "usage")?)?,
            changes: json_member(need(members, "changes")?)?,
        }),
        "rule_fired" => Ok(Record::RuleFired {
            at: json_member(need(members, "at")?)?,
            turn: json_member(need(members, "turn")?)?,
            rule: text_member(need(members, "rule")?, "rule")?,
            entry: entry_id_member(need(members, "entry")?, "entry")?,
        }),
        "resolved" => Ok(Record::Resolved {
            at: json_member(need(members, "at")?)?,
            request: json_member(need(members, "request")?)?,
            answer: answer_member(need(members, "answer")?)?,
            by: json_member(need(members, "by")?)?,
            was_default: match want(members, "was_default") {
                Some(member) => bool_member(member, "was_default")?,
                None => false,
            },
        }),
        "allow_always" => Ok(Record::AllowAlways {
            at: json_member(need(members, "at")?)?,
            tool: text_member(need(members, "tool")?, "tool")?,
            by: json_member(need(members, "by")?)?,
        }),
        "grant_given" => Ok(Record::GrantGiven {
            at: json_member(need(members, "at")?)?,
            ext: text_member(need(members, "ext")?, "ext")?,
            set: json_member(need(members, "set")?)?,
            scope: text_member(need(members, "scope")?, "scope")?,
            by: json_member(need(members, "by")?)?,
        }),
        "scoped_grant" => Ok(Record::ScopedGrant {
            at: json_member(need(members, "at")?)?,
            call: json_member(need(members, "call")?)?,
            prefix: json_member(need(members, "prefix")?)?,
            roots: json_member(need(members, "roots")?)?,
            job: json_member(need(members, "job")?)?,
            by: json_member(need(members, "by")?)?,
        }),
        "scoped_grant_ended" => Ok(Record::ScopedGrantEnded {
            at: json_member(need(members, "at")?)?,
            job: json_member(need(members, "job")?)?,
        }),
        "before_request_mut" => Ok(Record::BeforeRequestMut {
            at: json_member(need(members, "at")?)?,
            turn: json_member(need(members, "turn")?)?,
            ext: text_member(need(members, "ext")?, "ext")?,
            field: text_member(need(members, "field")?, "field")?,
            old: text_member(need(members, "old")?, "old")?,
            new: text_member(need(members, "new")?, "new")?,
        }),
        "tool_promoted" => Ok(Record::ToolPromoted {
            at: json_member(need(members, "at")?)?,
            tool: text_member(need(members, "tool")?, "tool")?,
            turn: match want(members, "turn") {
                Some(member) => Some(json_member::<TurnId>(member)?),
                None => None,
            },
            leaf: match want(members, "leaf") {
                Some(member) => Some(entry_id_member(member, "leaf")?),
                None => None,
            },
        }),
        "wake_attempt" => Ok(Record::WakeAttempt {
            at: json_member(need(members, "at")?)?,
            turn: json_member(need(members, "turn")?)?,
            count: u32::try_from(u64_member(need(members, "count")?, "count")?)
                .map_err(|_| invalid(0, "member `count` exceeds u32"))?,
        }),
        "job" => Ok(Record::Job {
            at: json_member(need(members, "at")?)?,
            job: json_member(need(members, "job")?)?,
            event: job_event_member(need(members, "event")?, members)?,
        }),
        "ext" => Ok(Record::Ext {
            at: json_member(need(members, "at")?)?,
            ext: text_member(need(members, "ext")?, "ext")?,
            kind: text_member(need(members, "kind")?, "kind")?,
            body: {
                let member = need(members, "body")?;
                RawJson::parse(member.1.as_raw_str())
                    .map_err(|error| invalid(member.0, error.to_string()))?
            },
        }),
        "mail" => Ok(Record::Mail(Mail {
            at: json_member(need(members, "at")?)?,
            from: json_member(need(members, "from")?)?,
            to: json_member(need(members, "to")?)?,
            mode: json_member(need(members, "mode")?)?,
            text: text_member(need(members, "text")?, "text")?,
            reply_to: opt_text_member(need(members, "reply_to")?, "reply_to")?,
        })),
        "inferred" => Ok(Record::Inferred {
            at: json_member(need(members, "at")?)?,
            who: owner_member(need(members, "who")?)?,
            purpose: purpose_member(need(members, "purpose")?)?,
            usage: usage_member(need(members, "usage")?)?
                .ok_or_else(|| invalid(0, "member `usage` must be an object"))?,
        }),
        other => Err(DecodeError::UnknownRecordKind { kind: other.into() }),
    }
}

fn decode_entry_kind(kind: &str, members: &mut Members<'_>) -> Result<EntryKind, DecodeError> {
    Ok(match kind {
        "user" => EntryKind::User {
            parts: parts_member(need(members, "parts")?)?,
        },
        "assistant" => EntryKind::Assistant {
            api: json_member(need(members, "api")?)?,
            model: model_id_member(need(members, "model")?)?,
            content: blocks_member(need(members, "content")?)?,
            usage: usage_member(need(members, "usage")?)?
                .ok_or_else(|| invalid(0, "member `usage` must be an object"))?,
            stop: assistant_stop_member(need(members, "stop")?)?,
        },
        "tool_result" => EntryKind::ToolResult {
            call: json_member(need(members, "call")?)?,
            name: text_member(need(members, "name")?, "name")?,
            error: bool_member(need(members, "error")?, "error")?,
            parts: parts_member(need(members, "parts")?)?,
            changes: json_member(need(members, "changes")?)?,
        },
        "reminder" => EntryKind::Reminder {
            source: text_member(need(members, "source")?, "source")?,
            text: text_member(need(members, "text")?, "text")?,
        },
        "model" => EntryKind::Model {
            route: model_route_members(members)?,
        },
        "thinking" => EntryKind::Thinking {
            level: json_member(need(members, "level")?)?,
        },
        "approval" => EntryKind::Approval {
            mode: mode_member(need(members, "mode")?)?,
        },
        "compaction" => EntryKind::Compaction {
            summary: opt_text_member(need(members, "summary")?, "summary")?,
            first_kept: opt_entry_id_member(need(members, "first_kept")?, "first_kept")?,
            tokens_before: u64_member(need(members, "tokens_before")?, "tokens_before")?,
            replay: opt_raw_json_member(need(members, "replay")?, "replay")?,
            usage: usage_member(need(members, "usage")?)?,
        },
        "branch_summary" => EntryKind::BranchSummary {
            from: entry_id_member(need(members, "from")?, "from")?,
            summary: text_member(need(members, "summary")?, "summary")?,
        },
        other => {
            return Err(DecodeError::UnknownRecordKind { kind: other.into() });
        }
    })
}

// Pure branch projection.

/// What a fork or clone copies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BranchMode {
    /// Copy the path through the anchor user entry's parent; the fork
    /// restarts from that entry's text.
    Fork {
        /// The user entry that restarts the branch.
        at: EntryId,
    },
    /// Copy the path through the current leaf.
    Clone,
}

/// The records of a branched session, in journal order.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    /// A header carrying `from` filled for the destination; the caller
    /// assigns the fresh `id`, `at`, `workspace`, and `product` before
    /// writing it.
    pub header: Header,
    /// The copied entries and path-local records, in source order.
    pub records: Vec<Record>,
    /// The fork anchor's parts; empty for a clone. The caller resolves
    /// blob parts and concatenates text parts with no separator.
    pub anchor_parts: Vec<JournalPart>,
}

/// A branch projection that cannot be made.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum BranchError {
    /// There is no path to copy.
    #[error("session has no entries to clone")]
    NoEntries,
    /// The anchor is not a user entry.
    #[error("entry {entry} is not a user message; /fork starts from a user message")]
    NotUserEntry {
        /// The offending entry.
        entry: EntryId,
    },
    /// The anchor does not exist.
    #[error("session has no entry {entry}")]
    UnknownEntry {
        /// The missing entry.
        entry: EntryId,
    },
}

/// Copies a session's chosen path into branch records without I/O.
///
/// A fork copies the path from the root through the anchor user entry's
/// parent; a clone copies it through `leaf`. Entry ids survive and
/// parents re-chain along the path. `label` records naming a path entry
/// copy with it. The returned header carries `from` describing the
/// source; its other members still describe the source session, so the
/// store rewrites `id`, `at`, `workspace`, and `product` and appends the
/// `boot` record itself.
///
/// # Errors
/// Returns [`BranchError::NoEntries`] when nothing can be cloned,
/// [`BranchError::UnknownEntry`] for a missing anchor, and
/// [`BranchError::NotUserEntry`] for a non-user anchor.
pub fn branch(
    records: &[Record],
    leaf: Option<EntryId>,
    mode: BranchMode,
    source: &Header,
) -> Result<Branch, BranchError> {
    let mut index: BTreeMap<EntryId, &Entry> = BTreeMap::new();
    for record in records {
        if let Some(entry) = record.entry() {
            index.insert(entry.id, entry);
        }
    }
    let anchor = match mode {
        BranchMode::Fork { at } => at,
        BranchMode::Clone => leaf.ok_or(BranchError::NoEntries)?,
    };
    let anchor_entry = index
        .get(&anchor)
        .ok_or(BranchError::UnknownEntry { entry: anchor })?;
    let (cutoff, anchor_parts) = match mode {
        BranchMode::Fork { .. } => match &anchor_entry.kind {
            EntryKind::User { parts } => (anchor_entry.parent, parts.clone()),
            _ => {
                return Err(BranchError::NotUserEntry {
                    entry: anchor_entry.id,
                });
            }
        },
        BranchMode::Clone => (Some(anchor_entry.id), Vec::new()),
    };
    let mut path: Vec<EntryId> = Vec::new();
    let mut cursor = cutoff;
    while let Some(id) = cursor {
        path.push(id);
        cursor = index.get(&id).and_then(|entry| entry.parent);
    }
    path.reverse();
    let mut copied = Vec::new();
    for record in records {
        let on_path = match record {
            Record::Label { entry, .. } => path.contains(entry),
            _ => record.entry().is_some_and(|entry| path.contains(&entry.id)),
        };
        if on_path {
            copied.push(record.clone());
        }
    }
    let header = Header {
        id: source.id,
        at: source.at,
        workspace: source.workspace.clone(),
        product: source.product,
        from: Some(Source {
            session: source.id,
            entry: Some(anchor),
        }),
    };
    Ok(Branch {
        header,
        records: copied,
        anchor_parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every canonical vector whose members are spec-shaped. The
    /// `req_0142`/`job_0091` lines are shorthand the document uses for
    /// readability; the typed members are `UUIDv7` text, so those shapes
    /// round-trip in `resolved_and_job_records_round_trip` with real ids.
    const VECTORS: &[&str] = &[
        "{\"v\":1,\"type\":\"session\",\"id\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"at\":\"2026-09-25T10:15:30.123Z\",\"workspace\":\"/home/alpha/harness/reed\",\"product\":\"dal\",\"from\":null}\n",
        "{\"v\":1,\"type\":\"session\",\"id\":\"01927f40-0000-7000-8000-000000000001\",\"at\":\"2026-09-25T11:00:00.000Z\",\"workspace\":\"/home/alpha/harness/reed\",\"product\":\"dal\",\"from\":{\"session\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"entry\":41}}\n",
        "{\"v\":1,\"type\":\"boot\",\"at\":\"2026-09-25T10:15:30.124Z\",\"gen\":1,\"version\":\"0.1.0\"}\n",
        "{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}\n",
        "{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}\n",
        "{\"v\":1,\"type\":\"label\",\"at\":\"2026-09-25T14:06:00.000Z\",\"entry\":41,\"label\":\"before refactor\"}\n",
        "{\"v\":1,\"type\":\"label\",\"at\":\"2026-09-25T14:06:30.000Z\",\"entry\":41,\"label\":null}\n",
        "{\"v\":1,\"type\":\"name\",\"at\":\"2026-09-25T14:07:00.000Z\",\"name\":\"parser fix\"}\n",
        "{\"v\":1,\"type\":\"name\",\"at\":\"2026-09-25T14:07:30.000Z\",\"name\":null}\n",
        "{\"v\":1,\"type\":\"archive\",\"at\":\"2026-09-25T14:08:00.000Z\",\"archived\":true}\n",
        "{\"v\":1,\"type\":\"turn_start\",\"at\":\"2026-09-25T10:15:31.000Z\",\"turn\":1}\n",
        "{\"v\":1,\"type\":\"tool_start\",\"at\":\"2026-09-25T10:15:35.420Z\",\"turn\":1,\"call\":\"toolu_01\"}\n",
        "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":5400,\"output\":610,\"cache_read\":4200,\"cache_write\":1100,\"reasoning\":null,\"cost_micro_usd\":18830},\"changes\":[{\"path\":\"lib/lexer.ml\",\"added\":12,\"removed\":3}]}\n",
        "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:20:00.000Z\",\"turn\":2,\"stop\":\"aborted\",\"usage\":{\"input\":5400,\"output\":10,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":310},\"changes\":[]}\n",
        "{\"v\":1,\"type\":\"rule_fired\",\"at\":\"2026-09-25T10:15:40.001Z\",\"turn\":1,\"rule\":\"no-sleep\",\"entry\":7}\n",
        "{\"v\":1,\"type\":\"allow_always\",\"at\":\"2026-09-25T10:15:38.000Z\",\"tool\":\"exec\",\"by\":\"tui\"}\n",
        "{\"v\":1,\"type\":\"grant_given\",\"at\":\"2026-09-25T10:15:39.000Z\",\"ext\":\"web\",\"set\":[\"net\"],\"scope\":\"saved\",\"by\":\"tui\"}\n",
        "{\"v\":1,\"type\":\"grant_given\",\"at\":\"2026-09-25T10:15:39.500Z\",\"ext\":\"orchestration\",\"set\":[\"agents\",\"jobs\"],\"scope\":\"call\",\"by\":\"tui\"}\n",
        "{\"v\":1,\"type\":\"tool_promoted\",\"at\":\"2026-09-25T10:15:41.000Z\",\"tool\":\"write\"}\n",
        "{\"v\":1,\"type\":\"ext\",\"at\":\"2026-09-25T10:15:43.000Z\",\"ext\":\"orchestration\",\"kind\":\"children\",\"body\":{\"run\":\"r1\",\"step\":\"scan\"}}\n",
        "{\"v\":1,\"type\":\"user\",\"id\":4,\"parent\":3,\"at\":\"2026-09-25T10:15:31.001Z\",\"parts\":[{\"type\":\"text\",\"text\":\"Fix the parser\"},{\"type\":\"image\",\"mime\":\"image/png\",\"blob\":\"9f2ca1bd...64 hex...\",\"bytes\":48213}]}\n",
        "{\"v\":1,\"type\":\"model\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.125Z\",\"api\":\"openai_responses\",\"model\":\"gpt-6-luna\"}\n",
        "{\"v\":1,\"type\":\"thinking\",\"id\":2,\"parent\":1,\"at\":\"2026-09-25T10:15:30.126Z\",\"level\":\"high\"}\n",
        "{\"v\":1,\"type\":\"approval\",\"id\":3,\"parent\":2,\"at\":\"2026-09-25T10:15:30.127Z\",\"mode\":\"ask\"}\n",
        "{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":4,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"claude-opus-5\",\"content\":[{\"type\":\"reasoning\",\"text\":\"The lexer drops the last token.\",\"replay\":{\"type\":\"thinking\",\"thinking\":\"The lexer drops the last token.\",\"signature\":\"EqQBCkgI...\"}},{\"type\":\"text\",\"text\":\"I will read the lexer.\",\"replay\":null},{\"type\":\"tool_call\",\"id\":\"toolu_01\",\"name\":\"read\",\"input\":{\"path\":\"lib/lexer.ml\"}}],\"usage\":{\"input\":1200,\"output\":85,\"cache_read\":0,\"cache_write\":1100,\"reasoning\":null,\"cost_micro_usd\":4210},\"stop\":\"tool_use\"}\n",
        "{\"v\":1,\"type\":\"tool_result\",\"id\":6,\"parent\":5,\"at\":\"2026-09-25T10:15:35.431Z\",\"call\":\"toolu_01\",\"name\":\"read\",\"error\":false,\"parts\":[{\"type\":\"text\",\"blob\":\"4be1f00d...64 hex...\",\"bytes\":52110}],\"changes\":[]}\n",
        "{\"v\":1,\"type\":\"reminder\",\"id\":7,\"parent\":6,\"at\":\"2026-09-25T10:15:40.000Z\",\"source\":\"rule:no-sleep\",\"text\":\"Do not add sleep calls to tests.\"}\n",
        "{\"v\":1,\"type\":\"compaction\",\"id\":58,\"parent\":57,\"at\":\"2026-09-25T13:00:00.000Z\",\"summary\":\"The user asked to fix the parser...\",\"first_kept\":51,\"tokens_before\":171234,\"replay\":null,\"usage\":{\"input\":5400,\"output\":610,\"cache_read\":4200,\"cache_write\":1100,\"reasoning\":null,\"cost_micro_usd\":18830}}\n",
        "{\"v\":1,\"type\":\"compaction\",\"id\":90,\"parent\":89,\"at\":\"2026-09-25T14:00:00.000Z\",\"summary\":null,\"first_kept\":null,\"tokens_before\":203004,\"replay\":{\"api\":\"openai_responses\",\"items\":[{\"type\":\"compaction\",\"id\":\"cmp_001\",\"encrypted_content\":\"gAAAAABpM0Yj...\"}]},\"usage\":null}\n",
        "{\"v\":1,\"type\":\"branch_summary\",\"id\":91,\"parent\":41,\"at\":\"2026-09-25T14:05:01.000Z\",\"from\":90,\"summary\":\"On that branch the user tried a table-driven lexer.\"}\n",
        "{\"v\":1,\"type\":\"mail\",\"at\":\"2026-09-26T10:15:30.123Z\",\"from\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"to\":\"01927f40-0000-7000-8000-000000000001\",\"mode\":\"aside\",\"text\":\"review this result\",\"reply_to\":null}\n",
        "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":{\"extension\":{\"name\":\"fusion\",\"origin\":\"bundled\"}},\"purpose\":{\"synthetic\":{\"id\":\"dalgona/fusion\"}},\"usage\":{\"input\":10,\"output\":4,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}\n",
    ];

    fn nz(value: u64) -> NonZeroU64 {
        match NonZeroU64::new(value) {
            Some(id) => id,
            None => NonZeroU64::MIN,
        }
    }

    fn user_entry(id: u64, parent: Option<u64>, text: &str) -> Entry {
        Entry {
            id: EntryId::new(nz(id)),
            parent: parent.map(|value| EntryId::new(nz(value))),
            at: "2026-09-25T10:15:30.000Z"
                .parse()
                .unwrap_or(jiff::Timestamp::UNIX_EPOCH),
            kind: EntryKind::User {
                parts: vec![JournalPart::Text { text: text.into() }],
            },
        }
    }

    #[test]
    fn canonical_vectors_round_trip_byte_for_byte() -> Result<(), Box<dyn std::error::Error>> {
        for line in VECTORS {
            let decoded = decode(line.as_bytes())?;
            let encoded = encode(&decoded.record)?;
            assert_eq!(
                encoded.as_slice(),
                line.as_bytes(),
                "round trip diverged for {line}"
            );
        }
        Ok(())
    }

    #[test]
    fn resolved_and_extended_records_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        // The doc vectors use `req_0142`/`job_0091` shorthand; the typed
        // members are UUIDv7 text, so these shapes ride real ids.
        let lines = [
            "{\"v\":1,\"type\":\"resolved\",\"at\":\"2026-09-25T10:15:36.000Z\",\"request\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"answer\":\"approve\",\"by\":\"tui\"}\n",
            "{\"v\":1,\"type\":\"resolved\",\"at\":\"2026-09-25T10:15:37.000Z\",\"request\":\"01927f40-0000-7000-8000-000000000001\",\"answer\":{\"value\":{\"text\":\"alpha\"}},\"by\":\"rpc:7:codex-desktop\"}\n",
            "{\"v\":1,\"type\":\"job\",\"at\":\"2026-09-25T10:15:42.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\",\"event\":\"start\",\"kind\":\"worker\"}\n",
            "{\"v\":1,\"type\":\"job\",\"at\":\"2026-09-25T10:16:00.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\",\"event\":\"end\",\"outcome\":{\"exited\":0}}\n",
            "{\"v\":1,\"type\":\"job\",\"at\":\"2026-09-25T10:16:01.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\",\"event\":\"orphaned\"}\n",
            "{\"v\":1,\"type\":\"resolved\",\"at\":\"2026-09-25T10:15:38.000Z\",\"request\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"answer\":\"approve\",\"by\":\"tui\",\"was_default\":true}\n",
            "{\"v\":1,\"type\":\"tool_promoted\",\"at\":\"2026-09-25T10:15:41.000Z\",\"tool\":\"write\",\"turn\":1,\"leaf\":41}\n",
            "{\"v\":1,\"type\":\"scoped_grant\",\"at\":\"2026-09-25T10:15:39.000Z\",\"call\":\"call_1\",\"prefix\":[\"git\",\"push\"],\"roots\":[\"/work\"],\"job\":\"01927f40-0000-7000-8000-000000000002\",\"by\":\"tui\"}\n",
            "{\"v\":1,\"type\":\"scoped_grant_ended\",\"at\":\"2026-09-25T10:16:40.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\"}\n",
            "{\"v\":1,\"type\":\"before_request_mut\",\"at\":\"2026-09-25T10:15:33.000Z\",\"turn\":1,\"ext\":\"redact\",\"field\":\"model\",\"old\":\"a\",\"new\":\"b\"}\n",
            "{\"v\":1,\"type\":\"wake_attempt\",\"at\":\"2026-09-25T10:15:34.000Z\",\"turn\":1,\"count\":2}\n",
        ];
        for line in lines {
            let decoded = decode(line.as_bytes())?;
            assert_eq!(
                encode(&decoded.record)?.as_slice(),
                line.as_bytes(),
                "round trip diverged for {line}"
            );
        }
        Ok(())
    }

    #[test]
    fn decode_drops_unknown_members_and_rejects_bad_shapes() {
        // A record member the format does not declare is dropped, per the
        // store's `unknown_member_dropped_on_decode` test contract.
        let decoded = decode(
            b"{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41,\"mystery\":true}",
        );
        assert!(matches!(
            decoded,
            Ok(Decoded {
                record: Record::Leaf { .. }
            })
        ));
        assert!(matches!(
            decode(b"{\"v\":2,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}"),
            Err(DecodeError::UnsupportedVersion { found: 2 })
        ));
        assert!(matches!(
            decode(b"{\"type\":\"leaf\",\"at\":\"x\",\"to\":1}"),
            Err(DecodeError::MissingVersion)
        ));
        assert!(matches!(
            decode(b"{\"v\":1,\"type\":\"bogus\",\"at\":\"x\"}"),
            Err(DecodeError::UnknownRecordKind { .. })
        ));
        // The closed `usage` object stays strict: a `cost_usd` member
        // would silently mis-spell money if it were tolerated.
        assert!(matches!(
            decode(b"{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_usd\":0.5},\"stop\":\"done\"}"),
            Err(DecodeError::Invalid { .. })
        ));
        // A non-null `from` object must carry both `session` and `entry`;
        // `entry:null` is the only way to say "no anchor entry".
        assert!(matches!(
            decode(b"{\"v\":1,\"type\":\"session\",\"id\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"at\":\"2026-09-25T10:15:30.123Z\",\"workspace\":\"/w\",\"product\":\"dal\",\"from\":{\"session\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\"}}"),
            Err(DecodeError::Invalid { .. })
        ));
    }

    #[test]
    fn decode_rejects_duplicate_version_type_and_record_members_at_second_value() {
        let cases = [
            (
                "{\"v\":1,\"v\":2,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}",
                "\"v\":2",
                4,
            ),
            (
                "{\"v\":1,\"type\":\"leaf\",\"type\":\"name\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}",
                "\"type\":\"name\"",
                7,
            ),
            (
                "{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41,\"to\":42}",
                "\"to\":42",
                5,
            ),
        ];
        for (line, marker, value_offset) in cases {
            let expected_offset = line.find(marker).unwrap() + value_offset;
            assert!(matches!(
                decode(line.as_bytes()),
                Err(DecodeError::Invalid { offset, .. }) if offset == expected_offset
            ));
        }
    }

    #[test]
    fn decode_rejects_duplicate_nested_members_at_second_value() {
        let line = "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":1,\"input\":2,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":0},\"changes\":[]}";
        let expected_offset = line.find("\"input\":2").unwrap() + 8;
        assert!(matches!(
            decode(line.as_bytes()),
            Err(DecodeError::Invalid { offset, .. }) if offset == expected_offset
        ));
    }

    #[test]
    fn decode_reports_absolute_nested_member_offsets() {
        let cases = [
            (
                "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":1,\"input\":2,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":0},\"changes\":[]}",
                "\"input\":",
                1,
                8,
            ),
            (
                "{\"v\":1,\"type\":\"user\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.000Z\",\"parts\":[{\"type\":\"text\",\"text\":\"a\",\"text\":\"b\"}]}",
                "\"text\":",
                1,
                7,
            ),
            (
                "{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"a\",\"text\":\"b\"}],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\"}",
                "\"text\":",
                1,
                7,
            ),
            (
                "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":{\"extension\":{\"name\":\"x\",\"name\":\"y\",\"origin\":\"bundled\"}},\"purpose\":{\"synthetic\":{\"id\":\"m\"}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}",
                "\"name\":",
                1,
                7,
            ),
            (
                "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":\"core\",\"purpose\":{\"synthetic\":{\"id\":\"x\",\"id\":\"y\"}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}",
                "\"id\":",
                1,
                5,
            ),
            (
                "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":1,\"future\":2},\"changes\":[]}",
                "\"future\":",
                0,
                9,
            ),
            (
                "{\"v\":1,\"type\":\"user\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.000Z\",\"parts\":[{\"type\":\"text\",\"future\":1}]}",
                "\"future\":",
                0,
                9,
            ),
            (
                "{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"future\":1}],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\"}",
                "\"future\":",
                0,
                9,
            ),
            (
                "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":{\"extension\":{\"name\":\"x\",\"origin\":\"bundled\",\"future\":1}},\"purpose\":{\"synthetic\":{\"id\":\"m\"}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}",
                "\"future\":",
                0,
                9,
            ),
            (
                "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":\"core\",\"purpose\":{\"synthetic\":{\"id\":\"m\",\"future\":1}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}",
                "\"future\":",
                0,
                9,
            ),
        ];
        for (line, marker, occurrence, value_offset) in cases {
            let marker_at = line.match_indices(marker).nth(occurrence).unwrap().0;
            let expected_offset = marker_at + value_offset;
            assert!(matches!(
                decode(line.as_bytes()),
                Err(DecodeError::Invalid { offset, .. }) if offset == expected_offset
            ));
        }
    }

    #[test]
    fn unsupported_version_precedes_duplicate_type_and_payload() {
        for line in [
            "{\"v\":2,\"type\":\"leaf\",\"type\":\"other\",\"at\":\"x\",\"to\":1}",
            "{\"type\":\"leaf\",\"to\":1,\"to\":2,\"v\":2}",
            "{\"to\":1,\"v\":2,\"type\":\"leaf\",\"to\":2}",
        ] {
            assert!(matches!(
                decode(line.as_bytes()),
                Err(DecodeError::UnsupportedVersion { found: 2 })
            ));
        }
    }

    fn turn_end_with_cost(cost_usd: f64) -> Record {
        Record::TurnEnd {
            at: "2026-09-25T10:16:02.000Z"
                .parse()
                .unwrap_or(jiff::Timestamp::UNIX_EPOCH),
            turn: TurnId::new(nz(1)),
            stop: TurnEndStop::Done,
            usage: Some(Usage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: Some(cost_usd),
            }),
            changes: Vec::new(),
        }
    }

    #[test]
    fn encode_preserves_representable_high_micro_dollar_cost() -> Result<(), EncodeError> {
        let encoded = encode(&turn_end_with_cost(18_000_000_000_000.0))?;
        assert!(
            std::str::from_utf8(&encoded)
                .is_ok_and(|line| line.contains("\"cost_micro_usd\":18000000000000000000"))
        );
        Ok(())
    }

    #[test]
    fn encode_rejects_cost_rounding_to_u64_overflow() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "u64::MAX rounds to 2^64 as f64, the exclusive micro-dollar bound"
        )]
        let cost_usd = (u64::MAX as f64) / 1_000_000.0;
        assert!(matches!(
            encode(&turn_end_with_cost(cost_usd)),
            Err(EncodeError::InvalidCost)
        ));
    }

    #[test]
    fn encode_rejects_negative_and_infinite_costs() {
        assert!(matches!(
            encode(&turn_end_with_cost(-0.000_001)),
            Err(EncodeError::InvalidCost)
        ));
        assert!(matches!(
            encode(&turn_end_with_cost(f64::INFINITY)),
            Err(EncodeError::InvalidCost)
        ));
    }

    #[test]
    fn encode_keeps_explicit_zero_cost() -> Result<(), EncodeError> {
        let encoded = encode(&turn_end_with_cost(0.0))?;
        assert!(
            std::str::from_utf8(&encoded).is_ok_and(|line| line.contains("\"cost_micro_usd\":0"))
        );
        Ok(())
    }

    fn model_line(tail: &str) -> String {
        format!(
            "{{\"v\":1,\"type\":\"model\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.125Z\",{tail}}}\n"
        )
    }

    fn decoded_route(line: &str) -> Result<ModelRoute, Box<dyn std::error::Error>> {
        let Record::Model(Entry {
            kind: EntryKind::Model { route },
            ..
        }) = decode(line.as_bytes())?.record
        else {
            return Err("expected a model record".into());
        };
        Ok(route)
    }

    #[test]
    fn model_record_keeps_format1_api_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let line = model_line(r#""api":"openai_responses","model":"gpt-6-luna""#);
        let decoded = decode(line.as_bytes())?;
        let Record::Model(entry) = &decoded.record else {
            return Err("expected a model record".into());
        };
        assert_eq!(
            entry.kind,
            EntryKind::Model {
                route: ModelRoute::Api {
                    family: Family::Responses,
                    model: "gpt-6-luna".into(),
                },
            }
        );
        assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());
        Ok(())
    }

    #[test]
    fn model_record_round_trips_synthetic_and_harness_routes()
    -> Result<(), Box<dyn std::error::Error>> {
        let cases = [
            (
                r#""route":{"synthetic":{"id":"dalgona/fusion-2.1_x"}}"#,
                ModelRoute::Synthetic {
                    id: "dalgona/fusion-2.1_x".into(),
                },
            ),
            (
                r#""route":{"harness":{"id":"dalgon/eval-first"}}"#,
                ModelRoute::Harness {
                    id: "dalgon/eval-first".into(),
                },
            ),
        ];
        for (tail, expected) in cases {
            let line = model_line(tail);
            assert_eq!(decoded_route(&line)?, expected, "{line}");
            assert_eq!(
                encode(&decode(line.as_bytes())?.record)?.as_slice(),
                line.as_bytes()
            );
        }
        Ok(())
    }

    #[test]
    fn model_record_rejects_bad_route_shapes() {
        let cases = [
            // Both spellings at once.
            r#""route":{"synthetic":{"id":"a/b"}},"api":"anthropic","model":"m""#,
            r#""model":"m","route":{"harness":{"id":"dalgon/normal"}}"#,
            // Neither spelling, or half of the API pair.
            r#""name":"m""#,
            r#""model":"m""#,
            r#""api":"anthropic""#,
            // An unknown family stays rejected.
            r#""api":"openai_other","model":"m""#,
            // No tag, two tags, an API tag, or a non-object route.
            r#""route":{}"#,
            r#""route":{"synthetic":{"id":"a/b"},"harness":{"id":"dalgon/normal"}}"#,
            r#""route":{"api":{"family":"anthropic","model":"m"}}"#,
            r#""route":"dalgon/normal""#,
            // Missing, unknown, duplicate, or non-string fields.
            r#""route":{"synthetic":{}}"#,
            r#""route":{"synthetic":{"id":"a/b","family":"anthropic"}}"#,
            r#""route":{"harness":{"id":"dalgon/normal","id":"dalgon/eval-only"}}"#,
            r#""route":{"synthetic":{"id":1}}"#,
            // A duplicate record member.
            r#""route":{"harness":{"id":"dalgon/normal"}},"route":{"harness":{"id":"dalgon/normal"}}"#,
            // Ids outside the closed grammars.
            r#""route":{"synthetic":{"id":"Upper/x"}}"#,
            r#""route":{"synthetic":{"id":"no-slash"}}"#,
            r#""route":{"harness":{"id":"dalgon/fast"}}"#,
            r#""route":{"harness":{"id":"dalgona/fusion"}}"#,
        ];
        for tail in cases {
            let line = model_line(tail);
            assert!(
                matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
                "{line}"
            );
        }
        let line = model_line(r#""route":{"harness":{"id":"dalgon/fast"}}"#);
        let expected = line.find("\"dalgon/fast\"").unwrap_or(usize::MAX);
        assert!(matches!(
            decode(line.as_bytes()),
            Err(DecodeError::Invalid { offset, .. }) if offset == expected
        ));
    }

    #[test]
    fn encode_rejects_non_api_route_the_decoder_would_reject() {
        let entry = |route: ModelRoute| {
            Record::Model(Entry {
                id: EntryId::new(nz(1)),
                parent: None,
                at: jiff::Timestamp::UNIX_EPOCH,
                kind: EntryKind::Model { route },
            })
        };
        assert!(matches!(
            encode(&entry(ModelRoute::Synthetic { id: "bad".into() })),
            Err(EncodeError::InvalidRoute(
                RouteError::InvalidSyntheticId { .. }
            ))
        ));
        assert!(matches!(
            encode(&entry(ModelRoute::Harness {
                id: "dalgon/fast".into()
            })),
            Err(EncodeError::InvalidRoute(
                RouteError::InvalidHarnessId { .. }
            ))
        ));
    }

    #[test]
    fn empty_model_id_is_rejected_both_ways() -> Result<(), Box<dyn std::error::Error>> {
        let assistant = "{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\"}\n";
        let empty = assistant.replace("\"model\":\"m\"", "\"model\":\"\"");
        let expected = empty.find("\"model\":\"\"").map(|at| at + 8);
        assert!(matches!(
            decode(empty.as_bytes()),
            Err(DecodeError::Invalid { offset, .. }) if Some(offset) == expected
        ));
        assert!(matches!(
            decode(model_line(r#""api":"anthropic","model":"""#).as_bytes()),
            Err(DecodeError::Invalid { .. })
        ));
        let mut record = decode(assistant.as_bytes())?.record;
        assert_eq!(encode(&record)?.as_slice(), assistant.as_bytes());
        if let Record::Assistant(Entry {
            kind: EntryKind::Assistant { model, .. },
            ..
        }) = &mut record
        {
            *model = "".into();
        }
        // The serde path (views, updates) refuses the same empty id: the
        // derived spelling of this record differs from the valid one only
        // in `model`, and the valid one decodes.
        let Record::Assistant(entry) = &record else {
            return Err("expected an assistant record".into());
        };
        let serde_text = sonic_rs::to_string(&entry.kind)?;
        assert!(sonic_rs::from_str::<EntryKind>(&serde_text).is_err());
        let valid = serde_text.replace("\"model\":\"\"", "\"model\":\"m\"");
        sonic_rs::from_str::<EntryKind>(&valid)?;
        assert!(matches!(encode(&record), Err(EncodeError::EmptyModel)));
        let api = Record::Model(Entry {
            id: EntryId::new(nz(1)),
            parent: None,
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::Model {
                route: ModelRoute::Api {
                    family: Family::Anthropic,
                    model: "".into(),
                },
            },
        });
        assert!(matches!(encode(&api), Err(EncodeError::EmptyModel)));
        let Record::Model(entry) = &api else {
            return Err("expected a model record".into());
        };
        let serde_text = sonic_rs::to_string(&entry.kind)?;
        assert!(sonic_rs::from_str::<EntryKind>(&serde_text).is_err());
        let valid = serde_text.replace("\"model\":\"\"", "\"model\":\"m\"");
        sonic_rs::from_str::<EntryKind>(&valid)?;
        Ok(())
    }

    #[test]
    fn generic_blob_part_round_trips_and_stays_strict() -> Result<(), Box<dyn std::error::Error>> {
        let user_line = |part: &str| {
            format!(
                "{{\"v\":1,\"type\":\"user\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.000Z\",\"parts\":[{part}]}}\n"
            )
        };
        let digest = "0123456789abcdef".repeat(4);
        let line = user_line(&format!(
            r#"{{"type":"blob","mime":"application/pdf","blob":"{digest}","bytes":9}}"#
        ));
        let decoded = decode(line.as_bytes())?;
        let Record::User(Entry {
            kind: EntryKind::User { parts },
            ..
        }) = &decoded.record
        else {
            return Err("expected a user record".into());
        };
        assert_eq!(
            parts.as_slice(),
            &[JournalPart::Blob {
                mime: "application/pdf".into(),
                blob: digest.as_str().into(),
                bytes: 9,
            }]
        );
        assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());

        for part in [
            r#"{"type":"blob","blob":"ab12","bytes":9}"#,
            r#"{"type":"blob","mime":"application/pdf","bytes":9}"#,
            r#"{"type":"blob","mime":"application/pdf","blob":"ab12"}"#,
            r#"{"type":"blob","mime":"a/b","mime":"a/c","blob":"ab12","bytes":9}"#,
            r#"{"type":"blob","mime":"a/b","blob":"ab12","bytes":9,"text":"x"}"#,
            r#"{"type":"blob","mime":"a/b","blob":"ab12","bytes":9,"base64":"eA=="}"#,
            r#"{"type":"blob","mime":"a/b","blob":"ab12","bytes":9,"future":1}"#,
        ] {
            let line = user_line(part);
            assert!(
                matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
                "{line}"
            );
        }
        Ok(())
    }

    #[test]
    fn new_stop_literals_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let turn_end = |stop: &str| {
            format!(
                "{{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:20:00.000Z\",\"turn\":2,\"stop\":\"{stop}\",\"usage\":null,\"changes\":[]}}\n"
            )
        };
        let turn_cases = [
            ("length", TurnEndStop::Length),
            ("filter", TurnEndStop::Filter),
            ("max_steps", TurnEndStop::MaxSteps),
            ("aborted", TurnEndStop::Aborted),
        ];
        for (literal, expected) in turn_cases {
            let line = turn_end(literal);
            let decoded = decode(line.as_bytes())?;
            let Record::TurnEnd { stop, .. } = &decoded.record else {
                return Err("expected a turn_end record".into());
            };
            assert_eq!(*stop, expected);
            assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());
        }
        assert!(matches!(
            decode(turn_end("tool_use").as_bytes()),
            Err(DecodeError::Invalid { .. })
        ));

        let line = "{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"filter\"}\n";
        let decoded = decode(line.as_bytes())?;
        let Record::Assistant(Entry {
            kind: EntryKind::Assistant { stop, .. },
            ..
        }) = &decoded.record
        else {
            return Err("expected an assistant record".into());
        };
        assert_eq!(*stop, AssistantStop::Filter);
        assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());
        assert!(matches!(
            decode(line.replace("\"filter\"", "\"max_steps\"").as_bytes()),
            Err(DecodeError::Invalid { .. })
        ));
        Ok(())
    }

    #[test]
    fn fork_rejects_non_user_anchor_as_not_user_entry() -> Result<(), Box<dyn std::error::Error>> {
        let header = Header {
            id: SessionId::new_v7(),
            at: "2026-09-25T10:15:30.000Z".parse()?,
            workspace: Workspace::new("/w".into())?,
            product: Product::Dal,
            from: None,
        };
        let entry = Entry {
            id: EntryId::new(nz(7)),
            parent: None,
            at: "2026-09-25T10:15:30.000Z".parse()?,
            kind: EntryKind::Reminder {
                source: "rule:test".into(),
                text: "Reminder".into(),
            },
        };
        assert!(matches!(
            branch(
                &[Record::Reminder(entry)],
                None,
                BranchMode::Fork {
                    at: EntryId::new(nz(7))
                },
                &header
            ),
            Err(BranchError::NotUserEntry { entry }) if entry.get() == 7
        ));
        Ok(())
    }

    #[test]
    fn invalid_cost_is_rejected_not_nulled() -> Result<(), Box<dyn std::error::Error>> {
        let record = Record::TurnEnd {
            at: "2026-09-25T10:16:02.000Z".parse()?,
            turn: TurnId::new(nz(1)),
            stop: TurnEndStop::Done,
            usage: Some(Usage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: Some(f64::NAN),
            }),
            changes: Vec::new(),
        };
        assert!(matches!(encode(&record), Err(EncodeError::InvalidCost)));
        Ok(())
    }

    #[test]
    fn scan_head_reads_only_tree_prefixes() {
        let user = b"{\"v\":1,\"type\":\"user\",\"id\":4,\"parent\":3,\"at\":\"t\",\"parts\":[]}\n";
        let head = scan_head(user);
        assert_eq!(
            head.map(|head| (head.id.get(), head.parent.map(EntryId::get), head.kind)),
            Some((4, Some(3), TreeKind::User))
        );
        assert_eq!(scan_head(b"{\"v\":1,\"type\":\"leaf\",\"to\":1}"), None);
        assert_eq!(scan_head(b"{\"v\":1,\"type\":\"user\"}"), None);
    }

    #[test]
    fn branch_copies_path_and_labels() -> Result<(), Box<dyn std::error::Error>> {
        let header = Header {
            id: SessionId::new_v7(),
            at: "2026-09-25T10:15:30.000Z".parse()?,
            workspace: Workspace::new("/home/alpha/harness/reed".into())?,
            product: Product::Dal,
            from: None,
        };
        let records = vec![
            Record::User(user_entry(1, None, "first")),
            Record::User(user_entry(2, Some(1), "second")),
            Record::User(user_entry(3, Some(2), "third")),
            Record::Label {
                at: "2026-09-25T10:15:31.000Z".parse()?,
                entry: EntryId::new(nz(1)),
                label: Some("kept".into()),
            },
            Record::Label {
                at: "2026-09-25T10:15:32.000Z".parse()?,
                entry: EntryId::new(nz(3)),
                label: Some("dropped".into()),
            },
        ];
        let forked = branch(
            &records,
            Some(EntryId::new(nz(3))),
            BranchMode::Fork {
                at: EntryId::new(nz(2)),
            },
            &header,
        )?;
        let ids: Vec<u64> = forked
            .records
            .iter()
            .filter_map(|record| record.entry().map(|entry| entry.id.get()))
            .collect();
        assert_eq!(ids, vec![1]);
        assert_eq!(
            forked.records.len(),
            2,
            "the path entry and its label copy; the off-path label does not"
        );
        assert_eq!(
            forked.anchor_parts,
            vec![JournalPart::Text {
                text: "second".into()
            }]
        );
        let source = forked.header.from.ok_or("fork sets from")?;
        assert_eq!(source.session, header.id);
        assert_eq!(source.entry.map(EntryId::get), Some(2));
        let cloned = branch(
            &records,
            Some(EntryId::new(nz(3))),
            BranchMode::Clone,
            &header,
        )?;
        let ids: Vec<u64> = cloned
            .records
            .iter()
            .filter_map(|record| record.entry().map(|entry| entry.id.get()))
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(cloned.anchor_parts.is_empty());
        assert!(matches!(
            branch(&[], None, BranchMode::Clone, &header),
            Err(BranchError::NoEntries)
        ));
        assert!(matches!(
            branch(
                &records,
                Some(EntryId::new(nz(3))),
                BranchMode::Fork {
                    at: EntryId::new(nz(9)),
                },
                &header,
            ),
            Err(BranchError::UnknownEntry { .. })
        ));
        Ok(())
    }
}
