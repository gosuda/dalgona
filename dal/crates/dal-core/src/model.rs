//! Provider-neutral model values, raw-bearing stream parts, and token pricing.
//!
//! Routes describe a destination; provider lookup and alias resolution belong
//! to the provider layer. Prices are USD per million tokens. A reported cost
//! is authoritative, while absent or unusable prices remain unknown.

use std::sync::Arc;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use sonic_rs::JsonValueTrait;

use crate::content::Part;
use crate::id::CallId;
use crate::raw::{RawJson, Tagged};

/// The provider API family. These wire names differ from the variant names.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Family {
    /// Chat completions API.
    #[serde(rename = "openai_chat")]
    Chat,
    /// Responses API.
    #[serde(rename = "openai_responses")]
    Responses,
    /// Codex API.
    #[serde(rename = "openai_codex")]
    Codex,
    /// Anthropic Messages.
    #[serde(rename = "anthropic")]
    Anthropic,
}

/// Requested reasoning intensity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    /// Disable reasoning.
    Off,
    /// Minimal reasoning.
    Minimal,
    /// Low reasoning.
    Low,
    /// Medium reasoning.
    Medium,
    /// High reasoning.
    High,
    /// Extra-high reasoning.
    Xhigh,
    /// Maximum supported reasoning.
    Max,
}

/// A direct API model, a named synthetic model, or a built-in harness mode.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelRoute {
    /// Route directly to a provider API family and model name.
    Api {
        /// API family selected by the route.
        family: Family,
        /// Provider-native model name.
        model: Box<str>,
    },
    /// A composed model identified by `namespace/name`.
    Synthetic {
        /// Identifier matching `[a-z0-9-]+/[a-z0-9._-]+`.
        id: Box<str>,
    },
    /// One of the three built-in dalgon modes.
    Harness {
        /// `dalgon/normal`, `dalgon/eval-first`, or `dalgon/eval-only`.
        id: Box<str>,
    },
}

/// A synthetic or harness route id does not match its closed grammar.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RouteError {
    /// The synthetic identifier must have one slash and valid components.
    #[error("invalid synthetic model id `{id}`: expected `[a-z0-9-]+/[a-z0-9._-]+`")]
    InvalidSyntheticId {
        /// The rejected identifier.
        id: Box<str>,
    },
    /// Harness routes are limited to the three dalgon modes.
    #[error(
        "invalid harness model id `{id}`: expected dalgon/normal, dalgon/eval-first, or dalgon/eval-only"
    )]
    InvalidHarnessId {
        /// The rejected identifier.
        id: Box<str>,
    },
}

impl ModelRoute {
    /// Returns the provider model name or the synthetic/harness identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Api { model, .. } => model,
            Self::Synthetic { id } | Self::Harness { id } => id,
        }
    }

    /// Builds a synthetic route after checking its `namespace/name` grammar.
    ///
    /// # Errors
    /// Returns [`RouteError::InvalidSyntheticId`] for any other spelling.
    pub fn synthetic(id: impl Into<Box<str>>) -> Result<Self, RouteError> {
        let id = id.into();
        if !Self::is_valid_synthetic_id(&id) {
            return Err(RouteError::InvalidSyntheticId { id });
        }
        Ok(Self::Synthetic { id })
    }

    /// Builds one of the three built-in harness routes.
    ///
    /// # Errors
    /// Returns [`RouteError::InvalidHarnessId`] for an unknown mode.
    pub fn harness(id: impl Into<Box<str>>) -> Result<Self, RouteError> {
        let id = id.into();
        if !matches!(
            id.as_ref(),
            "dalgon/normal" | "dalgon/eval-first" | "dalgon/eval-only"
        ) {
            return Err(RouteError::InvalidHarnessId { id });
        }
        Ok(Self::Harness { id })
    }

    /// Checks the grammar `[a-z0-9-]+/[a-z0-9._-]+` without alias lookup.
    #[must_use]
    pub fn is_valid_synthetic_id(id: &str) -> bool {
        let Some((namespace, name)) = id.split_once('/') else {
            return false;
        };
        !namespace.is_empty()
            && namespace
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !name.is_empty()
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    }
}

#[derive(Deserialize)]
struct ApiRouteFields {
    family: Family,
    model: Box<str>,
}

