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

/// One request-wide price tier selected by the total prompt token count.
///
/// A tier applies when the prompt has more than `size` tokens. The tier
/// replaces only the rates that it provides; omitted rates keep the model's
/// base price. Tiers are selected per request, not per token band.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PriceTier {
    /// The largest prompt size that stays on the preceding rate card.
    pub size: u64,
    /// Uncached input rate, when this tier changes it.
    #[serde(default)]
    pub input: Option<f64>,
    /// Cached input rate, when this tier changes it.
    #[serde(default)]
    pub cached_input: Option<f64>,
    /// Output rate, when this tier changes it.
    #[serde(default)]
    pub output: Option<f64>,
    /// Reasoning rate, when this tier changes it.
    #[serde(default)]
    pub reasoning: Option<f64>,
}

/// USD prices per million tokens for each disjoint usage component.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Request-wide context tiers, in any order.
    #[serde(default)]
    pub tiers: Box<[PriceTier]>,
}

impl ModelPrice {
    /// Prices only disjoint counters; never double-counts cached input or
    /// reasoning. Zero rates are known prices, not missing prices.
    ///
    /// Tiers select one complete request-wide rate card using
    /// [`Usage::input_tokens`]. That counter already includes cached input and
    /// cache writes, so cache counters are not added again.
    ///
    /// Returns `None` for negative or non-finite rates, cached-input counter
    /// underflow, or a non-finite computed total.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "token counters need conversion to USD rates; floating-point rounding is inherent to the price representation"
    )]
    pub fn cost_usd(&self, usage: &Usage) -> Option<f64> {
        let (input, cached_input, output, reasoning) = self.rates_for(usage.input_tokens);
        let valid_rate = |rate: f64| rate.is_finite() && rate >= 0.0;
        if ![input, cached_input, output, reasoning]
            .into_iter()
            .all(valid_rate)
        {
            return None;
        }
        let uncached = usage.input_tokens.checked_sub(usage.cached_input_tokens)?;
        // USD rates are already f64; converting token counters intentionally
        // rounds values above the exact-integer range of f64.
        let total = (uncached as f64 * input
            + usage.cached_input_tokens as f64 * cached_input
            + usage.output_tokens as f64 * output
            + usage.reasoning_tokens.unwrap_or(0) as f64 * reasoning)
            / 1_000_000.0;
        total.is_finite().then_some(total)
    }

    fn rates_for(&self, prompt_tokens: u64) -> (f64, f64, f64, f64) {
        let mut selected = None;
        for tier in &self.tiers {
            if prompt_tokens > tier.size
                && selected.is_none_or(|selected: &PriceTier| tier.size > selected.size)
            {
                selected = Some(tier);
            }
        }
        let Some(tier) = selected else {
            return (self.input, self.cached_input, self.output, self.reasoning);
        };
        (
            tier.input.unwrap_or(self.input),
            tier.cached_input.unwrap_or(self.cached_input),
            tier.output.unwrap_or(self.output),
            tier.reasoning.unwrap_or(self.reasoning),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelPrice, PriceTier, Usage};

    fn price() -> ModelPrice {
        ModelPrice {
            input: 1.0,
            cached_input: 1.0,
            output: 2.0,
            reasoning: 3.0,
            tiers: vec![
                PriceTier {
                    size: 100,
                    input: Some(10.0),
                    cached_input: Some(4.0),
                    output: Some(20.0),
                    reasoning: Some(30.0),
                },
                PriceTier {
                    size: 200,
                    input: Some(20.0),
                    cached_input: Some(8.0),
                    output: None,
                    reasoning: None,
                },
            ]
            .into_boxed_slice(),
        }
    }

    #[test]
    fn request_wide_tiers_use_the_highest_exceeded_context_boundary() {
        let price = price();
        let usage = Usage {
            input_tokens: 99,
            cached_input_tokens: 0,
            output_tokens: 100,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        };
        assert_eq!(price.cost_usd(&usage), Some(0.000_299));

        let at_first_boundary = Usage {
            input_tokens: 100,
            ..usage
        };
        assert_eq!(price.cost_usd(&at_first_boundary), Some(0.0003));

        let above_first_boundary = Usage {
            input_tokens: 101,
            ..usage
        };
        assert_eq!(price.cost_usd(&above_first_boundary), Some(0.00301));

        let before_second_boundary = Usage {
            input_tokens: 199,
            ..usage
        };
        assert_eq!(price.cost_usd(&before_second_boundary), Some(0.00399));

        let at_second_boundary = Usage {
            input_tokens: 200,
            ..usage
        };
        assert_eq!(price.cost_usd(&at_second_boundary), Some(0.004));

        let above_second_boundary = Usage {
            input_tokens: 201,
            ..usage
        };
        assert_eq!(price.cost_usd(&above_second_boundary), Some(0.00422));
    }

    #[test]
    fn cached_tokens_do_not_count_twice_for_tier_selection() {
        let price = price();
        let usage = Usage {
            input_tokens: 100,
            cached_input_tokens: 100,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        };
        assert_eq!(price.cost_usd(&usage), Some(0.0001));
    }

    #[test]
    fn summed_cache_counters_do_not_cross_a_request_tier() {
        let price = price();
        let usage = Usage {
            input_tokens: 200,
            cached_input_tokens: 100,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 100,
            cost_usd: None,
        };
        assert_eq!(price.cost_usd(&usage), Some(0.0014));
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
