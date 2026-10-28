use std::{collections::BTreeMap, mem, path::PathBuf, time::Duration};

use super::*;
use crate::{
    run::RunLostReason,
    scripted::{Operation, ScriptError},
    usage::LUNA_RESERVE_MODEL,
};
use dal_core::InferFailure;

/// The longest prefix of the first line of `message` that is at most 300
/// bytes and ends on a character boundary, built one character at a time.
fn first_bounded_line(message: &str) -> String {
    let line = message.split(['\r', '\n']).next().unwrap_or_default();
    let mut kept = String::new();
    for ch in line.chars() {
        if kept.len() + ch.len_utf8() > 300 {
            break;
        }
        kept.push(ch);
    }
    kept
}

#[test]
fn status_message_first_line_and_utf8_cap() {
    let message = format!("{}\u{e9}{}\nsecond line", "a".repeat(299), "b".repeat(10));
    let error = ProviderError::Status {
        family: Family::Chat,
        status: 429,
        message: message.clone(),
    };
    let expected = format!("openai error 429: {}", "a".repeat(299));
    assert_eq!(error.to_string(), expected);
    assert_eq!(error.to_string(), expected);
    match &error {
        ProviderError::Status {
            message: stored, ..
        } => assert_eq!(stored, &message),
        other => panic!("unexpected variant {other:?}"),
    }
}

#[test]
fn status_message_at_the_byte_cap_stays_whole() {
    let error = ProviderError::Status {
        family: Family::Chat,
        status: 429,
        message: format!("{}zzz\nmore", "a".repeat(300)),
    };
    assert_eq!(
        error.to_string(),
        format!("openai error 429: {}", "a".repeat(300))
    );
}

#[test]
fn empty_messages_stay_empty() {
    let status = ProviderError::Status {
        family: Family::Responses,
        status: 401,
        message: String::new(),
    };
    assert_eq!(status.to_string(), "openai error 401: ");
    let reason = UsageCheckReason::Status {
        status: 401,
        message: String::new(),
    };
    assert_eq!(reason.to_string(), "401 ");
}

#[test]
fn ws_closed_renders_only_the_close_code_it_carries() {
    let bare = ProviderError::WsClosed { code: None };
    assert_eq!(
        bare.to_string(),
        "websocket closed by server before response.completed."
    );
    let coded = ProviderError::WsClosed {
        code: Some((1011, String::from("busy"))),
    };
    assert_eq!(
        coded.to_string(),
        "websocket closed by server before response.completed. (code 1011: busy)"
    );
}

#[test]
fn usage_limit_head_names_luna_reserve_for_gpt_reserve() {
    let reserve = ProviderError::UsageLimit {
        model: String::from(LUNA_RESERVE_MODEL),
        message: String::from("try tomorrow"),
    };
    assert_eq!(
        reserve.to_string(),
        "Luna Reserve usage limit reached: try tomorrow"
    );
    let other = ProviderError::UsageLimit {
        model: String::from("gpt-6-luna"),
        message: String::from("try tomorrow"),
    };
    assert_eq!(other.to_string(), "usage limit reached: try tomorrow");
    for near_miss in [
        format!("{LUNA_RESERVE_MODEL}-2"),
        format!("x{LUNA_RESERVE_MODEL}"),
        format!("{LUNA_RESERVE_MODEL} "),
        LUNA_RESERVE_MODEL.to_uppercase(),
        String::new(),
    ] {
        let error = ProviderError::UsageLimit {
            model: near_miss.clone(),
            message: String::from("try tomorrow"),
        };
        assert_eq!(
            error.to_string(),
            "usage limit reached: try tomorrow",
            "model {near_miss:?} is not the Luna Reserve model"
        );
    }
}

