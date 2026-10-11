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
    },
}

#[derive(Deserialize)]
pub(super) struct TextFields {
    pub(super) text: Box<str>,
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

impl<'de> Deserialize<'de> for AssistantPart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "type", &["text", "thinking", "tool_call"])?;
        match tagged.kind() {
            "text" => {
                let fields: TextFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Text { text: fields.text })
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
                let fields: ToolCallFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::ToolCall {
                    call: fields.call,
                    name: fields.name,
                    args: fields.args,
                })
            }
            other => Err(de::Error::custom(format!(
                "unknown assistant part type `{other}`"
            ))),
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