#[derive(Deserialize)]
struct IdRouteFields {
    id: Box<str>,
}

impl<'de> Deserialize<'de> for ModelRoute {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "kind", &["api", "synthetic", "harness"])?;
        match tagged.kind() {
            "api" => {
                let fields: ApiRouteFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Api {
                    family: fields.family,
                    model: fields.model,
                })
            }
            "synthetic" => {
                let fields: IdRouteFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Self::synthetic(fields.id).map_err(de::Error::custom)
            }
            "harness" => {
                let fields: IdRouteFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Self::harness(fields.id).map_err(de::Error::custom)
            }
            other => Err(de::Error::custom(format!(
                "unknown model route kind `{other}`"
            ))),
        }
    }
}

/// Maximum number of routes on a synthetic expansion path.
pub const MAX_SYNTHETIC_DEPTH: usize = 4;

/// Rejects a resolution path longer than four or a repeated synthetic id.
///
/// The input is the complete path as currently known to the runtime. A path
/// longer than four routes is rejected before checking for repeated ids.
/// This check does no provider lookup or alias resolution.
///
/// # Errors
/// Returns [`InferFailure::SyntheticDepth`] beyond four routes, or
/// [`InferFailure::SyntheticCycle`] for a repeated synthetic id in an admitted
/// path.
pub fn check_synthetic_chain(chain: &[ModelRoute]) -> Result<(), InferFailure> {
    if chain.len() > MAX_SYNTHETIC_DEPTH {
        return Err(InferFailure::SyntheticDepth {
            chain: chain.to_vec(),
        });
    }
    for (index, route) in chain.iter().enumerate() {
        let ModelRoute::Synthetic { id } = route else {
            continue;
        };
        let repeated = chain[..index].iter().any(|previous| {
            matches!(previous, ModelRoute::Synthetic { id: previous_id } if previous_id == id)
        });
        if repeated {
            return Err(InferFailure::SyntheticCycle {
                chain: chain.to_vec(),
            });
        }
    }
    Ok(())
}

/// Capabilities offered by a concrete model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Caps {
    /// Maximum context size in tokens.
    pub context_window: u32,
    /// Supported reasoning levels.
    pub thinking: Box<[ThinkingLevel]>,
    /// Whether tools are supported.
    pub tool_use: bool,
    /// Whether image input is supported.
    pub image_input: bool,
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
struct UserFields {
    parts: Vec<Part>,
}
#[derive(Deserialize)]
struct AssistantFields {
    parts: Vec<AssistantPart>,
}
#[derive(Deserialize)]
struct ToolResultFields {
    call: CallId,
    name: Box<str>,
    is_error: bool,
    parts: Vec<Part>,
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
struct TextFields {
    text: Box<str>,
}
#[derive(Deserialize)]
struct ThinkingFields {
    text: Box<str>,
    replay: Option<RawJson>,
}
#[derive(Deserialize)]
struct ToolCallFields {
    call: CallId,
    name: Box<str>,
    args: RawJson,
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
    /// A usage measurement, with fields inline in the event object.
    Usage(Usage),
    /// A stop reason, encoded as a unit-variant member beside the tag.
    Stop(Stop),
}

#[derive(Deserialize)]
struct DeltaFields {
    channel: StreamChannel,
    text: Box<str>,
}

fn decode_stop_member<E: de::Error>(raw: &str) -> Result<Stop, E> {
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
            &["delta", "tool_call", "usage", "stop"],
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
            "usage" => sonic_rs::from_str(tagged.raw())
                .map(Self::Usage)
                .map_err(de::Error::custom),
            "stop" => decode_stop_member(tagged.raw()).map(Self::Stop),
            other => Err(de::Error::custom(format!(
                "unknown stream event type `{other}`"
            ))),
        }
    }
}

/// The destination of streamed delta text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamChannel {
    /// Assistant-visible text.
    Text,
    /// Reasoning text.
    Thinking,
    /// Arguments for a named tool.
    ToolArgs {
        /// Tool name.
        tool: Box<str>,
    },
}

