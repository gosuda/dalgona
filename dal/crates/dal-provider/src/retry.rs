//! The retry table: which provider failures a request retries, and how long
//! it waits before the next attempt.
//!
//! Both functions are pure. The request lifecycle owns attempts, sleeps,
//! refresh, and cancellation; it asks [`classify`] what one failure means for
//! the request in its current [`RequestState`], and asks [`delay_for_attempt`]
//! how long the next backoff sleep lasts. A retryable decision carries the
//! error the request ends with when its retry budget is spent, so the
//! lifecycle never invents or loses a failure.

use std::time::Duration;

use dal_core::Family;

use crate::ProviderError;

/// The longest wait before one retry.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);

const MAX_RETRY_WAIT_SECS: f64 = 60.0;

/// The Codex model id of Luna Reserve.
const RESERVE_MODEL: &str = "gpt-reserve";

/// The HTTP status of the stream response that carries an in-stream error.
const IN_STREAM_STATUS: u16 = 200;

/// 429 codes that report an exhausted quota or credit balance.
const QUOTA_CODES: [&str; 5] = [
    "insufficient_quota",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "organization_usage_limit_exceeded",
];

/// What the lifecycle does with one failed attempt.
#[derive(Debug)]
pub enum RetryDecision {
    /// Back off and retry while attempts remain; when none remain, fail with
    /// the carried error.
    Retry(ProviderError),
    /// Run the serialized OAuth refresh once, then reconnect.
    RefreshOnce,
    /// Fail now with this error.
    Fail(ProviderError),
}

/// The facts about one request that change what a failure means.
#[derive(Clone, Copy, Debug)]
pub struct RequestState<'a> {
    /// The provider API family of the request.
    pub family: Family,
    /// The provider id the request named.
    pub provider: &'a str,
    /// The provider model id of the request.
    pub model: &'a str,
    /// The credential is an OAuth sign-in rather than an API key.
    pub oauth: bool,
    /// The one OAuth refresh of this request already ran.
    pub refreshed: bool,
    /// The first neutral event reached the consumer.
    pub delivered: bool,
}

/// A `Retry-After` value over the 60 s wait limit; the request fails at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryAfterTooLong {
    /// The requested wait, rounded up to whole seconds.
    pub seconds: u64,
}

impl RetryAfterTooLong {
    /// The request failure for this refusal, carrying the server message.
    #[must_use]
    pub fn into_error(self, message: String) -> ProviderError {
        ProviderError::RetryAfterTooLong {
            seconds: self.seconds,
            message,
        }
    }
}

/// The wait before retry `attempt` (1-based).
///
/// A `Retry-After` of at most 60 s is waited exactly; a past `Retry-After`
/// (zero or negative seconds) waits nothing. A `Retry-After` over 60 s is
/// never shortened: it is refused with the requested seconds rounded up, so
/// `60.2` reports 61 s. Without a usable `Retry-After` (absent or NaN), the
/// wait is `1 s * 2^(attempt - 1) * rng_jitter`, where `rng_jitter` is the
/// uniform draw from `[0.9, 1.1]`; out-of-range draws are clamped into that
/// interval, NaN counts as 1, attempt 0 counts as attempt 1, and the result
/// never exceeds 60 s.
///
/// # Errors
///
/// Returns [`RetryAfterTooLong`] when `retry_after_secs` is over 60 s,
/// including positive infinity.
pub fn delay_for_attempt(
    attempt: u32,
    retry_after_secs: Option<f64>,
    rng_jitter: f64,
) -> Result<Duration, RetryAfterTooLong> {
    if let Some(secs) = retry_after_secs.filter(|secs| !secs.is_nan()) {
        if secs > MAX_RETRY_WAIT_SECS {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "secs is over 60, and an unrepresentable wait saturates to u64::MAX"
            )]
            let seconds = secs.ceil() as u64;
            return Err(RetryAfterTooLong { seconds });
        }
        return Ok(if secs > 0.0 {
            Duration::from_secs_f64(secs)
        } else {
            Duration::ZERO
        });
    }
    let jitter = if rng_jitter.is_nan() {
        1.0
    } else {
        rng_jitter.clamp(0.9, 1.1)
    };
    // 2^7 s already exceeds the ceiling at the smallest jitter.
    let exponent = attempt.saturating_sub(1).min(7);
    let secs = f64::from(1_u32 << exponent) * jitter;
    Ok(Duration::from_secs_f64(secs).min(MAX_RETRY_WAIT))
}

