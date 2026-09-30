use super::{
    Arc, ContextItem, Deserialize, Duration, Family, ModelRoute, ModelToolSpec, Purpose, RawJson,
    RequestParams, Serialize, StreamEvent,
};

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

/// The result of a provider-native compaction request.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum CompactOutcome {
    /// Native compacted history, still bound to the route that produced it.
    Compacted(CompactedHistory),
    /// The provider route has no native compaction endpoint.
    Unsupported,
}

/// Opaque provider-native compacted history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CompactedHistory {
    /// The family that produced this native history.
    pub family: Family,
    /// The exact model that produced this native history.
    pub model: Box<str>,
    /// Opaque provider items, retained byte for byte.
    pub items: Vec<RawJson>,
}

/// A failed or rejected inference, classified for the fold.
///
/// The fold decides the overflow path from [`InferFailure::Overflow`] and
/// leaves the retry table to the actor for [`InferFailure::Retryable`];
/// [`InferFailure::Fatal`] ends the turn. Each provider-classified variant
/// displays exactly its `message`: the provider layer's final rendered text,
/// which never carries a token, a key, or an authorization header value.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum InferFailure {
    /// Caller cancelled inference.
    #[error("inference was cancelled")]
    Cancelled,
    /// The provider reported, by a typed error code, that the request
    /// exceeds the model's context window. Never retried as-is: the fold
    /// compacts once and resends.
    #[error("{message}")]
    Overflow {
        /// The provider error code that identified the overflow.
        code: Box<str>,
        /// The rendered failure text.
        message: Box<str>,
    },
    /// A transient provider failure the actor's retry table may resend.
    #[error("{message}")]
    Retryable {
        /// The wait the provider asked for, when it stated one.
        hint: Option<Duration>,
        /// The rendered failure text.
        message: Box<str>,
    },
    /// A failure that no resend of the same request can fix.
    #[error("{message}")]
    Fatal {
        /// The rendered failure text.
        message: Box<str>,
        /// The one next-action sentence, when the provider layer has one.
        fix: Option<Box<str>>,
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
