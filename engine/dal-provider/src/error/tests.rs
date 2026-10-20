use std::{collections::BTreeMap, mem, path::PathBuf, time::Duration};

use super::*;
use crate::{
    run::RunLostReason,
    scripted::{Operation, ScriptError},
    usage::LUNA_RESERVE_MODEL,
};
use dal_core::InferFailure;

/// Every provider API family, in declaration order.
const ALL_FAMILIES: [Family; 16] = [
    Family::Chat,
    Family::Responses,
    Family::Codex,
    Family::Anthropic,
    Family::Bedrock,
    Family::Cursor,
    Family::Devin,
    Family::Droid,
    Family::GitLab,
    Family::GoogleGenerativeAi,
    Family::GoogleVertex,
    Family::GoogleGeminiCli,
    Family::GoogleAntigravity,
    Family::Mistral,
    Family::Ollama,
    Family::PiMessages,
];

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
            Some("Retry the request, or ask for a smaller reply."),
        ),
        (
            ProviderError::Limit(LimitError::SseEvent),
            "SSE event exceeds 8 MiB.",
            Some("Retry the request, or ask for a smaller reply."),
        ),
        (
            ProviderError::Limit(LimitError::WsMessage),
            "WebSocket message exceeds 16 MiB.",
            Some("Retry the request, or ask for a smaller reply."),
        ),
        (
            ProviderError::Limit(LimitError::Body),
            "response body exceeds 16 MiB.",
            Some("Retry the request, or ask for a smaller reply."),
        ),
        (
            ProviderError::Limit(LimitError::Payload { limit: 1024 }),
            "response body exceeds its 1024 byte limit.",
            Some("Retry the request, or ask for a smaller reply."),
        ),
        (
            ProviderError::Limit(LimitError::StreamBytes),
            "provider stream exceeds 128 MiB of event data.",
            Some("Retry the request, or split the task into smaller steps."),
        ),
        (
            ProviderError::Limit(LimitError::StreamEvents),
            "provider stream exceeds 65536 events.",
            Some("Retry the request, or split the task into smaller steps."),
        ),
        (
            ProviderError::Limit(LimitError::StreamItems),
            "provider stream exceeds 16384 retained items.",
            Some("Retry the request, or split the task into smaller steps."),
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
                model: String::from(LUNA_RESERVE_MODEL),
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
            ProviderError::Cancelled,
            "provider operation cancelled.",
            None,
        ),
        (
            ProviderError::LoginInput {
                reason: String::from("the API key is empty."),
            },
            "sign-in failed: the API key is empty.",
            None,
        ),
        (
            ProviderError::ToolNameCollision {
                wire: String::from("read_file"),
                first: String::from("fs.read"),
                second: String::from("fs_read"),
            },
            "tools fs.read and fs_read both map to the provider tool name read_file; no request was sent.",
            None,
        ),
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
        (
            ProviderError::RunLost {
                family: Family::Chat,
                reason: RunLostReason::ServerClosed,
            },
            "openai run lost: the server ended the run before its final response",
            None,
        ),
        (
            ProviderError::RunLost {
                family: Family::Chat,
                reason: RunLostReason::Transport(String::from("reset")),
            },
            "openai run lost: transport failure: reset",
            None,
        ),
        (
            ProviderError::RunLost {
                family: Family::Chat,
                reason: RunLostReason::ResultsMismatch,
            },
            "openai run lost: the tool results are not exactly the pending tool calls",
            None,
        ),
        (
            ProviderError::RunLost {
                family: Family::Anthropic,
                reason: RunLostReason::Protocol(String::from("bad frame")),
            },
            "anthropic run lost: invalid stream: bad frame",
            None,
        ),
        (
            ProviderError::RunLost {
                family: Family::Codex,
                reason: RunLostReason::BufferExceeded,
            },
            "codex run lost: a buffer bound was exceeded",
            None,
        ),
        (
            ProviderError::RunLost {
                family: Family::Chat,
                reason: RunLostReason::Authority("web_fetch".into()),
            },
            "openai run lost: the server acted beyond the host's authority: web_fetch",
            None,
        ),
    ];
    let mut wrong = Vec::new();
    for (error, display, fix) in cases {
        let (text, next) = (error.to_string(), error.fix());
        if text != *display || next.as_deref() != *fix {
            wrong.push(format!(
                "{error:?}: expected {display:?} / {fix:?}, got {text:?} / {next:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
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
fn only_http_413_folds_to_a_request_size_overflow_in_every_family() {
    let statuses = (0..=700).chain([u16::MAX - 1, u16::MAX]);
    for family in ALL_FAMILIES {
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
    for family in ALL_FAMILIES {
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
    assert_eq!(seen.len(), ALL_FAMILIES.len() - 1);
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
        43,
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
