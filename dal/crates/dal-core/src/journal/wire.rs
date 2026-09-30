use super::wire_entries::BlockWire;
use super::{
    Answer, AssistantStop, Block, EncodeError, Entry, InferredPurpose, JobOutcome, JournalPart,
    Owner, RawJson, Record, Serialize, TurnEndStop, Usage, fmt,
};

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
            Self::Mode(_) => "mode",
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
            | Self::Mode(entry)
            | Self::Compaction(entry)
            | Self::BranchSummary(entry) => Some(entry),
            _ => None,
        }
    }
}

// Encode side: ordered member structs per record, hand-rolled lenses for
// the members whose durable literals differ from the wire shapes.

pub(super) fn encode_json<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, EncodeError> {
    sonic_rs::to_vec(value).map_err(|error| EncodeError::Json {
        message: error.to_string().into(),
    })
}

/// Timestamps write with exactly three fractional digits. Every
/// canonical vector carries millisecond precision, and the stock
/// `jiff::Timestamp` serializer prints no fraction on integral seconds,
/// which would break the byte round-trip.
pub(super) struct Millis<'a>(pub(super) &'a jiff::Timestamp);

impl fmt::Display for Millis<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:.3}", self.0)
    }
}

pub(super) struct TsWire<'a>(pub(super) &'a jiff::Timestamp);

impl Serialize for TsWire<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Millis(self.0))
    }
}

/// The journal `answer` member: a bare literal or `{"value": <json>}`.
pub(super) struct AnswerWire<'a>(pub(super) &'a Answer);

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
pub(super) struct OwnerWire<'a>(pub(super) &'a Owner);

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
pub(super) struct PurposeWire<'a>(pub(super) &'a InferredPurpose);

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
pub(super) struct UsageWire<'a> {
    pub(super) usage: &'a Usage,
    pub(super) cost_micro_usd: Option<u64>,
}

impl<'a> UsageWire<'a> {
    /// Precomputes the micro-dollar member so the `Serialize` impl stays
    /// infallible and `InvalidCost` reaches `encode`'s caller unchanged.
    pub(super) fn new(usage: &'a Usage) -> Result<Self, EncodeError> {
        Ok(Self {
            usage,
            cost_micro_usd: micro_cost(usage)?,
        })
    }
}

pub(super) fn micro_cost(usage: &Usage) -> Result<Option<u64>, EncodeError> {
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
pub(super) struct AssistantStopWire<'a>(pub(super) &'a AssistantStop);

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
pub(super) struct TurnEndStopWire<'a>(pub(super) &'a TurnEndStop);

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
pub(super) struct JobOutcomeWire<'a>(pub(super) &'a JobOutcome);

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

pub(super) struct JournalPartsWire<'a>(pub(super) &'a [JournalPart]);

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

pub(super) struct JournalPartWire<'a>(pub(super) &'a JournalPart);

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
pub(super) struct StoredBlobMember<'a> {
    pub(super) r#type: &'static str,
    pub(super) mime: &'a str,
    pub(super) blob: &'a str,
    pub(super) bytes: u64,
}

impl<'a> StoredBlobMember<'a> {
    pub(super) const fn new(
        r#type: &'static str,
        mime: &'a str,
        blob: &'a str,
        bytes: u64,
    ) -> Self {
        Self {
            r#type,
            mime,
            blob,
            bytes,
        }
    }
}

pub(super) struct AssistantBlocksWire<'a>(pub(super) &'a [Block]);

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
