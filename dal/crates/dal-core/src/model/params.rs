use super::{
    AssistantPart, CallId, Deserialize, Deserializer, ModelRoute, Part, RawJson, ReplaySource,
    Serialize, Tagged, ThinkingLevel, de,
};

/// Capabilities offered by a concrete model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Caps {
    /// Known maximum context size in tokens; `None` means the source did not provide one.
    pub context_window: Option<u32>,
    /// Supported reasoning levels.
    pub thinking: Box<[ThinkingLevel]>,
    /// Whether tools are supported.
    pub tool_use: bool,
    /// Whether image input is supported.
    pub image_input: bool,
    /// Whether the provider accepts constrained custom-grammar tools.
    #[serde(default)]
    pub custom_grammar: bool,
}

/// Display metadata and capabilities for a model route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelInfo {
    /// Resolved route.
    pub route: ModelRoute,
    /// Human-readable model name.
    pub name: Box<str>,
    /// Supported operations and limits.
    pub caps: Caps,
}

/// Tuning options supplied with an inference request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RequestParams {
    /// Requested reasoning level.
    pub thinking: ThinkingLevel,
    /// Optional provider-specific effort name.
    pub effort: Option<Box<str>>,
    /// Optional sampling temperature.
    pub temperature: Option<f64>,
    /// Optional maximum output token count for this request.
    pub max_output_tokens: Option<u32>,
}

impl Default for RequestParams {
    fn default() -> Self {
        Self {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        }
    }
}

impl RequestParams {
    /// Returns these parameters with a request-local output token cap.
    #[must_use]
    pub const fn with_max_output_tokens(mut self, max_output_tokens: u32) -> Self {
        self.max_output_tokens = Some(max_output_tokens);
        self
    }
}

/// Why the model is being invoked.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// Normal conversation turn.
    Turn,
    /// Context compaction.
    Compact,
    /// Judgment or evaluation.
    Judge,
    /// Child task invocation.
    Child,
}

/// Provider-facing tool declaration with an unmodified JSON schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelToolSpec {
    /// Tool name.
    pub name: Box<str>,
    /// Tool description.
    pub description: Box<str>,
    /// Raw JSON schema, preserving its spelling and member order.
    pub parameters: RawJson,
    /// Optional constrained custom-grammar schema.
    #[serde(default)]
    pub grammar: Option<Box<str>>,
}

/// One message or tool response in the model context.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ContextItem {
    /// A user message.
    User {
        /// Text, images, or stored content references.
        parts: Vec<Part>,
    },
    /// A previously produced assistant message.
    Assistant {
        /// The producing provider family and model for every part in this message.
        source: ReplaySource,
        /// Text, reasoning, and tool calls in document order.
        parts: Vec<AssistantPart>,
    },
    /// The result of a tool call.
    ToolResult {
        /// Id of the call receiving this result.
        call: CallId,
        /// Tool name.
        name: Box<str>,
        /// Whether the tool returned an error.
        is_error: bool,
        /// Text, images, or stored content references.
        parts: Vec<Part>,
    },
}

#[derive(Deserialize)]
pub(super) struct UserFields {
    pub(super) parts: Vec<Part>,
}
#[derive(Deserialize)]
pub(super) struct AssistantFields {
    pub(super) source: ReplaySource,
    pub(super) parts: Vec<AssistantPart>,
}
#[derive(Deserialize)]
pub(super) struct ToolResultFields {
    pub(super) call: CallId,
    pub(super) name: Box<str>,
    pub(super) is_error: bool,
    pub(super) parts: Vec<Part>,
}

impl<'de> Deserialize<'de> for ContextItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "role", &["user", "assistant", "tool_result"])?;
        match tagged.kind() {
            "user" => {
                let fields: UserFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::User {
                    parts: fields.parts,
                })
            }
            "assistant" => {
                let fields: AssistantFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Assistant {
                    source: fields.source,
                    parts: fields.parts,
                })
            }
            "tool_result" => {
                let fields: ToolResultFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::ToolResult {
                    call: fields.call,
                    name: fields.name,
                    is_error: fields.is_error,
                    parts: fields.parts,
                })
            }
            other => Err(de::Error::custom(format!("unknown context role `{other}`"))),
        }
    }
}