#[test]
fn family_labels_distinguish_openai_codex_and_anthropic() {
    let cases = [
        (Family::Chat, "openai sent an invalid stream: x"),
        (Family::Responses, "openai sent an invalid stream: x"),
        (Family::Codex, "codex sent an invalid stream: x"),
        (Family::Anthropic, "anthropic sent an invalid stream: x"),
    ];
    for (family, expected) in cases {
        let error = ProviderError::Protocol {
            family,
            detail: String::from("x"),
        };
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn fix_sentences_keep_first_candidates_and_path_values() {
    let ambiguous = ProviderError::AmbiguousModel {
        id: String::from("gpt-5.6-luna"),
        candidates: String::from("p1/gpt-5.6-luna, p2/gpt-5.6-luna"),
    };
    assert_eq!(ambiguous.fix().as_deref(), Some("Use p1/gpt-5.6-luna."));
    let path = PathBuf::from("/data/dir with space/auth.json");
    let perms = ProviderError::AuthFilePerms { path: path.clone() };
    assert_eq!(
        perms.fix().as_deref(),
        Some("Run chmod 600 /data/dir with space/auth.json.")
    );
    match &perms {
        ProviderError::AuthFilePerms { path: stored } => assert_eq!(stored, &path),
        other => panic!("unexpected variant {other:?}"),
    }
}

#[test]
fn retryable_by_loop_names_only_the_four_replayable_failures() {
    let retryable = [
        ProviderError::StreamCut,
        ProviderError::Overloaded,
        ProviderError::WsClosed { code: None },
        ProviderError::Transport {
            family: Family::Chat,
            reason: String::from("reset"),
        },
    ];
    for error in retryable {
        assert!(error.retryable_by_loop(), "{error}");
    }
    let terminal = [
        ProviderError::Status {
            family: Family::Chat,
            status: 500,
            message: String::new(),
        },
        ProviderError::InvalidRequest {
            message: String::from("bad"),
        },
        ProviderError::InvalidThinkingLevel {
            spelling: String::from("ultra"),
        },
        ProviderError::RateLimited {
            message: String::from("busy"),
            retry_after: None,
        },
        ProviderError::Quota {
            message: String::from("empty"),
        },
        ProviderError::Limit(LimitError::SseLine),
        ProviderError::AuthRejected {
            provider: String::from("openai"),
        },
        ProviderError::LoginCancelled,
        ProviderError::Cancelled,
    ];
    for error in terminal {
        assert!(!error.retryable_by_loop(), "{error}");
    }
}

#[test]
fn a_lost_run_is_fatal_and_never_replayable_whatever_its_reason() {
    let reasons = [
        RunLostReason::ServerClosed,
        RunLostReason::Transport(String::from("reset")),
        RunLostReason::ResultsMismatch,
        RunLostReason::Protocol(String::from("bad frame")),
        RunLostReason::BufferExceeded,
        RunLostReason::Authority("web_fetch".into()),
    ];
    for reason in reasons {
        let error = ProviderError::RunLost {
            family: Family::Chat,
            reason,
        };
        assert!(!error.retryable_by_loop(), "{error}");
        let text = error.to_string();
        let failure = InferFailure::from(error);
        assert!(
            matches!(&failure, InferFailure::Fatal { message, fix: None } if **message == *text),
            "{failure:?}"
        );
    }
}

#[test]
fn run_loss_text_keeps_only_the_first_bounded_line_of_server_text() {
    let long = format!("{}\nsecond line", "w".repeat(400));
    let reasons = [
        RunLostReason::Transport(long.clone()),
        RunLostReason::Protocol(long.clone()),
        RunLostReason::Authority(long.into()),
    ];
    for reason in reasons {
        let text = ProviderError::RunLost {
            family: Family::Chat,
            reason,
        }
        .to_string();
        assert!(!text.contains("second line"), "{text}");
        assert!(text.ends_with(&"w".repeat(300)), "{text}");
        assert!(!text.ends_with(&"w".repeat(301)), "{text}");
    }
}

#[test]
fn context_overflow_is_typed_by_code_never_by_message() {
    for (family, code) in [
        (Family::Chat, "context_length_exceeded"),
        (Family::Codex, "context_window_exceeded"),
    ] {
        let error = ProviderError::context_overflow(family, code, "too long");
        assert!(
            matches!(&error, Some(ProviderError::ContextOverflow { family: f, code: c, message })
                    if *f == family && c == code && message == "too long"),
            "{error:?}"
        );
    }
    for code in [
        "invalid_request_error",
        "Context_Length_Exceeded",
        "",
        " context_length_exceeded",
        "context_length_exceeded ",
        "context_length_exceeded\n",
        "context_length_exceeded_extra",
        "context_window",
        "CONTEXT_WINDOW_EXCEEDED",
    ] {
        assert!(
            ProviderError::context_overflow(
                Family::Anthropic,
                code,
                "prompt is too long: context window exceeded"
            )
            .is_none(),
            "{code}"
        );
    }
}

#[test]
fn infer_failure_separates_overflow_from_an_ordinary_bad_request() {
    let overflow = ProviderError::context_overflow(
        Family::Chat,
        "context_length_exceeded",
        "maximum context length is 8192 tokens",
    )
    .map(InferFailure::from);
    assert!(matches!(
        &overflow,
        Some(InferFailure::Overflow { code, message })
            if &**code == "context_length_exceeded"
                && &**message == "openai context window exceeded (context_length_exceeded): maximum context length is 8192 tokens"
    ));
    let ordinary = InferFailure::from(ProviderError::InvalidRequest {
        message: String::from("maximum context length is 8192 tokens"),
    });
    assert!(matches!(
        &ordinary,
        InferFailure::Fatal { message, fix: None }
            if &**message == "invalid request: maximum context length is 8192 tokens"
    ));
    assert_eq!(
        ordinary.to_string(),
        "invalid request: maximum context length is 8192 tokens"
    );
}
#[test]
fn http_413_is_classified_as_context_overflow() {
    let failure = InferFailure::from(ProviderError::Status {
        family: Family::Responses,
        status: 413,
        message: String::from("request body is too large"),
    });
    assert!(matches!(
        failure,
        InferFailure::Overflow { code, message }
            if &*code == "request_too_large"
                && &*message == "openai error 413: request body is too large"
    ));
}

#[test]
fn infer_failure_retries_only_transient_provider_failures() {
    let transient = [
        ProviderError::RateLimited {
            message: String::from("slow down"),
            retry_after: None,
        },
        ProviderError::Overloaded,
        ProviderError::StreamCut,
        ProviderError::WsClosed {
            code: Some((1011, String::from("busy"))),
        },
        ProviderError::Transport {
            family: Family::Anthropic,
            reason: String::from("connection reset"),
        },
    ];
    for error in transient {
        let text = error.to_string();
        let failure = InferFailure::from(error);
        assert!(
            matches!(&failure, InferFailure::Retryable { hint: None, message } if **message == *text),
            "{failure:?}"
        );
        assert_eq!(failure.to_string(), text);
    }
}

#[test]
fn infer_failure_hints_only_the_rate_limit_wait() {
    for wait in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_millis(2_500),
        Duration::from_secs(60),
        Duration::MAX,
    ] {
        let limited = ProviderError::RateLimited {
            message: String::from("slow down"),
            retry_after: Some(wait),
        };
        let failure = InferFailure::from(limited);
        assert!(
            matches!(
                &failure,
                InferFailure::Retryable { hint: Some(hint), message }
                    if *hint == wait && &**message == "rate limited: slow down"
            ),
            "{wait:?}: {failure:?}"
        );
    }
}

#[test]
fn infer_failure_ends_auth_quota_and_exhausted_status_with_their_fix() {
    let cases = [
        (
            ProviderError::AuthRejected {
                provider: String::from("p1"),
            },
            "p1 rejected the API key.",
            Some("Run dalgon login p1."),
        ),
        (
            ProviderError::SignInExpired {
                provider: String::from("openai-codex"),
            },
            "openai-codex sign-in expired: the refresh token was rejected.",
            Some("Run dalgon login openai-codex."),
        ),
        (
            ProviderError::Quota {
                message: String::from("out of credit"),
            },
            "quota exhausted: out of credit",
            None,
        ),
        (
            ProviderError::Status {
                family: Family::Responses,
                status: 503,
                message: String::from("unavailable"),
            },
            "openai error 503: unavailable",
            None,
        ),
        (
            ProviderError::RetryAfterTooLong {
                seconds: u64::MAX,
                message: String::from("later"),
            },
            "rate limited for 18446744073709551615 s, which is over the 60 s wait limit: later",
            None,
        ),
    ];
    for (error, text, expected_fix) in cases {
        let failure = InferFailure::from(error);
        assert!(
            matches!(&failure, InferFailure::Fatal { message, fix }
                    if &**message == text && fix.as_deref() == expected_fix),
            "{failure:?}"
        );
    }
}

#[test]
fn unresolved_blob_is_a_local_terminal_failure_naming_the_blob() {
    let blob_id = dal_core::BlobId::from_bytes(b"image bytes");
    let error = ProviderError::UnresolvedBlob { blob_id };
    let text = format!(
        "blob {blob_id} was not read from the session store before the request; no request was sent."
    );
    assert_eq!(error.to_string(), text);
    assert_eq!(error.fix(), None);
    assert!(!error.retryable_by_loop());
    assert!(matches!(
        InferFailure::from(error),
        InferFailure::Fatal { message, fix: None } if *message == *text
    ));
}

#[test]
fn usage_check_reason_renderings() {
    assert_eq!(
        UsageCheckReason::Timeout.to_string(),
        "no reply within 15 s"
    );
    assert_eq!(
        UsageCheckReason::NotJsonObject.to_string(),
        "the body is not a JSON object"
    );
    let long = UsageCheckReason::Status {
        status: 401,
        message: format!("{}\u{e9}tail\nnext", "m".repeat(299)),
    };
    assert_eq!(long.to_string(), format!("401 {}", "m".repeat(299)));
    let transport = UsageCheckReason::Transport {
        reason: String::from("connection refused\nsecond line"),
    };
    assert_eq!(transport.to_string(), "connection refused");
}

#[test]
fn resolve_error_texts_and_fixes() {
    let unknown = ResolveError::UnknownModel {
        reference: String::from("openai/gpt-reserve"),
    };
    assert_eq!(unknown.to_string(), "unknown model openai/gpt-reserve");
    assert_eq!(
        unknown.fix().as_deref(),
        Some("Run dalgon models to list the models.")
    );
    let ambiguous = ResolveError::AmbiguousModel {
        id: String::from("gpt-5.6-luna"),
        candidates: String::from("p1/id, p2/id"),
    };
    assert_eq!(
        ambiguous.to_string(),
        "model id gpt-5.6-luna matches several providers: p1/id, p2/id"
    );
    assert_eq!(ambiguous.fix().as_deref(), Some("Use p1/id."));
    let no_default = ResolveError::NoDefault;
    assert_eq!(no_default.to_string(), "no default model is configured");
    assert_eq!(no_default.fix(), None);
    let unknown_provider = ResolveError::UnknownProvider {
        provider: String::from("acme-proxy"),
        model: String::from("gpt-5"),
    };
    assert_eq!(
        unknown_provider.to_string(),
        "provider acme-proxy is not configured for model gpt-5"
    );
    let family = ResolveError::ProviderFamilyMismatch {
        provider: String::from("openai"),
        model: String::from("claude-3"),
    };
    assert_eq!(
        family.to_string(),
        "provider openai does not serve the API family of model claude-3"
    );
    assert_eq!(
        unknown_provider.fix().as_deref(),
        Some("Configure that provider or pick another model with /model.")
    );
    assert_eq!(unknown_provider.fix(), family.fix());
}

#[test]
fn unbound_and_fixed_wire_models_keep_their_resolution_fixes() {
    let unknown_provider = ResolveError::UnknownProvider {
        provider: String::from("acme-proxy"),
        model: String::from("gpt-5"),
    };
    let unbound = ResolveError::UnboundModel {
        provider: String::from("acme-proxy"),
        model: String::from("gpt-5"),
    };
    assert_eq!(unbound.fix(), unknown_provider.fix());
    assert!(unbound.fix().is_some());
    let fixed = ResolveError::FixedWireBase {
        provider: String::from("bai"),
        model: String::from("claude-opus-5"),
    };
    assert!(fixed.fix().is_some());
    assert_ne!(fixed.fix(), unknown_provider.fix());
}

#[test]
fn only_http_413_folds_to_a_request_size_overflow_in_every_family() {
    let statuses = (0..=700).chain([u16::MAX - 1, u16::MAX]);
    for family in Family::ALL {
        for status in statuses.clone() {
            let error = ProviderError::Status {
                family,
                status,
                message: String::from("body"),
            };
            let text = error.to_string();
            assert!(!error.retryable_by_loop(), "{family:?} {status}");
            let failure = InferFailure::from(error);
            if status == 413 {
                assert!(
                    matches!(&failure, InferFailure::Overflow { code, message }
                        if &**code == "request_too_large" && **message == *text),
                    "{family:?}: {failure:?}"
                );
            } else {
                assert!(
                    matches!(&failure, InferFailure::Fatal { message, fix: None }
                        if **message == *text),
                    "{family:?} {status}: {failure:?}"
                );
            }
        }
    }
}

fn hostile_messages() -> Vec<String> {
    let a = |count: usize| "a".repeat(count);
    vec![
        String::new(),
        String::from("head\rsecret"),
        String::from("head\r\nsecret"),
        String::from("head\nsecret"),
        String::from("\nleading newline"),
        String::from("\rleading return"),
        format!("{}\nsecond", a(400)),
        format!("{}\rsecond", a(300)),
        a(299),
        a(300),
        a(301),
        format!("{}\u{e9}tail", a(299)),
        format!("{}\u{20ac}tail", a(298)),
        format!("{}\u{20ac}tail", a(297)),
        format!("{}\u{1f600}tail", a(297)),
        format!("{}\u{1f600}tail", a(296)),
    ]
}

#[test]
fn every_capped_renderer_cuts_hostile_messages_to_one_bounded_line() {
    for message in hostile_messages() {
        let kept = first_bounded_line(&message);
        let rendered = [
            (
                ProviderError::Status {
                    family: Family::Chat,
                    status: 502,
                    message: message.clone(),
                }
                .to_string(),
                format!("openai error 502: {kept}"),
            ),
            (
                ProviderError::ContextOverflow {
                    family: Family::Codex,
                    code: String::from("context_window_exceeded"),
                    message: message.clone(),
                }
                .to_string(),
                format!("codex context window exceeded (context_window_exceeded): {kept}"),
            ),
            (
                UsageCheckReason::Status {
                    status: 500,
                    message: message.clone(),
                }
                .to_string(),
                format!("500 {kept}"),
            ),
            (
                UsageCheckReason::Transport {
                    reason: message.clone(),
                }
                .to_string(),
                kept.clone(),
            ),
            (
                ProviderError::RunLost {
                    family: Family::Chat,
                    reason: RunLostReason::Transport(message.clone()),
                }
                .to_string(),
                format!("openai run lost: transport failure: {kept}"),
            ),
            (
                ProviderError::RunLost {
                    family: Family::Chat,
                    reason: RunLostReason::Protocol(message.clone()),
                }
                .to_string(),
                format!("openai run lost: invalid stream: {kept}"),
            ),
            (
                ProviderError::RunLost {
                    family: Family::Chat,
                    reason: RunLostReason::Authority(message.as_str().into()),
                }
                .to_string(),
                format!("openai run lost: the server acted beyond the host's authority: {kept}"),
            ),
        ];
        for (text, expected) in rendered {
            assert_eq!(text, expected, "message {message:?}");
            assert!(!text.contains(['\r', '\n']), "{text:?}");
        }
    }
}

#[test]
fn every_family_label_is_distinct_and_matches_its_wire_name() {
    let mut seen = BTreeMap::new();
    for family in Family::ALL {
        let label = family_label(family);
        assert!(
            !label.is_empty() && label.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "{family:?}: {label:?}"
        );
        if let Some(other) = seen.insert(label, family) {
            assert!(
                matches!((other, family), (Family::Chat, Family::Responses)),
                "{other:?} and {family:?} share the label {label}"
            );
        }
        let wire = sonic_rs::to_string(&family).expect("a family serializes");
        let wire = wire.trim_matches('"');
        let expected = match family {
            Family::Chat | Family::Responses => "openai",
            Family::Codex => "codex",
            _ => wire,
        };
        assert_eq!(label, expected, "{family:?} has wire name {wire}");
    }
    assert_eq!(seen.len(), Family::ALL.len() - 1);
}

#[test]
fn a_synthetic_failure_passes_through_the_fold_exactly() {
    let failures = [
        InferFailure::Cancelled,
        InferFailure::Overflow {
            code: "context_length_exceeded".into(),
            message: "too long".into(),
        },
        InferFailure::Retryable {
            hint: Some(Duration::from_secs(5)),
            message: "later".into(),
        },
        InferFailure::Retryable {
            hint: None,
            message: "again".into(),
        },
        InferFailure::Fatal {
            message: "no".into(),
            fix: Some("Run dalgon login p.".into()),
        },
        InferFailure::Fatal {
            message: "no".into(),
            fix: None,
        },
    ];
    for failure in failures {
        let error = ProviderError::Synthetic(failure.clone());
        assert_eq!(error.to_string(), failure.to_string());
        assert!(!error.retryable_by_loop(), "{failure:?}");
        assert_eq!(InferFailure::from(error), failure);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one constructor per public error variant keeps the sweep complete"
)]
fn every_variant() -> Vec<ProviderError> {
    let family = Family::Responses;
    let detail = || String::from("detail");
    let path = || PathBuf::from("/data/auth.json");
    vec![
        ProviderError::Transport {
            family,
            reason: detail(),
        },
        ProviderError::Status {
            family,
            status: 500,
            message: detail(),
        },
        ProviderError::InvalidRequest { message: detail() },
        ProviderError::ContextOverflow {
            family,
            code: String::from("context_length_exceeded"),
            message: detail(),
        },
        ProviderError::InvalidThinkingLevel { spelling: detail() },
        ProviderError::RateLimited {
            message: detail(),
            retry_after: None,
        },
        ProviderError::RetryAfterTooLong {
            seconds: 61,
            message: detail(),
        },
        ProviderError::Quota { message: detail() },
        ProviderError::Overloaded,
        ProviderError::AuthRejected { provider: detail() },
        ProviderError::SignInExpired { provider: detail() },
        ProviderError::NoCredentials { provider: detail() },
        ProviderError::StreamCut,
        ProviderError::WsClosed { code: None },
        ProviderError::Protocol {
            family,
            detail: detail(),
        },
        ProviderError::RunLost {
            family,
            reason: RunLostReason::ServerClosed,
        },
        ProviderError::Limit(LimitError::Body),
        ProviderError::PlainHttp { host: detail() },
        ProviderError::UnresolvedBlob {
            blob_id: dal_core::BlobId::from_bytes(b"blob"),
        },
        ProviderError::ToolNameCollision {
            wire: detail(),
            first: detail(),
            second: detail(),
        },
        ProviderError::MissingEnvVariable {
            provider: detail().into_boxed_str(),
            variable: "CLOUDFLARE_ACCOUNT_ID",
        },
        ProviderError::MalformedEnvVariable {
            provider: detail().into_boxed_str(),
            variable: "CLOUDFLARE_GATEWAY_ID",
            expected: "one path segment",
        },
        ProviderError::CompactionMissing {
            family,
            noun: "item",
        },
        ProviderError::CompactionForeign {
            bound_family: Family::Anthropic,
            bound_model: detail(),
            family,
            model: detail(),
        },
        ProviderError::UsageLimit {
            model: detail(),
            message: detail(),
        },
        ProviderError::UsageNotIncluded { message: detail() },
        ProviderError::ReserveUnavailable {
            status: 404,
            message: detail(),
        },
        ProviderError::UnknownModel {
            reference: detail(),
        },
        ProviderError::AmbiguousModel {
            id: detail(),
            candidates: String::from("a/m, b/m"),
        },
        ProviderError::AuthFilePerms { path: path() },
        ProviderError::AuthFileSymlink { path: path() },
        ProviderError::AuthFileInvalid {
            path: path(),
            message: detail(),
        },
        ProviderError::AuthWrite { reason: detail() },
        ProviderError::CallbackBind {
            port: 7437,
            reason: detail(),
        },
        ProviderError::StateMismatch,
        ProviderError::LoginTimeout,
        ProviderError::TokenExchange {
            status: 400,
            message: detail(),
        },
        ProviderError::DeviceCode {
            status: 500,
            message: detail(),
        },
        ProviderError::NoAccountId,
        ProviderError::LoginCancelled,
        ProviderError::Cancelled,
        ProviderError::LoginInput { reason: detail() },
        ProviderError::UsageCheck {
            reason: UsageCheckReason::Timeout,
        },
        ProviderError::Script(ScriptError::Exhausted {
            operation: Operation::Open,
        }),
        ProviderError::Synthetic(InferFailure::Cancelled),
    ]
}

#[test]
fn the_fold_retries_exactly_five_variants_and_keeps_every_fix() {
    let variants = every_variant();
    assert_eq!(
        variants
            .iter()
            .map(mem::discriminant)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        45,
        "every variant appears exactly once"
    );
    for error in variants {
        let text = error.to_string();
        assert!(!text.is_empty(), "{error:?}");
        let fix = error.fix();
        let by_loop = error.retryable_by_loop();
        let transient = matches!(
            &error,
            ProviderError::RateLimited { .. }
                | ProviderError::Overloaded
                | ProviderError::StreamCut
                | ProviderError::WsClosed { .. }
                | ProviderError::Transport { .. }
        );
        let overflow = matches!(&error, ProviderError::ContextOverflow { .. });
        let synthetic = matches!(&error, ProviderError::Synthetic(_));
        let debug = format!("{error:?}");
        let failure = InferFailure::from(error);
        match &failure {
            InferFailure::Overflow { message, .. } => {
                assert!(overflow, "{debug} folded to Overflow");
                assert_eq!(**message, *text, "{debug}");
            }
            InferFailure::Retryable { message, .. } => {
                assert!(transient, "{debug} folded to Retryable");
                assert_eq!(**message, *text, "{debug}");
            }
            InferFailure::Fatal {
                message,
                fix: folded,
            } => {
                assert!(
                    !overflow && !transient,
                    "{debug} folded to Fatal, but it is transient or an overflow"
                );
                if synthetic {
                    continue;
                }
                assert_eq!(**message, *text, "{debug}");
                assert_eq!(folded.as_deref(), fix.as_deref(), "{debug}");
            }
            other => assert!(synthetic, "{debug} folded to {other:?}"),
        }
        if overflow {
            assert!(matches!(failure, InferFailure::Overflow { .. }), "{debug}");
        }
        if transient {
            assert!(
                matches!(failure, InferFailure::Retryable { .. }),
                "{debug} must fold to Retryable"
            );
        }
        if by_loop {
            assert!(transient, "{debug} is replayable but not transient");
        }
    }
}
