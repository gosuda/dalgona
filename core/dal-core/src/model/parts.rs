use sonic_rs::JsonValueTrait;

use super::{
    CallId, CompactOutcome, Deserialize, Deserializer, RawJson, Serialize, StreamChannel, Tagged,
    Usage, de,
};

/// One assistant text, reasoning, or tool-call part.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantPart {
    /// Assistant-visible text.
    Text {
        /// Generated text.
        text: Box<str>,
        /// Unmodified provider replay payload for this text, if available.
        ///
        /// Omitted from the serialized form when absent; an explicit `null`
        /// decodes as absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<RawJson>,
    },
    /// Reasoning text with optional provider replay data.
    Thinking {
        /// Reasoning text.
        text: Box<str>,
        /// Unmodified provider replay payload, if available.
        replay: Option<RawJson>,
    },
    /// A request to execute a tool.
    ToolCall {
        /// Correlates the result with this call.
        call: CallId,
        /// Tool name.
        name: Box<str>,
        /// Unmodified tool arguments JSON.
        args: RawJson,
        /// Unmodified provider replay payload for this call, if available.
        ///
        /// Omitted from the serialized form when absent; an explicit `null`
        /// decodes as absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<RawJson>,
    },
}

#[derive(Deserialize)]
pub(super) struct TextFields {
    pub(super) text: Box<str>,
    #[serde(default)]
    pub(super) replay: Option<RawJson>,
}
#[derive(Deserialize)]
pub(super) struct ThinkingFields {
    pub(super) text: Box<str>,
    pub(super) replay: Option<RawJson>,
}
#[derive(Deserialize)]
pub(super) struct ToolCallFields {
    pub(super) call: CallId,
    pub(super) name: Box<str>,
    pub(super) args: RawJson,
}
/// The assistant-part tool call members: [`ToolCallFields`] plus replay. A
/// stream `tool_call` event carries no replay member, so it keeps the
/// narrower struct.
#[derive(Deserialize)]
pub(super) struct AssistantToolCallFields {
    pub(super) call: CallId,
    pub(super) name: Box<str>,
    pub(super) args: RawJson,
    #[serde(default)]
    pub(super) replay: Option<RawJson>,
}

impl<'de> Deserialize<'de> for AssistantPart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "type", &["text", "thinking", "tool_call"])?;
        match tagged.kind() {
            "text" => {
                let fields: TextFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Text {
                    text: fields.text,
                    replay: fields.replay,
                })
            }
            "thinking" => {
                let fields: ThinkingFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Thinking {
                    text: fields.text,
                    replay: fields.replay,
                })
            }
            "tool_call" => {
                let fields: AssistantToolCallFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::ToolCall {
                    call: fields.call,
                    name: fields.name,
                    args: fields.args,
                    replay: fields.replay,
                })
            }
            other => Err(de::Error::custom(format!(
                "unknown assistant part type `{other}`"
            ))),
        }
    }
}

/// The outcome of capturing exact provider replay data for one response.
///
/// A capture is either the payload to journal and send back verbatim, or a
/// typed refusal. A refusal carries no payload and is never a success:
/// consumers handle both cases explicitly. The discriminator is the nested
/// `status` member, so a capture embeds in a `type`-tagged event without a
/// tag collision.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReplayCapture {
    /// The records were retained exactly.
    Captured {
        /// Unmodified provider replay JSON.
        ///
        /// Callers must never store `null` here: a null payload journals as
        /// an absent replay, which the journal codec rejects, leaving an
        /// unreadable line. Decode enforces this; construction is the
        /// caller's obligation.
        payload: RawJson,
    },
    /// The records could not be retained exactly.
    Refused {
        /// Why the capture was refused.
        reason: ReplayRefusal,
    },
}

#[derive(Deserialize)]
struct CapturedFields {
    payload: RawJson,
}

#[derive(Deserialize)]
struct RefusedFields {
    reason: ReplayRefusal,
}

fn decode_captured<E: de::Error>(raw: &str) -> Result<ReplayCapture, E> {
    let fields: CapturedFields = sonic_rs::from_str(raw).map_err(E::custom)?;
    // A `null` payload would journal as an absent replay and lose the
    // capture silently.
    if fields.payload.as_str() == "null" {
        return Err(E::custom("captured replay payload must not be null"));
    }
    Ok(ReplayCapture::Captured {
        payload: fields.payload,
    })
}

