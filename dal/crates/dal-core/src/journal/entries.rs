use super::{
    ApprovalMode, AssistantStop, Block, CallId, ClientId, Deserialize, EntryId, EntryKind, Family,
    FileChange, JournalPart, MailMode, Mode, ModelRoute, RawJson, Serialize, SessionId, Tagged,
    ThinkingLevel, Usage,
};

#[derive(Deserialize)]
pub(super) struct UserEntryFields {
    pub(super) parts: Vec<JournalPart>,
}

#[derive(Deserialize)]
pub(super) struct AssistantEntryFields {
    pub(super) api: Family,
    pub(super) model: Box<str>,
    pub(super) content: Vec<Block>,
    pub(super) usage: Usage,
    pub(super) stop: AssistantStop,
}

#[derive(Deserialize)]
pub(super) struct ToolResultEntryFields {
    pub(super) call: CallId,
    pub(super) name: Box<str>,
    pub(super) error: bool,
    pub(super) parts: Vec<JournalPart>,
    pub(super) changes: Vec<FileChange>,
}

#[derive(Deserialize)]
pub(super) struct ReminderEntryFields {
    pub(super) source: Box<str>,
    pub(super) text: Box<str>,
}

#[derive(Deserialize)]
pub(super) struct ModelEntryFields {
    pub(super) route: ModelRoute,
}

#[derive(Deserialize)]
pub(super) struct ThinkingEntryFields {
    pub(super) level: ThinkingLevel,
}

#[derive(Deserialize)]
pub(super) struct ApprovalEntryFields {
    pub(super) mode: ApprovalMode,
}

#[derive(Deserialize)]
pub(super) struct ModeEntryFields {
    pub(super) mode: Mode,
}

#[derive(Deserialize)]
pub(super) struct CompactionEntryFields {
    pub(super) summary: Option<Box<str>>,
    pub(super) first_kept: Option<EntryId>,
    pub(super) tokens_before: u64,
    pub(super) replay: Option<RawJson>,
    pub(super) usage: Option<Usage>,
    #[serde(default)]
    pub(super) parts: Vec<JournalPart>,
    #[serde(default)]
    pub(super) parts_tokens: u64,
}

#[derive(Deserialize)]
pub(super) struct BranchSummaryEntryFields {
    pub(super) from: EntryId,
    pub(super) summary: Box<str>,
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
                "mode",
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
            other => decode_trailing_entry::<D>(&tagged, other),
        }
    }
}

/// Decodes the trailing entry kinds behind the [`Tagged`] carrier.
pub(super) fn decode_trailing_entry<'de, D: serde::Deserializer<'de>>(
    tagged: &Tagged<'_>,
    kind: &str,
) -> Result<EntryKind, D::Error> {
    match kind {
        "model" => {
            let wire: ModelEntryFields =
                sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
            if matches!(&wire.route, ModelRoute::Api { model, .. } if model.is_empty()) {
                return Err(serde::de::Error::custom(
                    "model route `model` must not be empty",
                ));
            }
            Ok(EntryKind::Model { route: wire.route })
        }
        "thinking" => {
            let wire: ThinkingEntryFields =
                sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
            Ok(EntryKind::Thinking { level: wire.level })
        }
        "approval" => {
            let wire: ApprovalEntryFields =
                sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
            Ok(EntryKind::Approval { mode: wire.mode })
        }
        "mode" => {
            let wire: ModeEntryFields =
                sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
            Ok(EntryKind::Mode { mode: wire.mode })
        }
        "compaction" => {
            let wire: CompactionEntryFields =
                sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
            Ok(EntryKind::Compaction {
                summary: wire.summary,
                first_kept: wire.first_kept,
                tokens_before: wire.tokens_before,
                replay: wire.replay,
                usage: wire.usage,
                parts: wire.parts,
                parts_tokens: wire.parts_tokens,
            })
        }
        "branch_summary" => {
            let wire: BranchSummaryEntryFields =
                sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
            Ok(EntryKind::BranchSummary {
                from: wire.from,
                summary: wire.summary,
            })
        }
        other => Err(serde::de::Error::custom(format!(
            "unknown entry kind `{other}`"
        ))),
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