/// Normalized token counts and optional provider-reported cost.
///
/// `input_tokens` includes `cached_input_tokens`; `output_tokens` excludes
/// `reasoning_tokens`; `cache_write_tokens` are included in uncached input.
/// If cached input exceeds total input, local pricing returns unknown.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Usage {
    /// All input tokens, including cache hits and cache writes.
    pub input_tokens: u64,
    /// Input tokens served from cache, a subset of `input_tokens`.
    pub cached_input_tokens: u64,
    /// Output tokens, excluding reasoning tokens.
    pub output_tokens: u64,
    /// Reasoning tokens if separately reported.
    pub reasoning_tokens: Option<u64>,
    /// Input tokens written to cache, already counted as uncached input.
    pub cache_write_tokens: u64,
    /// Provider-supplied USD cost, if reported.
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Returns the provider-reported cost first, then configured pricing,
    /// then compiled pricing; absent sources leave the cost unknown.
    ///
    /// An invalid provider cost or selected price is unknown rather than
    /// silently recomputed from a lower-priority source.
    #[must_use]
    pub fn cost_usd(
        &self,
        configured: Option<&ModelPrice>,
        compiled: Option<&ModelPrice>,
    ) -> Option<f64> {
        match self.cost_usd {
            Some(reported) if reported.is_finite() && reported >= 0.0 => Some(reported),
            Some(_) => None,
            None => configured
                .or(compiled)
                .and_then(|price| price.cost_usd(self)),
        }
    }
}

/// USD prices per million tokens for each disjoint usage component.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelPrice {
    /// Uncached input rate.
    pub input: f64,
    /// Cached input rate.
    pub cached_input: f64,
    /// Output rate, excluding reasoning.
    pub output: f64,
    /// Separately reported reasoning rate.
    pub reasoning: f64,
}

impl ModelPrice {
    /// Prices only disjoint counters; never double-counts cached input or
    /// reasoning. Zero rates are known prices, not missing prices.
    ///
    /// Returns `None` for negative or non-finite rates, cached-input counter
    /// underflow, or a non-finite computed total.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "token counters need conversion to USD rates; floating-point rounding is inherent to the price representation"
    )]
    pub fn cost_usd(&self, usage: &Usage) -> Option<f64> {
        let valid_rate = |rate: f64| rate.is_finite() && rate >= 0.0;
        if ![self.input, self.cached_input, self.output, self.reasoning]
            .into_iter()
            .all(valid_rate)
        {
            return None;
        }
        let uncached = usage.input_tokens.checked_sub(usage.cached_input_tokens)?;
        // USD rates are already f64; converting token counters intentionally
        // rounds values above the exact-integer range of f64.
        let total = (uncached as f64 * self.input
            + usage.cached_input_tokens as f64 * self.cached_input
            + usage.output_tokens as f64 * self.output
            + usage.reasoning_tokens.unwrap_or(0) as f64 * self.reasoning)
            / 1_000_000.0;
        total.is_finite().then_some(total)
    }
}

/// A provider-neutral inference request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelRequest {
    /// Why inference is being requested.
    pub purpose: Purpose,
    /// Route to invoke.
    pub model: ModelRoute,
    /// System instructions.
    pub system: Arc<str>,
    /// Tools visible to the model.
    pub tools: Arc<[ModelToolSpec]>,
    /// Ordered context messages.
    pub context: Arc<[ContextItem]>,
    /// Reasoning and sampling options.
    pub params: RequestParams,
    /// Provider cache key, if supplied.
    pub cache_key: Option<Box<str>>,
}

/// Normalized events returned by one inference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Inference {
    /// Events in stream order.
    pub events: Vec<StreamEvent>,
}

