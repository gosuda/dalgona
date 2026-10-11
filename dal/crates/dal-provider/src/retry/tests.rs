use super::*;

const fn request(family: Family, model: &'static str) -> RequestState<'static> {
    RequestState {
        family,
        provider: "p1",
        model,
        oauth: false,
        refreshed: false,
        delivered: false,
    }
}

const RESPONSES: RequestState<'static> = request(Family::Responses, "gpt-6");

#[test]
fn a_429_code_decides_before_the_retryable_status() {
    for code in QUOTA_CODES {
        let decision = classify(Some(429), Some(code), "out of credit", &RESPONSES);
        assert!(
            matches!(&decision, RetryDecision::Fail(ProviderError::Quota { message }) if message == "out of credit"),
            "{code}: {decision:?}"
        );
    }
    let codex = request(Family::Codex, "gpt-6-luna");
    assert!(matches!(
        classify(Some(429), Some("usage_limit_reached"), "m", &codex),
        RetryDecision::Fail(ProviderError::UsageLimit { model, .. }) if model == "gpt-6-luna"
    ));
    assert!(matches!(
        classify(Some(429), Some("usage_not_included"), "m", &codex),
        RetryDecision::Fail(ProviderError::UsageNotIncluded { .. })
    ));
    for code in [Some("rate_limit_error"), Some("rate_limit_exceeded"), None] {
        assert!(matches!(
            classify(Some(429), code, "slow down", &RESPONSES),
            RetryDecision::Retry(ProviderError::RateLimited { message, retry_after: None }) if message == "slow down"
        ));
    }
}

#[test]
fn overload_retries_and_ends_overloaded_but_plain_503_ends_in_status() {
    let anthropic = request(Family::Anthropic, "claude");
    assert!(matches!(
        classify(Some(503), Some("server_is_overloaded"), "busy", &RESPONSES),
        RetryDecision::Retry(ProviderError::Overloaded)
    ));
    assert!(matches!(
        classify(Some(529), Some("overloaded_error"), "busy", &anthropic),
        RetryDecision::Retry(ProviderError::Overloaded)
    ));
    assert!(matches!(
        classify(None, Some("overloaded_error"), "busy", &anthropic),
        RetryDecision::Retry(ProviderError::Overloaded)
    ));
    assert!(matches!(
        classify(None, Some("server_busy"), "busy", &anthropic),
        RetryDecision::Retry(ProviderError::Overloaded)
    ));
    assert!(matches!(
        classify(None, Some("servers are currently busy"), "busy", &anthropic),
        RetryDecision::Retry(ProviderError::Overloaded)
    ));
    for status in [503, 408, 500, 502, 504] {
        assert!(matches!(
            classify(Some(status), Some("other"), "busy", &RESPONSES),
            RetryDecision::Retry(ProviderError::Status { status: s, family: Family::Responses, .. }) if s == status
        ));
    }
}

#[test]
fn a_401_refreshes_an_oauth_sign_in_exactly_once() {
    let mut state = RequestState {
        oauth: true,
        ..RESPONSES
    };
    assert!(matches!(
        classify(Some(401), None, "", &state),
        RetryDecision::RefreshOnce
    ));
    state.refreshed = true;
    assert!(matches!(
        classify(Some(401), None, "", &state),
        RetryDecision::Fail(ProviderError::SignInExpired { provider }) if provider == "p1"
    ));
    assert!(matches!(
        classify(Some(401), None, "", &RESPONSES),
        RetryDecision::Fail(ProviderError::AuthRejected { provider }) if provider == "p1"
    ));
}

#[test]
fn no_failure_retries_after_the_first_delivered_event() {
    let delivered = RequestState {
        delivered: true,
        oauth: true,
        ..request(Family::Anthropic, "claude")
    };
    assert!(matches!(
        classify(None, Some("overloaded_error"), "busy", &delivered),
        RetryDecision::Fail(ProviderError::Overloaded)
    ));
    assert!(matches!(
        classify(None, None, "reset", &delivered),
        RetryDecision::Fail(ProviderError::Transport { reason, .. }) if reason == "reset"
    ));
    assert!(matches!(
        classify(Some(401), None, "", &delivered),
        RetryDecision::Fail(ProviderError::SignInExpired { .. })
    ));
}

#[test]
fn a_context_overflow_code_is_typed_and_never_retried() {
    assert!(matches!(
        classify(Some(400), Some("context_length_exceeded"), "too long", &RESPONSES),
        RetryDecision::Fail(ProviderError::ContextOverflow { family: Family::Responses, code, message })
            if code == "context_length_exceeded" && message == "too long"
    ));
    assert!(matches!(
        classify(
            Some(400),
            Some("invalid_request_error"),
            "context window exceeded",
            &RESPONSES
        ),
        RetryDecision::Fail(ProviderError::InvalidRequest { .. })
    ));
    let codex = request(Family::Codex, "gpt-6");
    assert!(matches!(
        classify(None, Some("context_window_exceeded"), "too long", &codex),
        RetryDecision::Fail(ProviderError::ContextOverflow { family: Family::Codex, code, .. })
            if code == "context_window_exceeded"
    ));
}

#[test]
fn terminal_statuses_never_retry() {
    let anthropic = request(Family::Anthropic, "claude");
    assert!(matches!(
        classify(Some(402), None, "billing", &anthropic),
        RetryDecision::Fail(ProviderError::Quota { .. })
    ));
    assert!(matches!(
        classify(None, Some("invalid_request_error"), "too long", &anthropic),
        RetryDecision::Fail(ProviderError::Status { status: 200, message, .. }) if message == "too long"
    ));
    assert!(matches!(
        classify(Some(402), None, "pay", &RESPONSES),
        RetryDecision::Fail(ProviderError::Status { status: 402, .. })
    ));
    for status in [400, 422] {
        assert!(matches!(
            classify(Some(status), None, "bad", &RESPONSES),
            RetryDecision::Fail(ProviderError::InvalidRequest { .. })
        ));
    }
    for status in [403, 404, 413] {
        assert!(matches!(
            classify(Some(status), None, "no", &RESPONSES),
            RetryDecision::Fail(ProviderError::Status { status: s, .. }) if s == status
        ));
    }
    let reserve = request(Family::Codex, "gpt-reserve");
    for status in [400, 403, 404, 422] {
        assert!(matches!(
            classify(Some(status), None, "closed", &reserve),
            RetryDecision::Fail(ProviderError::ReserveUnavailable { status: s, .. }) if s == status
        ));
    }
    assert!(matches!(
        classify(Some(413), None, "big", &reserve),
        RetryDecision::Fail(ProviderError::Status { status: 413, .. })
    ));
}

#[test]
fn retry_after_over_the_limit_is_refused_not_shortened() {
    assert_eq!(
        delay_for_attempt(1, Some(120.0), 1.0),
        Err(RetryAfterTooLong { seconds: 120 })
    );
    assert_eq!(
        delay_for_attempt(1, Some(60.2), 1.0),
        Err(RetryAfterTooLong { seconds: 61 })
    );
    assert_eq!(
        delay_for_attempt(1, Some(f64::INFINITY), 1.0),
        Err(RetryAfterTooLong { seconds: u64::MAX })
    );
    assert_eq!(delay_for_attempt(9, Some(60.0), 1.0), Ok(MAX_RETRY_WAIT));
    assert_eq!(
        delay_for_attempt(9, Some(2.0), 1.0),
        Ok(Duration::from_secs(2))
    );
    assert_eq!(delay_for_attempt(1, Some(-3.0), 1.0), Ok(Duration::ZERO));
    assert_eq!(
        delay_for_attempt(1, Some(f64::NEG_INFINITY), 1.0),
        Ok(Duration::ZERO)
    );
    assert_eq!(
        delay_for_attempt(3, Some(f64::NAN), 1.0),
        Ok(Duration::from_secs(4))
    );
}

proptest::proptest! {
    #[test]
    fn exponential_backoff_property(attempt in 1_u32..=5, jitter in 0.9_f64..=1.1) {
        let Ok(delay) = delay_for_attempt(attempt, None, jitter) else {
            proptest::prop_assert!(false, "no Retry-After never refuses");
            return Ok(());
        };
        let base = 1000.0 * f64::from(1_u32 << (attempt - 1));
        let millis = delay.as_secs_f64() * 1000.0;
        proptest::prop_assert!(millis >= base * 0.9 - 1e-6 && millis <= base * 1.1 + 1e-6);
    }

    #[test]
    fn backoff_stays_under_the_hard_ceiling(attempt: u32, jitter: f64) {
        let delay = delay_for_attempt(attempt, None, jitter);
        proptest::prop_assert!(matches!(delay, Ok(d) if d <= MAX_RETRY_WAIT && d >= Duration::from_millis(900)));
    }
}