fn decode_refused<E: de::Error>(raw: &str) -> Result<ReplayCapture, E> {
    let fields: RefusedFields = sonic_rs::from_str(raw).map_err(E::custom)?;
    Ok(ReplayCapture::Refused {
        reason: fields.reason,
    })
}

impl<'de> Deserialize<'de> for ReplayCapture {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "status", &["captured", "refused"])?;
        match tagged.kind() {
            "captured" => decode_captured(tagged.raw()),
            "refused" => decode_refused(tagged.raw()),
            other => Err(de::Error::custom(format!(
                "unknown replay capture status `{other}`"
            ))),
        }
    }
}

/// Why exact replay data could not be retained.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReplayRefusal {
    /// A server record is not valid UTF-8.
    RecordNotUtf8,
    /// A server record does not re-encode to its received bytes.
    RecordNotRoundtrip,
    /// A server record is not valid JSON.
    RecordNotJson,
    /// The record of the user message that opened the turn was not found.
    UserRecordNotFound,
    /// The run produced no server records.
    NoRecords,
    /// The final checkpoint does not cover the whole turn.
    CheckpointNotCoveringTurn,
    /// The records span more than one wire route.
    RouteMixed,
    /// The encoded journal line would exceed the record bound. Produced only
    /// by the session fold, never by an adapter.
    LineBound,
}

impl ReplayRefusal {
    /// The stable `snake_case` token, identical to the serde spelling.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::RecordNotUtf8 => "record_not_utf8",
            Self::RecordNotRoundtrip => "record_not_roundtrip",
            Self::RecordNotJson => "record_not_json",
            Self::UserRecordNotFound => "user_record_not_found",
            Self::NoRecords => "no_records",
            Self::CheckpointNotCoveringTurn => "checkpoint_not_covering_turn",
            Self::RouteMixed => "route_mixed",
            Self::LineBound => "line_bound",
        }
    }
}

/// Why a model stream ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// The assistant finished the turn.
    EndTurn,
    /// The model hit a length limit.
    Length,
    /// A provider filter stopped output.
    Filter,
    /// The runtime reached its step limit.
    MaxSteps,
    /// The call was cancelled.
    Cancelled,
    /// The call failed.
    Failed,
}

/// One normalized model stream event.
///
/// `Stop(Stop::EndTurn)` keeps serde's tagged-newtype shape
/// `{"type":"stop","end_turn":null}`; `Usage` fields are inline beside
/// `"type":"usage"`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Incremental text on one channel.
    Delta {
        /// Text, thinking, or a tool's arguments.
        channel: StreamChannel,
        /// Newly generated text.
        text: Box<str>,
    },
    /// A completed tool-call event.
    ToolCall {
        /// Correlates the result with this call.
        call: CallId,
        /// Tool name.
        name: Box<str>,
        /// Unmodified tool arguments JSON.
        args: RawJson,
    },
    /// An opaque provider reasoning payload for replay, kept byte for byte.
    ThinkingReplay {
        /// Unmodified provider replay JSON.
        payload: RawJson,
    },
    /// A usage measurement, with fields inline in the event object.
    Usage(Usage),
    /// A stop reason, encoded as a unit-variant member beside the tag.
    Stop(Stop),
    /// A native-compaction result, terminal for a `Purpose::Compact` stream.
    Compaction {
        /// The provider's opaque result or typed unsupported response.
        outcome: CompactOutcome,
    },
    /// The exact provider replay capture for the response: its payload or a
    /// typed refusal.
    ///
    /// A turn stream carries at most one, after its tool calls and before its
    /// usage. Consumers handle `Captured` and `Refused` explicitly.
    MessageReplay {
        /// The captured payload or the typed refusal.
        capture: ReplayCapture,
    },
}

#[derive(Deserialize)]
pub(super) struct DeltaFields {
    pub(super) channel: StreamChannel,
    pub(super) text: Box<str>,
}

#[derive(Deserialize)]
pub(super) struct ThinkingReplayFields {
    pub(super) payload: RawJson,
}

#[derive(Deserialize)]
pub(super) struct CompactionFields {
    pub(super) outcome: CompactOutcome,
}

#[derive(Deserialize)]
pub(super) struct MessageReplayFields {
    pub(super) capture: ReplayCapture,
}