/// A failed or rejected inference.
#[non_exhaustive]
#[derive(Clone, Debug, thiserror::Error)]
pub enum InferFailure {
    /// Caller cancelled inference.
    #[error("inference was cancelled")]
    Cancelled,
    /// The provider or runtime failed.
    #[error("inference failed: {message}")]
    Failed {
        /// Failure description.
        message: Box<str>,
    },
    /// A synthetic route appeared twice on the same expansion path.
    #[error("synthetic model cycle: {chain:?}")]
    SyntheticCycle {
        /// Full path that contained the repeat.
        chain: Vec<ModelRoute>,
    },
    /// The expansion path exceeded [`MAX_SYNTHETIC_DEPTH`].
    #[error("synthetic model depth exceeds 4: {chain:?}")]
    SyntheticDepth {
        /// Full path that exceeded the limit.
        chain: Vec<ModelRoute>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn priced_usage() -> Usage {
        Usage {
            input_tokens: 2_000_000,
            cached_input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            reasoning_tokens: Some(500_000),
            cache_write_tokens: 100_000,
            cost_usd: None,
        }
    }

    fn price(input: f64, cached_input: f64, output: f64, reasoning: f64) -> ModelPrice {
        ModelPrice {
            input,
            cached_input,
            output,
            reasoning,
        }
    }

    #[test]
    fn reported_configured_and_compiled_cost_precedence() {
        let configured = price(3.0, 3.0, 3.0, 3.0);
        let compiled = price(1.0, 1.0, 1.0, 1.0);
        let usage = priced_usage();
        assert_eq!(usage.cost_usd(None, None), None);
        assert_eq!(usage.cost_usd(None, Some(&compiled)), Some(3.5));
        assert_eq!(
            usage.cost_usd(Some(&configured), Some(&compiled)),
            Some(10.5)
        );
        assert_eq!(
            usage.cost_usd(Some(&price(-1.0, 0.0, 0.0, 0.0)), Some(&compiled)),
            None
        );
        let reported = Usage {
            cost_usd: Some(7.25),
            ..usage
        };
        assert_eq!(
            reported.cost_usd(Some(&configured), Some(&compiled)),
            Some(7.25)
        );
        assert_eq!(
            Usage {
                cost_usd: Some(0.0),
                ..usage
            }
            .cost_usd(None, None),
            Some(0.0)
        );
        assert_eq!(
            Usage {
                cost_usd: Some(f64::NAN),
                ..usage
            }
            .cost_usd(Some(&configured), None),
            None
        );
    }

    #[test]
    fn usage_price_formula_does_not_double_count_cached_or_reasoning_tokens() {
        let usage = priced_usage();
        assert_eq!(price(1.0, 0.5, 2.0, 0.25).cost_usd(&usage), Some(3.625));
        assert_eq!(
            price(1.0, 1.0, 1.0, 1.0).cost_usd(&Usage {
                cache_write_tokens: 0,
                ..usage
            }),
            price(1.0, 1.0, 1.0, 1.0).cost_usd(&usage)
        );
        assert_eq!(
            price(1.0, 1.0, 1.0, 1.0).cost_usd(&Usage {
                cached_input_tokens: 2_000_001,
                ..usage
            }),
            None
        );
        assert_eq!(price(-1.0, 1.0, 1.0, 1.0).cost_usd(&usage), None);
        assert_eq!(price(f64::NAN, 1.0, 1.0, 1.0).cost_usd(&usage), None);
        assert_eq!(
            price(1e300, 1.0, 1.0, 1.0).cost_usd(&Usage {
                input_tokens: u64::MAX,
                cached_input_tokens: 0,
                ..usage
            }),
            None
        );
        let zero = Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        };
        assert_eq!(zero.cost_usd(None, None), None);
        assert_eq!(
            zero.cost_usd(Some(&price(0.0, 0.0, 0.0, 0.0)), None),
            Some(0.0)
        );
    }

    #[test]
    fn uniform_rate_is_independent_of_cached_split() {
        let uniform = price(1_000_000.0, 1_000_000.0, 1_000_000.0, 1_000_000.0);
        for input in [0u16, 1, 2, 7, 10_000] {
            for cached in [0u16, 1, 2, 7, 10_000] {
                for output in [0u16, 1, 7] {
                    let usage = Usage {
                        input_tokens: u64::from(input),
                        cached_input_tokens: u64::from(cached.min(input)),
                        output_tokens: u64::from(output),
                        reasoning_tokens: Some(7),
                        cache_write_tokens: 0,
                        cost_usd: None,
                    };
                    assert_eq!(
                        uniform.cost_usd(&usage),
                        Some(f64::from(input) + f64::from(output) + 7.0)
                    );
                }
            }
        }
    }

