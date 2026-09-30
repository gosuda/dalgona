use std::{path::PathBuf, time::Duration};

use super::*;
use dal_core::InferFailure;

#[expect(
    clippy::too_many_lines,
    reason = "one row per public error variant pins its exact display text and fix"
)]
#[test]
fn error_display_texts() {
    let cases: &[(ProviderError, &str, Option<&str>)] = &[
        (
            ProviderError::Transport {
                family: Family::Chat,
                reason: String::from("connection refused"),
            },
            "openai request failed: connection refused",
            None,
        ),
        (
            ProviderError::Status {
                family: Family::Responses,
                status: 429,
                message: String::from("slow down"),
            },
            "openai error 429: slow down",
            None,
        ),
        (
            ProviderError::InvalidRequest {
                message: String::from("bad tool"),
            },
            "invalid request: bad tool",
            None,
        ),
        (
            ProviderError::InvalidThinkingLevel {
                spelling: String::from("Ultra"),
            },
            "unknown thinking level \"Ultra\"; use one of off, minimal, low, medium, high, xhigh, max.",
            None,
        ),
        (
            ProviderError::RateLimited {
                message: String::from("try later"),
                retry_after: Some(Duration::from_secs(3)),
            },
            "rate limited: try later",
            None,
        ),
        (
            ProviderError::RetryAfterTooLong {
                seconds: 120,
                message: String::from("busy"),
            },
            "rate limited for 120 s, which is over the 60 s wait limit: busy",
            None,
        ),
        (
            ProviderError::Quota {
                message: String::from("no credit"),
            },
            "quota exhausted: no credit",
            None,
        ),
        (ProviderError::Overloaded, "server overloaded.", None),
        (
            ProviderError::AuthRejected {
                provider: String::from("anthropic"),
            },
            "anthropic rejected the API key.",
            Some("Run dalgon login anthropic."),
        ),
        (
            ProviderError::SignInExpired {
                provider: String::from("openai-codex"),
            },
            "openai-codex sign-in expired: the refresh token was rejected.",
            Some("Run dalgon login openai-codex."),
        ),
        (
            ProviderError::NoCredentials {
                provider: String::from("openai"),
            },
            "openai has no credentials: auth.json has no entry for it",
            Some("Run dalgon login openai."),
        ),
        (
            ProviderError::StreamCut,
            "stream cut off before completion.",
            None,
        ),
        (
            ProviderError::WsClosed { code: None },
            "websocket closed by server before response.completed.",
            None,
        ),
        (
            ProviderError::Protocol {
                family: Family::Codex,
                detail: String::from("binary WebSocket frame"),
            },
            "codex sent an invalid stream: binary WebSocket frame",
            None,
        ),
        (
            ProviderError::Limit(LimitError::SseLine),
            "SSE line exceeds 1 MiB.",
            None,
        ),
        (
            ProviderError::Limit(LimitError::SseEvent),
            "SSE event exceeds 8 MiB.",
            None,
        ),
        (
            ProviderError::Limit(LimitError::WsMessage),
            "WebSocket message exceeds 16 MiB.",
            None,
        ),
        (
            ProviderError::Limit(LimitError::Body),
            "response body exceeds 16 MiB.",
            None,
        ),
        (
            ProviderError::PlainHttp {
                host: String::from("example.com"),
            },
            "refusing plain http for non-loopback host example.com.",
            None,
        ),
        (
            ProviderError::CompactionMissing {
                family: Family::Codex,
                noun: "item",
            },
            "codex compaction returned no compaction item.",
            None,
        ),
        (
            ProviderError::CompactionMissing {
                family: Family::Anthropic,
                noun: "block",
            },
            "anthropic compaction returned no compaction block.",
            None,
        ),
        (
            ProviderError::CompactionForeign {
                bound_family: Family::Anthropic,
                bound_model: String::from("claude-sonnet-5"),
                family: Family::Responses,
                model: String::from("gpt-5.6-luna"),
            },
            "the compacted history belongs to anthropic/claude-sonnet-5; this request uses openai/gpt-5.6-luna.",
            None,
        ),
        (
            ProviderError::UsageLimit {
                model: String::from("gpt-reserve"),
                message: String::from("try tomorrow"),
            },
            "Luna Reserve usage limit reached: try tomorrow",
            Some(
                "Wait for the limit to reset, or add credits at https://chatgpt.com/codex/settings/usage?credits_modal=true.",
            ),
        ),
        (
            ProviderError::UsageNotIncluded {
                message: String::from("plan"),
            },
            "this ChatGPT plan does not include Codex usage: plan",
            Some(
                "Upgrade the ChatGPT plan at https://chatgpt.com/explore/plus, or run dalgon login openai-codex with another account.",
            ),
        ),
        (
            ProviderError::ReserveUnavailable {
                status: 404,
                message: String::from("no such model"),
            },
            "Luna Reserve is not available for this account: no such model",
            Some(
                "Luna Reserve opens only when the included usage of your ChatGPT plan runs out. Switch to another model to continue.",
            ),
        ),
        (
            ProviderError::UnknownModel {
                reference: String::from("openai/gpt-reserve"),
            },
            "unknown model openai/gpt-reserve",
            Some("Run dalgon models to list the models."),
        ),
        (
            ProviderError::AmbiguousModel {
                id: String::from("gpt-5.6-luna"),
                candidates: String::from("p1/gpt-5.6-luna, p2/gpt-5.6-luna"),
            },
            "model id gpt-5.6-luna matches several providers: p1/gpt-5.6-luna, p2/gpt-5.6-luna",
            Some("Use p1/gpt-5.6-luna."),
        ),
        (
            ProviderError::AuthFilePerms {
                path: PathBuf::from("/data/auth.json"),
            },
            "auth.json has group or other permissions",
            Some("Run chmod 600 /data/auth.json."),
        ),
        (
            ProviderError::AuthFileSymlink {
                path: PathBuf::from("/data/auth.json"),
            },
            "auth.json is a symbolic link; dalgon does not follow it",
            Some("Replace /data/auth.json with a regular file."),
        ),
        (
            ProviderError::AuthFileInvalid {
                path: PathBuf::from("/data/auth.json"),
                message: String::from("trailing bytes"),
            },
            "auth.json is not valid: trailing bytes",
            Some("Fix or delete /data/auth.json, then run dalgon login."),
        ),
        (
            ProviderError::AuthWrite {
                reason: String::from("disk full"),
            },
            "could not write auth.json: disk full",
            None,
        ),
        (
            ProviderError::CallbackBind {
                port: 7437,
                reason: String::from("address in use"),
            },
            "could not listen on 127.0.0.1:7437: address in use",
            None,
        ),
        (
            ProviderError::StateMismatch,
            "sign-in failed: the OAuth state does not match.",
            None,
        ),
        (
            ProviderError::LoginTimeout,
            "sign-in timed out after 15 minutes.",
            None,
        ),
        (
            ProviderError::TokenExchange {
                status: 400,
                message: String::from("bad code"),
            },
            "sign-in failed: the token endpoint returned 400: bad code",
            None,
        ),
        (
            ProviderError::DeviceCode {
                status: 500,
                message: String::from("boom"),
            },
            "sign-in failed: the device code endpoint returned 500: boom",
            None,
        ),
        (
            ProviderError::NoAccountId,
            "sign-in failed: the ID token has no chatgpt_account_id.",
            None,
        ),
        (ProviderError::LoginCancelled, "sign-in cancelled.", None),
        (
            ProviderError::UsageCheck {
                reason: UsageCheckReason::Timeout,
            },
            "usage failed: no reply within 15 s",
            None,
        ),
        (
            ProviderError::UsageCheck {
                reason: UsageCheckReason::NotJsonObject,
            },
            "usage failed: the body is not a JSON object",
            None,
        ),
        (
            ProviderError::UsageCheck {
                reason: UsageCheckReason::Status {
                    status: 401,
                    message: String::from("denied"),
                },
            },
            "usage failed: 401 denied",
            None,
        ),
        (
            ProviderError::ContextOverflow {
                family: Family::Responses,
                code: String::from("context_length_exceeded"),
                message: String::from("input exceeds the window\ndetail"),
            },
            "openai context window exceeded (context_length_exceeded): input exceeds the window",
            None,
        ),
    ];
    for (error, display, fix) in cases {
        assert_eq!(error.to_string(), *display);
        assert_eq!(error.fix().as_deref(), *fix);
    }
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
        model: String::from("gpt-reserve"),
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
    ];
    for error in terminal {
        assert!(!error.retryable_by_loop(), "{error}");
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
    for code in ["invalid_request_error", "Context_Length_Exceeded", ""] {
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
    let limited = ProviderError::RateLimited {
        message: String::from("slow down"),
        retry_after: Some(Duration::from_millis(2_500)),
    };
    let failure = InferFailure::from(limited);
    assert!(
        matches!(
            &failure,
            InferFailure::Retryable { hint: Some(hint), message }
                if *hint == Duration::from_millis(2_500) && &**message == "rate limited: slow down"
        ),
        "{failure:?}"
    );
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
}