/// What one failed attempt means for `request`.
///
/// `status` is the HTTP status, or `None` for a failure without one: a
/// transport error when `code` is `None`, else an in-stream error event
/// identified by `code`. An in-stream error with a code outside the table
/// never retries and ends in `Status` with the 200 of the stream response
/// that carried it.
/// `code` is the provider error code or type; it takes precedence over the
/// bare status meaning. `message` is the server message or transport
/// failure text, stored verbatim in the error.
///
/// Once `request.delivered` is set, no failure retries or refreshes: the
/// mapped error is final, and a 401 ends as the credential's terminal error.
#[must_use]
pub fn classify(
    status: Option<u16>,
    code: Option<&str>,
    message: &str,
    request: &RequestState<'_>,
) -> RetryDecision {
    match table(status, code, message, request) {
        RetryDecision::Retry(error) if request.delivered => RetryDecision::Fail(error),
        RetryDecision::RefreshOnce if request.delivered => {
            RetryDecision::Fail(ProviderError::SignInExpired {
                provider: String::from(request.provider),
            })
        }
        decision => decision,
    }
}

fn table(
    status: Option<u16>,
    code: Option<&str>,
    message: &str,
    request: &RequestState<'_>,
) -> RetryDecision {
    use RetryDecision::{Fail, RefreshOnce, Retry};

    let message = String::from(message);
    let family = request.family;
    let reserve = family == Family::Codex && request.model == RESERVE_MODEL;

    if let Some(code) = code {
        if let Some(overflow) = ProviderError::context_overflow(family, code, &message) {
            return Fail(overflow);
        }
        if QUOTA_CODES.contains(&code) {
            return Fail(ProviderError::Quota { message });
        }
        match code {
            "usage_limit_reached" => {
                return Fail(ProviderError::UsageLimit {
                    model: String::from(request.model),
                    message,
                });
            }
            "usage_not_included" => return Fail(ProviderError::UsageNotIncluded { message }),
            _ => {}
        }
    }

    let Some(status) = status else {
        return match code {
            Some(
                "overloaded_error"
                | "server_is_overloaded"
                | "server_busy"
                | "servers are currently busy",
            ) => Retry(ProviderError::Overloaded),
            Some("rate_limit_error" | "rate_limit_exceeded") => Retry(ProviderError::RateLimited {
                message,
                retry_after: None,
            }),
            Some(_) => Fail(ProviderError::Status {
                family,
                status: IN_STREAM_STATUS,
                message,
            }),
            None => Retry(ProviderError::Transport {
                family,
                reason: message,
            }),
        };
    };

    let status_error = |message| ProviderError::Status {
        family,
        status,
        message,
    };
    let overloaded = status == 529 || (status == 503 && code == Some("server_is_overloaded"));
    match status {
        _ if overloaded => Retry(ProviderError::Overloaded),
        408 | 500 | 502 | 503 | 504 => Retry(status_error(message)),
        429 => Retry(ProviderError::RateLimited {
            message,
            retry_after: None,
        }),
        402 if family == Family::Anthropic => Fail(ProviderError::Quota { message }),
        400 | 403 | 404 | 422 if reserve => {
            Fail(ProviderError::ReserveUnavailable { status, message })
        }
        400 | 422 => Fail(ProviderError::InvalidRequest { message }),
        401 if request.oauth && !request.refreshed => RefreshOnce,
        401 if request.oauth => Fail(ProviderError::SignInExpired {
            provider: String::from(request.provider),
        }),
        401 => Fail(ProviderError::AuthRejected {
            provider: String::from(request.provider),
        }),
        _ => Fail(status_error(message)),
    }
}

#[cfg(test)]
mod tests;