    #[test]
    fn routes_validate_id_grammars_and_wire_names() -> TestResult {
        for (family, wire) in [
            (Family::Chat, "\"openai_chat\""),
            (Family::Responses, "\"openai_responses\""),
            (Family::Codex, "\"openai_codex\""),
            (Family::Anthropic, "\"anthropic\""),
        ] {
            assert_eq!(sonic_rs::to_string(&family)?, wire);
            assert_eq!(sonic_rs::from_str::<Family>(wire)?, family);
        }
        for id in ["a/b", "a-0/b.c_0-1"] {
            assert_eq!(ModelRoute::synthetic(id)?.id(), id);
        }
        for id in ["", "a", "/b", "a/", "A/b", "a/B", "a/b/c", "a.b/c", "a/b c"] {
            assert!(
                ModelRoute::synthetic(id).is_err(),
                "accepted invalid id {id}"
            );
            assert!(
                sonic_rs::from_str::<ModelRoute>(&format!(r#"{{"kind":"synthetic","id":"{id}"}}"#))
                    .is_err()
            );
        }
        for id in ["dalgon/normal", "dalgon/eval-first", "dalgon/eval-only"] {
            assert_eq!(ModelRoute::harness(id)?.id(), id);
        }
        assert!(ModelRoute::harness("dalgon/other").is_err());
        assert!(
            sonic_rs::from_str::<ModelRoute>(r#"{"kind":"harness","id":"dalgon/other"}"#).is_err()
        );
        let api = ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-5".into(),
        };
        assert_eq!(api.id(), "gpt-5");
        assert_eq!(
            sonic_rs::to_string(&api)?,
            r#"{"kind":"api","family":"openai_chat","model":"gpt-5"}"#
        );
        assert_eq!(
            sonic_rs::from_str::<ModelRoute>(&sonic_rs::to_string(&api)?)?,
            api
        );
        Ok(())
    }

    #[test]
    fn synthetic_chain_rejects_depth_before_cycle_scan() -> TestResult {
        let routes: Vec<ModelRoute> = ["a/one", "a/two", "a/three", "a/four", "a/five"]
            .into_iter()
            .map(ModelRoute::synthetic)
            .collect::<Result<_, _>>()?;
        assert!(check_synthetic_chain(&routes[..4]).is_ok());
        assert!(
            matches!(check_synthetic_chain(&routes), Err(InferFailure::SyntheticDepth { chain }) if chain == routes)
        );
        let repeated = vec![routes[0].clone(), routes[1].clone(), routes[0].clone()];
        assert!(
            matches!(check_synthetic_chain(&repeated), Err(InferFailure::SyntheticCycle { chain }) if chain == repeated)
        );
        let long_invalid = vec![
            routes[0].clone(),
            routes[1].clone(),
            routes[2].clone(),
            routes[3].clone(),
            routes[0].clone(),
        ];
        assert!(
            matches!(check_synthetic_chain(&long_invalid), Err(InferFailure::SyntheticDepth { chain }) if chain == long_invalid)
        );
        assert!(check_synthetic_chain(&[]).is_ok());
        Ok(())
    }

    #[test]
    fn native_tags_preserve_raw_args_and_nested_replay() -> TestResult {
        let arg_json = r#"{"b":2, "a":1e+02}"#;
        let event_json =
            format!(r#"{{"type":"tool_call","call":"c1","name":"run","args":{arg_json}}}"#);
        let event: StreamEvent = sonic_rs::from_str(&event_json)?;
        assert!(matches!(&event, StreamEvent::ToolCall { args, .. } if args.as_str() == arg_json));
        let encoded = sonic_rs::to_string(&event)?;
        assert!(
            encoded.contains(arg_json),
            "raw arguments changed: {encoded}"
        );
        assert_eq!(sonic_rs::from_str::<StreamEvent>(&encoded)?, event);

        let nested_json = format!(
            r#"{{"role":"assistant","parts":[{{"type":"thinking","text":"plan","replay":{arg_json}}},{{"type":"tool_call","call":"c1","name":"run","args":{arg_json}}}]}}"#
        );
        let context: ContextItem = sonic_rs::from_str(&nested_json)?;
        let encoded = sonic_rs::to_string(&context)?;
        assert_eq!(
            encoded.matches(arg_json).count(),
            2,
            "raw nested values changed: {encoded}"
        );
        assert_eq!(sonic_rs::from_str::<ContextItem>(&encoded)?, context);

        let spec_json =
            format!(r#"{{"name":"run","description":"a tool","parameters":{arg_json}}}"#);
        let spec: ModelToolSpec = sonic_rs::from_str(&spec_json)?;
        assert!(sonic_rs::to_string(&spec)?.contains(arg_json));
        Ok(())
    }

    #[test]
    fn stream_usage_and_stop_keep_their_tagged_newtype_wire() -> TestResult {
        let usage = priced_usage();
        let event = StreamEvent::Usage(usage);
        let encoded = sonic_rs::to_string(&event)?;
        assert!(encoded.contains(r#""type":"usage""#));
        assert!(encoded.contains(r#""input_tokens":2000000"#));
        assert_eq!(sonic_rs::from_str::<StreamEvent>(&encoded)?, event);
        for reason in [
            Stop::EndTurn,
            Stop::Length,
            Stop::Filter,
            Stop::MaxSteps,
            Stop::Cancelled,
            Stop::Failed,
        ] {
            let event = StreamEvent::Stop(reason);
            let encoded = sonic_rs::to_string(&event)?;
            assert!(encoded.contains(r#""type":"stop""#), "{encoded}");
            assert_eq!(sonic_rs::from_str::<StreamEvent>(&encoded)?, event);
        }
        assert_eq!(
            sonic_rs::to_string(&StreamEvent::Stop(Stop::EndTurn))?,
            r#"{"type":"stop","end_turn":null}"#
        );
        Ok(())
    }

    #[test]
    fn malformed_model_discriminators_and_stop_reasons_fail() {
        for text in [
            r#"{"type":"tool_call","call":"c1","name":"run","args":{},"type":"delta"}"#,
            r#"{"type":"alien"}"#,
            r#"{"call":"c1","name":"run","args":{}}"#,
            r#"{"type":"stop"}"#,
            r#"{"type":"stop","end_turn":1}"#,
            r#"{"type":"stop","end_turn":null,"length":null}"#,
            r#"{"type":"delta"}"#,
        ] {
            assert!(
                sonic_rs::from_str::<StreamEvent>(text).is_err(),
                "accepted {text}"
            );
        }
        for text in [
            r#"{"role":"alien"}"#,
            r#"{"parts":[]}"#,
            r#"{"role":"user","role":"user","parts":[]}"#,
        ] {
            assert!(
                sonic_rs::from_str::<ContextItem>(text).is_err(),
                "accepted {text}"
            );
        }
        for text in [
            r#"{"type":"alien"}"#,
            r#"{"text":"hello"}"#,
            r#"{"type":"thinking","text":"hello","replay":{broken}}"#,
        ] {
            assert!(
                sonic_rs::from_str::<AssistantPart>(text).is_err(),
                "accepted {text}"
            );
        }
        for text in [
            r#"{"kind":"alien"}"#,
            r#"{"id":"a/b"}"#,
            r#"{"kind":"synthetic","kind":"synthetic","id":"a/b"}"#,
        ] {
            assert!(
                sonic_rs::from_str::<ModelRoute>(text).is_err(),
                "accepted {text}"
            );
        }
    }

    #[test]
    fn request_and_inference_round_trip_through_nested_carriers() -> TestResult {
        let request = ModelRequest {
            purpose: Purpose::Turn,
            model: ModelRoute::synthetic("pool/fast")?,
            system: Arc::from("be useful"),
            tools: Arc::from([ModelToolSpec {
                name: "run".into(),
                description: "executes".into(),
                parameters: RawJson::parse(r#"{"z":1e+02, "a":2}"#)?,
            }]),
            context: Arc::from([ContextItem::Assistant {
                parts: vec![AssistantPart::Thinking {
                    text: "plan".into(),
                    replay: Some(RawJson::parse(r#"{"z":1e+02, "a":2}"#)?),
                }],
            }]),
            params: RequestParams {
                thinking: ThinkingLevel::Xhigh,
                effort: Some("high".into()),
                temperature: Some(0.5),
            },
            cache_key: Some("scope".into()),
        };
        let encoded = sonic_rs::to_string(&request)?;
        assert_eq!(encoded.matches(r#"{"z":1e+02, "a":2}"#).count(), 2);
        assert_eq!(sonic_rs::from_str::<ModelRequest>(&encoded)?, request);
        let inference = Inference {
            events: vec![
                StreamEvent::Delta {
                    channel: StreamChannel::ToolArgs { tool: "run".into() },
                    text: "x".into(),
                },
                StreamEvent::Stop(Stop::EndTurn),
            ],
        };
        assert_eq!(
            sonic_rs::from_str::<Inference>(&sonic_rs::to_string(&inference)?)?,
            inference
        );
        Ok(())
    }
}
