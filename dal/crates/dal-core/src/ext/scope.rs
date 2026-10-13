use super::{Deserialize, Duration, Serialize};

/// The failure policy of one scope.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    /// The first failed handle cancels its siblings.
    #[default]
    Cancel,
    /// Every result is kept, failures included.
    Settle,
}

/// Optional bounds on the work one scope may start.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    /// The most handles the scope may start.
    pub requests: Option<u64>,
    /// The most input tokens the scope may spend.
    pub input_tokens: Option<u64>,
    /// The most output tokens the scope may spend.
    pub output_tokens: Option<u64>,
    /// The wall time from scope creation.
    pub wall: Option<Duration>,
    /// The most USD the scope may spend.
    pub usd: Option<f64>,
}

/// The fan-out, error policy, and budget of one scope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScopeSpec {
    /// The concurrent handle limit; handles beyond it wait in FIFO order.
    pub limit: u16,
    /// What one failed handle does to its siblings.
    pub on_error: OnError,
    /// The scope's bounds.
    pub budget: Budget,
}

/// Normalized usage rolled up per handle or scope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ScopeUsage {
    /// The number of provider requests.
    pub requests: u64,
    /// All input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// The summed USD cost; `None` once any request had an unknown cost.
    pub cost_usd: Option<f64>,
}

impl Default for ScopeUsage {
    fn default() -> Self {
        Self {
            requests: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: Some(0.0),
        }
    }
}

impl ScopeUsage {
    /// Adds `other` to this usage; one unknown cost makes the sum unknown.
    pub fn add(&mut self, other: &Self) {
        self.requests = self.requests.saturating_add(other.requests);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (Some(left), Some(right)) => Some(left + right),
            _ => None,
        };
    }
}

/// A scope specification or budget refusal.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScopeSpecError {
    /// `limit` is zero or above the global member cap.
    #[error("scope limit must be 1..={cap}, got {limit}")]
    InvalidLimit {
        /// The rejected limit.
        limit: u16,
        /// The global member cap that bounded it.
        cap: u16,
    },
    /// A budget bound is zero, negative, or not finite.
    #[error("scope budget `{field}` must be positive and finite")]
    InvalidBudget {
        /// The rejected budget field.
        field: &'static str,
    },
    /// A USD-limited scope reached a model with no known price.
    #[error("model `{model}` has no known price; a usd budget cannot admit it")]
    UnpricedModel {
        /// The unpriced model.
        model: Box<str>,
    },
    /// A budget limit was reached.
    #[error("scope budget exhausted")]
    Exhausted,
    /// The scope was cancelled.
    #[error("scope cancelled")]
    Cancelled,
}

impl ScopeSpec {
    /// Checks the limit against `global_member_cap` and every set bound.
    ///
    /// # Errors
    /// Returns [`ScopeSpecError::InvalidLimit`] when `limit` is outside
    /// `1..=global_member_cap`, and [`ScopeSpecError::InvalidBudget`] when a
    /// set integer bound is zero, `wall` is zero, or `usd` is not finite and
    /// strictly positive.
    pub fn validate(&self, global_member_cap: u16) -> Result<(), ScopeSpecError> {
        if self.limit == 0 || self.limit > global_member_cap {
            return Err(ScopeSpecError::InvalidLimit {
                limit: self.limit,
                cap: global_member_cap,
            });
        }
        let budget = &self.budget;
        let bad = |field| Err(ScopeSpecError::InvalidBudget { field });
        if budget.requests == Some(0) {
            return bad("requests");
        }
        if budget.input_tokens == Some(0) {
            return bad("input_tokens");
        }
        if budget.output_tokens == Some(0) {
            return bad("output_tokens");
        }
        if budget.wall == Some(Duration::ZERO) {
            return bad("wall");
        }
        if budget.usd.is_some_and(|usd| !usd.is_finite() || usd <= 0.0) {
            return bad("usd");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(limit: u16, budget: Budget) -> ScopeSpec {
        ScopeSpec {
            limit,
            on_error: OnError::default(),
            budget,
        }
    }

    #[test]
    fn limit_is_bounded_by_the_cap_on_both_sides() {
        assert!(spec(1, Budget::default()).validate(500).is_ok());
        assert!(spec(500, Budget::default()).validate(500).is_ok());
        assert_eq!(
            spec(0, Budget::default()).validate(500),
            Err(ScopeSpecError::InvalidLimit { limit: 0, cap: 500 })
        );
        assert_eq!(
            spec(501, Budget::default()).validate(500),
            Err(ScopeSpecError::InvalidLimit {
                limit: 501,
                cap: 500
            })
        );
    }

    #[test]
    fn every_set_bound_must_be_positive_and_finite() {
        let cases = [
            (
                "requests",
                Budget {
                    requests: Some(0),
                    ..Budget::default()
                },
            ),
            (
                "input_tokens",
                Budget {
                    input_tokens: Some(0),
                    ..Budget::default()
                },
            ),
            (
                "output_tokens",
                Budget {
                    output_tokens: Some(0),
                    ..Budget::default()
                },
            ),
            (
                "wall",
                Budget {
                    wall: Some(Duration::ZERO),
                    ..Budget::default()
                },
            ),
            (
                "usd",
                Budget {
                    usd: Some(0.0),
                    ..Budget::default()
                },
            ),
            (
                "usd",
                Budget {
                    usd: Some(-1.0),
                    ..Budget::default()
                },
            ),
            (
                "usd",
                Budget {
                    usd: Some(f64::NAN),
                    ..Budget::default()
                },
            ),
            (
                "usd",
                Budget {
                    usd: Some(f64::INFINITY),
                    ..Budget::default()
                },
            ),
        ];
        for (field, budget) in cases {
            assert_eq!(
                spec(1, budget).validate(500),
                Err(ScopeSpecError::InvalidBudget { field })
            );
        }
        let ok = Budget {
            requests: Some(1),
            usd: Some(0.01),
            ..Budget::default()
        };
        assert!(spec(1, ok).validate(500).is_ok());
    }

    #[test]
    fn priced_usage_rolls_up_from_zero() {
        let mut total = ScopeUsage::default();
        total.add(&ScopeUsage {
            requests: 1,
            input_tokens: 3,
            output_tokens: 4,
            cost_usd: Some(0.25),
        });
        total.add(&ScopeUsage {
            requests: 1,
            cost_usd: Some(0.5),
            ..ScopeUsage::default()
        });
        assert_eq!(total.cost_usd, Some(0.75));
        assert_eq!(
            (total.requests, total.input_tokens, total.output_tokens),
            (2, 3, 4)
        );
        total.add(&ScopeUsage {
            cost_usd: None,
            ..ScopeUsage::default()
        });
        assert_eq!(total.cost_usd, None);
    }
}