pub(super) fn decode_stop_member<E: de::Error>(raw: &str) -> Result<Stop, E> {
    let mut found = None;
    for member in sonic_rs::to_object_iter(raw) {
        let (name, value) = member.map_err(E::custom)?;
        let stop = match name.as_ref() {
            "end_turn" => Stop::EndTurn,
            "length" => Stop::Length,
            "filter" => Stop::Filter,
            "max_steps" => Stop::MaxSteps,
            "cancelled" => Stop::Cancelled,
            "failed" => Stop::Failed,
            _ => continue,
        };
        if found.is_some() {
            return Err(E::custom("duplicate stop reason member"));
        }
        if !value.is_null() {
            return Err(E::custom(format!("stop reason `{name}` must be null")));
        }
        found = Some(stop);
    }
    found.ok_or_else(|| E::custom("missing stop reason member"))
}

impl<'de> Deserialize<'de> for StreamEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(
            deserializer,
            "type",
            &[
                "delta",
                "tool_call",
                "thinking_replay",
                "usage",
                "stop",
                "compaction",
                "message_replay",
            ],
        )?;
        match tagged.kind() {
            "delta" => {
                let fields: DeltaFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Delta {
                    channel: fields.channel,
                    text: fields.text,
                })
            }
            "tool_call" => {
                let fields: ToolCallFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::ToolCall {
                    call: fields.call,
                    name: fields.name,
                    args: fields.args,
                })
            }
            "thinking_replay" => {
                let fields: ThinkingReplayFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::ThinkingReplay {
                    payload: fields.payload,
                })
            }
            "usage" => sonic_rs::from_str(tagged.raw())
                .map(Self::Usage)
                .map_err(de::Error::custom),
            "stop" => decode_stop_member(tagged.raw()).map(Self::Stop),
            "message_replay" => {
                let fields: MessageReplayFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::MessageReplay {
                    capture: fields.capture,
                })
            }
            "compaction" => {
                let fields: CompactionFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Compaction {
                    outcome: fields.outcome,
                })
            }
            other => Err(de::Error::custom(format!(
                "unknown stream event type `{other}`"
            ))),
        }
    }
}

#[cfg(test)]
mod replay_capture_tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const REFUSALS: [ReplayRefusal; 8] = [
        ReplayRefusal::RecordNotUtf8,
        ReplayRefusal::RecordNotRoundtrip,
        ReplayRefusal::RecordNotJson,
        ReplayRefusal::UserRecordNotFound,
        ReplayRefusal::NoRecords,
        ReplayRefusal::CheckpointNotCoveringTurn,
        ReplayRefusal::RouteMixed,
        ReplayRefusal::LineBound,
    ];

    #[test]
    fn unknown_status_is_rejected() {
        let error =
            sonic_rs::from_str::<ReplayCapture>(r#"{"status":"lost","reason":"no_records"}"#)
                .expect_err("unknown status must fail");
        assert!(
            error
                .to_string()
                .contains("unknown `status` variant `lost`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_status_is_rejected() {
        let error = sonic_rs::from_str::<ReplayCapture>(r#"{"payload":{"type":"cursor_history"}}"#)
            .expect_err("missing status must fail");
        assert!(
            error.to_string().contains("missing field `status`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn captured_null_payload_is_rejected() {
        let error = sonic_rs::from_str::<ReplayCapture>(r#"{"status":"captured","payload":null}"#)
            .expect_err("null captured payload must fail");
        assert!(
            error.to_string().contains("must not be null"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn refused_rejects_unknown_reason() {
        let error =
            sonic_rs::from_str::<ReplayCapture>(r#"{"status":"refused","reason":"line_bounded"}"#)
                .expect_err("unknown reason token must fail");
        assert!(
            error.to_string().contains("unknown variant"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn refusal_tokens_match_serde_spelling() -> TestResult {
        for refusal in REFUSALS {
            let wire = sonic_rs::to_string(&refusal)?;
            assert_eq!(wire, format!("\"{}\"", refusal.token()));
            let back: ReplayRefusal = sonic_rs::from_str(&wire)?;
            assert_eq!(back, refusal);
        }
        Ok(())
    }

    #[test]
    fn message_replay_requires_capture() {
        sonic_rs::from_str::<StreamEvent>(r#"{"type":"message_replay"}"#)
            .expect_err("message replay without capture must fail");
    }
}
