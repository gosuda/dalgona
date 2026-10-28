use super::{
    ApprovalMode, CallId, Deserialize, EntryId, Family, FileChange, Mode, ModelRoute, RawJson,
    RouteError, Serialize, SessionId, Tagged, ThinkingLevel, Usage, Workspace,
};

/// The highest supported journal line format. Existing records retain format 1;
/// see [`Record::format`] for the kinds that need a later one.
pub const VERSION: u16 = 3;

/// The [`EntryKind::Reminder`] source of the text `before_turn` hooks add to a
/// turn. The session fold journals it and the model context reads it back as
/// user text.
pub const BEFORE_TURN_SOURCE: &str = "hook:before_turn";

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
/// A `text` block without a replay payload still writes `"replay":null`, as
/// in format 1; a `reasoning` block writes its provider replay payload, or
/// `"replay":null` when the provider streamed none. Only on a `reasoning`
/// block does the reader keep that `null`; a `text` or `tool_call` block reads
/// it as absent. A `tool_call` block omits an absent replay member.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Assistant-visible text; absent `replay` writes `null`, as in format 1.
    Text {
        /// The generated text.
        text: Box<str>,
        /// The opaque provider replay payload, when the provider returned one.
        replay: Option<RawJson>,
    },
    /// Reasoning text with its provider replay payload.
    Reasoning {
        /// The reasoning text.
        text: Box<str>,
        /// The opaque provider replay payload; `RawJson::null()` when the
        /// provider streamed reasoning text without one.
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
        /// The opaque provider replay payload, when the provider returned one.
        /// Omitted when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<RawJson>,
    },
}

// Blocks and entry kinds decode through the raw tagged carrier: their
// `RawJson` members cannot pass through serde's internally tagged
// content buffering. The wire structs below mirror the `Serialize`
// derive's member names exactly.
#[derive(Deserialize)]
pub(super) struct TextBlockFields {
    pub(super) text: Box<str>,
    #[serde(default)]
    pub(super) replay: Option<RawJson>,
}

#[derive(Deserialize)]
pub(super) struct ReasoningBlockFields {
    pub(super) text: Box<str>,
    pub(super) replay: RawJson,
}

#[derive(Deserialize)]
pub(super) struct ToolCallBlockFields {
    pub(super) id: CallId,
    pub(super) name: Box<str>,
    pub(super) input: RawJson,
    #[serde(default)]
    pub(super) replay: Option<RawJson>,
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "type", &["text", "reasoning", "tool_call"])?;
        match tagged.kind() {
            "text" => {
                let wire: TextBlockFields =
                    sonic_rs::from_str(tagged.raw()).map_err(serde::de::Error::custom)?;
                Ok(Self::Text {
                    text: wire.text,
                    replay: wire.replay,
                })
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
                    replay: wire.replay,
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

/// The durable provenance of one assistant response: which kind of model
/// route produced it.
///
/// The provenance never repeats the entry's `model` identifier; it names
/// only the route kind, and for [`AssistantRoute::Api`] the API route
/// family. That family is independent of the record's `api` wire member,
/// so a `Chat` route whose model id also parses as a synthetic name stays
/// distinguishable from a synthetic route on the same identifier. Absent
/// provenance means unknown, never API; legacy records predate the
/// member and stay valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AssistantRoute {
    /// The response came from a direct provider API route of this family.
    Api(Family),
    /// The response came from a named synthetic model.
    Synthetic,
    /// The response came from a built-in harness mode.
    Harness,
}

impl From<&ModelRoute> for AssistantRoute {
    fn from(route: &ModelRoute) -> Self {
        match route {
            ModelRoute::Api { family, .. } => Self::Api(*family),
            ModelRoute::Synthetic { .. } => Self::Synthetic,
            ModelRoute::Harness { .. } => Self::Harness,
        }
    }
}

impl AssistantRoute {
    pub(super) fn check_id(self, id: &str) -> Result<(), RouteError> {
        match self {
            Self::Api(_) => Ok(()),
            Self::Synthetic if ModelRoute::is_valid_synthetic_id(id) => Ok(()),
            Self::Harness if ModelRoute::is_valid_harness_id(id) => Ok(()),
            Self::Synthetic => Err(RouteError::InvalidSyntheticId { id: id.into() }),
            Self::Harness => Err(RouteError::InvalidHarnessId { id: id.into() }),
        }
    }
}

impl<'de> Deserialize<'de> for AssistantRoute {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum RouteField {
            Api,
            Synthetic,
            Harness,
        }

        struct AssistantRouteVisitor;

        impl<'de> serde::de::Visitor<'de> for AssistantRouteVisitor {
            type Value = AssistantRoute;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(
                    "an assistant route: `\"synthetic\"`, `\"harness\"`, or `{\"api\":<family>}`",
                )
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "synthetic" => Ok(AssistantRoute::Synthetic),
                    "harness" => Ok(AssistantRoute::Harness),
                    other => Err(E::custom(format!("unknown assistant route `{other}`"))),
                }
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let Some(member) = map.next_key::<RouteField>()? else {
                    return Err(serde::de::Error::custom(
                        "an assistant route object needs an `api` member",
                    ));
                };
                match member {
                    RouteField::Api => {
                        let family: Family = map.next_value()?;
                        if map.next_key::<RouteField>()?.is_some() {
                            return Err(serde::de::Error::custom(
                                "an assistant route object accepts only an `api` member",
                            ));
                        }
                        Ok(AssistantRoute::Api(family))
                    }
                    RouteField::Synthetic | RouteField::Harness => Err(serde::de::Error::custom(
                        "a unit assistant route accepts no payload",
                    )),
                }
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Err(E::custom("assistant route must not be null"))
            }
        }
        deserializer.deserialize_any(AssistantRouteVisitor)
    }
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
        /// The configured provider id that served this response, when known.
        ///
        /// Legacy records predate the member: it is omitted when unknown
        /// and absent means unattributed, never a default provider. A present
        /// id is preserved exactly and never inferred from `api`, `model`,
        /// or `route`; synthetic and harness routes must not name one.
        /// An explicit `null`, an empty id, or a non-string is rejected
        /// by the journal codec, like `route` and `replay`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<Box<str>>,
        /// The response content blocks.
        content: Vec<Block>,
        /// The normalized token usage.
        usage: Usage,
        /// Why the response ended.
        stop: AssistantStop,
        /// The route kind that produced this response, when known.
        ///
        /// Legacy records predate the member: it is omitted when unknown
        /// and absent means unknown, never API. A present route describes
        /// the `model` identifier and is independent of `api`.
        #[cfg_attr(feature = "schema", schemars(required))]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route: Option<AssistantRoute>,
        /// The opaque provider replay payload for the whole response, when the
        /// provider returned one.
        ///
        /// Omitted when absent; an explicit `null` is not a valid encoding of
        /// absence and is rejected by the journal codec, like `route`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<RawJson>,
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
        /// Milliseconds the tool ran, read from a monotonic clock around the
        /// execution. Approval waits are not counted. `None` when the call
        /// never ran or the journal predates the member.
        #[serde(skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u64>,
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
    /// The harness mode changed.
    Mode {
        /// The new mode.
        mode: Mode,
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
        /// Replacement context parts, when this is a local parts compaction.
        parts: Vec<JournalPart>,
        /// Token cost of the replacement parts.
        parts_tokens: u64,
    },
    /// A summary emitted for an abandoned branch.
    BranchSummary {
        /// The entry the summary describes.
        from: EntryId,
        /// The summary text.
        summary: Box<str>,
    },
}
