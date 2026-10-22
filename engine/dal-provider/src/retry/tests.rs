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
fn resource_exhausted_quota_wording_is_terminal_quota() {
    let google = request(Family::GoogleGenerativeAi, "gemini-3-pro");
    for message in [
        "You have exhausted your capacity on this model. Your quota will reset after 3h15m38s.",
        "Your quota will reset after 1h.",
        "quota will reset at midnight",
    ] {
        let decision = classify(Some(429), Some("RESOURCE_EXHAUSTED"), message, &google);
        assert!(
            matches!(&decision, RetryDecision::Fail(ProviderError::Quota { message: m }) if m == message),
            "{message}: {decision:?}"
        );
    }
    // A numeric-only frame (HTTP status, no RPC spelling) with the wording is
    // the same terminal quota: the status leg covers it.
    let decision = classify(Some(429), None, "Your quota will reset after 1h.", &google);
    assert!(
        matches!(&decision, RetryDecision::Fail(ProviderError::Quota { .. })),
        "{decision:?}"
    );
    // No quota wording: a plain throttle stays retryable, and a details-only
    // exhaustion signal stays with the Google join, which owns the envelope.
    for message in ["Slow down", "Resource has been exhausted"] {
        assert!(
            matches!(
                classify(Some(429), Some("RESOURCE_EXHAUSTED"), message, &google),
                RetryDecision::Retry(ProviderError::RateLimited { .. })
            ),
            "{message}"
        );
    }
    // Other families never send this code; the wording rule still holds, so a
    // future caller cannot silently turn quota into a retry.
    assert!(google_quota_wording("quota will reset after 1h."));
    assert!(!google_quota_wording("Slow down"));
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
    let reserve = request(Family::Codex, LUNA_RESERVE_MODEL);
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
fn statuses_next_to_a_retry_rule_do_not_inherit_it() {
    for status in [407, 409, 428, 430, 499, 501, 505, 528, 530] {
        assert!(
            matches!(
                classify(Some(status), Some("other"), "no", &RESPONSES),
                RetryDecision::Fail(ProviderError::Status { status: s, family: Family::Responses, .. }) if s == status
            ),
            "{status} must end in Status without a retry"
        );
    }
}

#[test]
fn the_overload_code_is_special_only_on_a_503() {
    for status in [500, 502, 504] {
        let decision = classify(
            Some(status),
            Some("server_is_overloaded"),
            "busy",
            &RESPONSES,
        );
        assert!(
            matches!(
                &decision,
                RetryDecision::Retry(ProviderError::Status { status: s, .. }) if *s == status
            ),
            "the overload code is special only on 503: {status} gave {decision:?}"
        );
    }
}

#[test]
fn after_the_first_delivered_event_no_input_retries_and_the_error_is_unchanged() {
    let codes = [
        None,
        Some("overloaded_error"),
        Some("server_is_overloaded"),
        Some("rate_limit_error"),
        Some("insufficient_quota"),
        Some("context_length_exceeded"),
        Some("other"),
    ];
    let states = [
        (Family::Responses, false, false),
        (Family::Codex, true, false),
        (Family::Codex, true, true),
        (Family::Anthropic, false, false),
    ];
    let mut problems = Vec::new();
    for status in std::iter::once(None).chain((100..=599).map(Some)) {
        for code in codes {
            for (family, oauth, refreshed) in states {
                let fresh = RequestState {
                    oauth,
                    refreshed,
                    ..request(family, "gpt-6")
                };
                let delivered = RequestState {
                    delivered: true,
                    ..fresh
                };
                let before = classify(status, code, "m", &fresh);
                let after = classify(status, code, "m", &delivered);
                let context =
                    format!("{status:?} {code:?} {family:?} oauth={oauth} refreshed={refreshed}");
                match (&before, &after) {
                    (_, RetryDecision::Retry(_) | RetryDecision::RefreshOnce) => {
                        problems.push(format!("{context}: delivered still gave {after:?}"));
                    }
                    (
                        RetryDecision::Retry(kept) | RetryDecision::Fail(kept),
                        RetryDecision::Fail(error),
                    ) => {
                        if format!("{kept:?}") != format!("{error:?}") {
                            problems.push(format!("{context}: {kept:?} became {error:?}"));
                        }
                    }
                    (RetryDecision::RefreshOnce, RetryDecision::Fail(error)) => {
                        if !matches!(error, ProviderError::SignInExpired { provider } if provider == "p1")
                        {
                            problems.push(format!("{context}: refresh became {error:?}"));
                        }
                    }
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "{} problems: {:#?}",
        problems.len(),
        &problems[..problems.len().min(8)]
    );
}

#[test]
fn the_reserve_model_name_means_reserve_only_on_the_codex_family() {
    for family in [Family::Responses, Family::Chat, Family::Anthropic] {
        let other = request(family, LUNA_RESERVE_MODEL);
        for status in [400, 403, 404, 422] {
            let decision = classify(Some(status), None, "closed", &other);
            assert!(
                !matches!(
                    &decision,
                    RetryDecision::Fail(ProviderError::ReserveUnavailable { .. })
                ),
                "{family:?} {status}: {decision:?}"
            );
        }
    }
    let reserve = request(Family::Codex, LUNA_RESERVE_MODEL);
    for status in [429, 500, 503, 529] {
        assert!(
            matches!(
                classify(Some(status), None, "slow", &reserve),
                RetryDecision::Retry(_)
            ),
            "{status} on the reserve model still retries"
        );
    }
    assert!(matches!(
        classify(Some(401), None, "", &reserve),
        RetryDecision::Fail(ProviderError::AuthRejected { .. })
    ));
    assert!(matches!(
        classify(Some(409), None, "", &reserve),
        RetryDecision::Fail(ProviderError::Status { status: 409, .. })
    ));
}

#[test]
fn backoff_doubles_to_the_cap_and_never_past_it() {
    for (attempt, jitter, seconds) in [
        (0_u32, 1.0, 1.0),
        (1, 1.0, 1.0),
        (2, 1.0, 2.0),
        (6, 1.0, 32.0),
        (7, 0.9, 57.6),
        (7, 1.0, 60.0),
        (8, 0.9, 60.0),
        (9, 1.1, 60.0),
        (u32::MAX, 1.0, 60.0),
    ] {
        let waited = delay_for_attempt(attempt, None, jitter)
            .unwrap()
            .as_secs_f64();
        assert!(
            (waited - seconds).abs() < 1.0e-6,
            "attempt {attempt} at jitter {jitter} waited {waited} s, expected {seconds} s"
        );
    }
}

#[test]
fn a_jitter_draw_outside_the_allowed_range_is_clamped_not_trusted() {
    for (jitter, seconds) in [
        (f64::NAN, 1.0),
        (f64::INFINITY, 1.1),
        (f64::NEG_INFINITY, 0.9),
        (0.0, 0.9),
        (-5.0, 0.9),
        (1.5, 1.1),
        (1.0e300, 1.1),
    ] {
        let waited = delay_for_attempt(1, None, jitter).unwrap().as_secs_f64();
        assert!(
            (waited - seconds).abs() < 1.0e-6,
            "jitter {jitter} waited {waited} s, expected {seconds} s"
        );
    }
    assert_eq!(
        delay_for_attempt(4, Some(2.5), 0.9),
        Ok(Duration::from_millis(2500)),
        "a usable Retry-After ignores the jitter draw"
    );
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

#[test]
fn jitter_samples_map_to_the_full_allowed_range() {
    assert!((jitter_for_sample(0) - 0.9).abs() < 1.0e-9);
    assert!((jitter_for_sample(u32::MAX / 2) - 1.0).abs() < 1.0e-9);
    assert!((jitter_for_sample(u32::MAX) - 1.1).abs() < f64::EPSILON);
}

proptest::proptest! {
    #[test]
    fn jitter_samples_stay_within_range_in_sample_order(
        before in proptest::prelude::any::<u32>(),
        after in proptest::prelude::any::<u32>(),
    ) {
        let low = jitter_for_sample(before.min(after));
        let high = jitter_for_sample(before.max(after));
        proptest::prop_assert!((0.9..=1.1).contains(&low), "low {low}");
        proptest::prop_assert!((0.9..=1.1).contains(&high), "high {high}");
        proptest::prop_assert!(low <= high, "samples {before} and {after}");
    }
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
