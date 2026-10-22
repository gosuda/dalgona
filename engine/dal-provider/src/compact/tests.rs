use super::*;
use super::{
    attempt::{chat, openai_codex, openai_responses},
    bodies::{
        codex_compaction_body, compact_responses_body, decode_codex_events, parse_anthropic_block,
        parse_responses_output, retain_user_messages,
    },
    support::{redact, redact_stream_error},
};
use std::time::Duration;
use std::{borrow::Cow, sync::atomic::AtomicBool};

use dal_core::{Family, RawJson, SessionId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::{
    error::ProviderError,
    family::{anthropic::AnthropicWire, codex::CodexWire},
    http,
    lifecycle::AttemptFailure,
    sse::SseEvent,
};

#[test]
fn responses_compaction_body_preserves_wire_values_byte_for_byte() {
    let request = br#"{"model":"m,1","instructions":"keep \"exact\"","input":[ {"role":"user","content":[{"text":"x","n":1.00}]} ],"tools":[],"store":false,"stream":true}"#;
    assert_eq!(
            compact_responses_body(request, "m,1").expect("valid Responses body"),
            br#"{"model":"m,1","instructions":"keep \"exact\"","input":[ {"role":"user","content":[{"text":"x","n":1.00}]} ]}"#
        );
}

#[test]
fn codex_trigger_is_last_input_and_existing_items_keep_their_bytes() {
    let request = br#"{"model":"m","instructions":"i","input":[{"role":"user", "content":[{"text":"old"}]}],"store":false,"stream":true}"#;
    let (retained, body) = codex_compaction_body(request).expect("valid Codex body");
    assert_eq!(retained.len(), 1);
    assert_eq!(
            body,
            br#"{"model":"m","instructions":"i","input":[{"role":"user", "content":[{"text":"old"}]},{"type":"compaction_trigger"}],"store":false,"stream":true}"#
        );
}

#[test]
fn codex_retention_keeps_newest_users_in_original_order_with_a_byte_budget() {
    let old = RawJson::parse(&format!(
        "{{\"role\":\"user\",\"text\":\"{}\"}}",
        "a".repeat(140_000)
    ))
    .expect("valid old message");
    let middle = RawJson::parse(&format!(
        "{{\"role\":\"user\",\"text\":\"{}\"}}",
        "c".repeat(100_000)
    ))
    .expect("valid middle message");
    let assistant = RawJson::parse(r#"{"role":"assistant","text":"not retained"}"#)
        .expect("valid assistant item");
    let newest = RawJson::parse(&format!(
        "{{\"role\":\"user\",\"text\":\"{}\"}}",
        "b".repeat(120_000)
    ))
    .expect("valid newest message");
    let input = [
        Cow::Borrowed(old.as_str()),
        Cow::Borrowed(middle.as_str()),
        Cow::Borrowed(assistant.as_str()),
        Cow::Borrowed(newest.as_str()),
    ];
    let retained = retain_user_messages(&input).expect("valid Responses input items");
    assert_eq!(
        retained.iter().map(RawJson::as_str).collect::<Vec<_>>(),
        [middle.as_str(), newest.as_str()]
    );
}

#[tokio::test]
async fn codex_stream_without_compaction_item_is_typed_missing() {
    let event = SseEvent {
        name: None,
        data: String::from(r#"{"type":"response.completed","response":{"output":[]}}"#),
    };
    let read_failed = AtomicBool::new(false);
    let cancel = CancellationToken::new();
    let result = decode_codex_events(
        futures::stream::iter([Ok(event)]),
        String::from("gpt-6").into_boxed_str(),
        Vec::new(),
        &read_failed,
        &[],
        &cancel,
    )
    .await;
    assert!(matches!(
        result,
        Err(AttemptFailure::Provider(ProviderError::CompactionMissing {
            family: Family::Codex,
            noun: "item",
        }))
    ));
}

#[tokio::test]
async fn codex_stream_keeps_first_compaction_alias_raw() {
    let first = r#"{"type":"context_compaction", "encrypted_content":"secret", "n":1.00}"#;
    let second = r#"{"type":"compaction_summary","summary":"second"}"#;
    let events = [
        SseEvent {
            name: None,
            data: format!(r#"{{"type":"response.output_item.done","item":{first}}}"#),
        },
        SseEvent {
            name: None,
            data: format!(r#"{{"type":"response.output_item.done","item":{second}}}"#),
        },
        SseEvent {
            name: None,
            data: String::from(r#"{"type":"response.completed","response":{"output":[]}}"#),
        },
    ];
    let read_failed = AtomicBool::new(false);
    let cancel = CancellationToken::new();
    let history = decode_codex_events(
        futures::stream::iter(events.into_iter().map(Ok)),
        String::from("gpt-6").into_boxed_str(),
        Vec::new(),
        &read_failed,
        &[Box::from("secret")],
        &cancel,
    )
    .await
    .expect("complete compaction item")
    .expect("not cancelled");
    assert_eq!(history.items[0].as_str(), first);
}

#[test]
fn codex_error_message_redacts_token_without_rewriting_output_items() {
    let error = redact_stream_error(
        ProviderError::RateLimited {
            message: String::from("secret rejected"),
            retry_after: Some(Duration::from_secs(2)),
        },
        &[Box::from("secret")],
    );
    assert!(matches!(
        error,
        ProviderError::RateLimited { message, retry_after: Some(wait) }
            if message == "<redacted> rejected" && wait == Duration::from_secs(2)
    ));
}

#[test]
fn response_output_and_anthropic_block_are_preserved_raw() {
    let responses = br#"{"output":[ {"type":"compaction", "encrypted_content":"enc-v1"} ]}"#;
    let history = parse_responses_output(responses, "gpt-6").expect("valid Responses fixture");
    assert_eq!(history.family, Family::Responses);
    assert_eq!(history.model.as_ref(), "gpt-6");
    assert_eq!(
        history.items[0].as_str(),
        r#"{"type":"compaction", "encrypted_content":"enc-v1"}"#
    );

    let anthropic =
        br#"{"stop_reason":"compaction","content":[ {"type":"compaction", "content":"summary"} ]}"#;
    let block =
        parse_anthropic_block(anthropic, "claude-sonnet-5").expect("valid Anthropic fixture");
    assert_eq!(
        block.items[0].as_str(),
        r#"{"type":"compaction", "content":"summary"}"#
    );
}

#[test]
fn missing_anthropic_compaction_block_is_typed() {
    let error = parse_anthropic_block(
        br#"{"stop_reason":"end_turn","content":[{"type":"text","text":"no summary"}]}"#,
        "claude-sonnet-5",
    )
    .expect_err("ordinary response is not a compaction");
    assert!(matches!(
        error,
        ProviderError::CompactionMissing {
            family: Family::Anthropic,
            noun: "block"
        }
    ));
}

#[test]
fn compacted_history_refuses_another_family_or_model() {
    let history = CompactedHistory {
        family: Family::Responses,
        model: "gpt-6".into(),
        items: vec![RawJson::parse(r#"{"type":"message"}"#).expect("valid item")],
    };
    let items = items_for(&history, Family::Responses, "gpt-6").expect("the producing identity");
    assert_eq!(
        items.iter().map(RawJson::as_str).collect::<Vec<_>>(),
        [r#"{"type":"message"}"#]
    );
    assert!(matches!(
        items_for(&history, Family::Codex, "gpt-6"),
        Err(ProviderError::CompactionForeign {
            bound_family: Family::Responses,
            ..
        })
    ));
    assert!(matches!(
        items_for(&history, Family::Responses, "gpt-7"),
        Err(ProviderError::CompactionForeign { bound_model, model, .. })
            if bound_model == "gpt-6" && model == "gpt-7"
    ));
}
#[test]
fn chat_is_unsupported_without_a_transport_or_request_argument() {
    assert_eq!(chat(), CompactOutcome::Unsupported);
}

#[tokio::test]
async fn cancelling_an_in_flight_responses_compaction_drops_the_request() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("loopback bind");
    let address = listener.local_addr().expect("listener address");
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let (_socket, _) = listener.accept().await.expect("request accepted");
        let _ = accepted_tx.send(());
        std::future::pending::<()>().await;
    });
    let client = http::build_client();
    let cancel = CancellationToken::new();
    let body = br#"{"model":"m","instructions":"i","input":[]}"#;
    let mut attempts = tokio::task::JoinSet::new();
    attempts.spawn({
        let client = client.clone();
        let cancel = cancel.clone();
        async move {
            openai_responses(
                &client,
                &format!("http://{address}/v1"),
                "m",
                body,
                &[],
                "dalgon/test (test test; x64)",
                &cancel,
            )
            .await
        }
    });
    accepted_rx.await.expect("request reached the server");
    cancel.cancel();
    let joined = attempts.join_next().await.expect("attempt finished");
    let result = joined.expect("attempt task joined");
    assert!(result.expect("cancelled attempt").is_none());
    attempts.abort_all();
    while attempts.join_next().await.is_some() {}
    servers.abort_all();
    while servers.join_next().await.is_some() {}
}

async fn codex_compaction_failure(body: &str) -> AttemptFailure {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("loopback bind");
    let address = listener.local_addr().expect("listener address");
    let response = format!(
        "HTTP/1.1 400 Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("request accepted");
        let mut seen = Vec::new();
        let mut chunk = [0_u8; 4096];
        while !seen.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut chunk).await.expect("request read");
            seen.extend_from_slice(&chunk[..read]);
        }
        socket
            .write_all(response.as_bytes())
            .await
            .expect("response written");
    });
    let wire = CodexWire {
        headers: vec![
            ("authorization", String::from("Bearer tok-secret")),
            ("chatgpt-account-id", String::from("acct-secret")),
        ],
        body: br#"{"model":"m","instructions":"i","input":[],"store":false,"stream":true}"#
            .to_vec(),
        model: Box::from("m"),
        session_id: SessionId::new_v7(),
        user_agent: String::from("dalgon/test (test test; x64)"),
    };
    let failure = openai_codex(
        &http::build_client(),
        &format!("http://{address}/v1"),
        wire,
        &CancellationToken::new(),
    )
    .await
    .expect_err("a 400 response is a failure");
    servers.abort_all();
    while servers.join_next().await.is_some() {}
    failure
}

#[tokio::test]
async fn codex_compaction_failures_redact_access_token_and_account_id() {
    for body in [
        r#"{"error":{"code":"echo-acct-secret","message":"tok-secret / acct-secret"}}"#,
        r#"{"error":{"code":"echo-acct\u002dsecret","message":"tok\u002dsecret / acct\u002dsecret"}}"#,
    ] {
        let failure = codex_compaction_failure(body).await;
        assert!(matches!(
            &failure,
            AttemptFailure::Response { status: 400, code: Some(code), message, .. }
                if code == "echo-<redacted>" && message == "<redacted> / <redacted>"
        ));
    }
}

/// A user message whose JSON text is exactly `total_bytes` long.
fn user_item(total_bytes: usize) -> String {
    let frame = r#"{"role":"user","text":""}"#.len();
    format!(
        r#"{{"role":"user","text":"{}"}}"#,
        "a".repeat(total_bytes - frame)
    )
}

fn retained_lengths(items: &[String]) -> Vec<usize> {
    let inputs: Vec<Cow<'_, str>> = items
        .iter()
        .map(|item| Cow::Borrowed(item.as_str()))
        .collect();
    retain_user_messages(&inputs)
        .expect("valid Responses input items")
        .iter()
        .map(|item| item.as_str().len())
        .collect()
}

#[test]
fn codex_retention_budget_is_inclusive_at_the_exact_byte_limit() {
    let limit = CODEX_RETAINED_USER_BYTE_LIMIT;
    assert_eq!(retained_lengths(&[user_item(limit - 1)]), [limit - 1]);
    assert_eq!(retained_lengths(&[user_item(limit)]), [limit]);
    assert_eq!(
        retained_lengths(&[user_item(limit + 1)]),
        Vec::<usize>::new()
    );
    // The budget is shared: two messages that together fill it exactly both
    // stay, one byte more drops the older.
    let half = limit / 2;
    assert_eq!(limit, half * 2);
    assert_eq!(
        retained_lengths(&[user_item(half), user_item(half)]),
        [half, half]
    );
    assert_eq!(
        retained_lengths(&[user_item(half + 1), user_item(half)]),
        [half]
    );
}

#[test]
fn codex_retention_keeps_only_user_items_and_refuses_malformed_ones() {
    let items = [
        r#"{"role":"user","text":"first"}"#,
        r#"{"role":"assistant","text":"a"}"#,
        r#"{"role":"developer","text":"d"}"#,
        r#"{"role":"system","text":"s"}"#,
        r#"{"type":"function_call_output","call_id":"c","output":"o"}"#,
        r#"{"role":"user","text":"second"}"#,
        r#"{"role":null,"text":"n"}"#,
    ];
    let inputs: Vec<Cow<'_, str>> = items.iter().map(|item| Cow::Borrowed(*item)).collect();
    let retained = retain_user_messages(&inputs).expect("valid Responses input items");
    assert_eq!(
        retained.iter().map(RawJson::as_str).collect::<Vec<_>>(),
        [items[0], items[5]]
    );
    for malformed in ["not json", "42", r#""user""#, r#"{"role":7}"#] {
        let error = retain_user_messages(&[Cow::Borrowed(malformed)])
            .expect_err("a malformed input item is refused");
        assert!(
            matches!(
                &error,
                ProviderError::InvalidRequest { message }
                    if message.starts_with("Responses input item is invalid")
            ),
            "{malformed}: {error}"
        );
    }
}

fn refusal<T: std::fmt::Debug>(result: Result<T, ProviderError>) -> String {
    match result {
        Err(ProviderError::InvalidRequest { message }) => message,
        other => panic!("expected an invalid-request refusal, got {other:?}"),
    }
}

#[test]
fn responses_compaction_body_refuses_hostile_bodies_with_the_exact_reason() {
    assert_eq!(
        refusal(compact_responses_body(
            br#"{"instructions":"i","input":[]}"#,
            "m"
        )),
        "Responses body has no model"
    );
    assert_eq!(
        refusal(compact_responses_body(br#"{"model":"m","input":[]}"#, "m")),
        "Responses body has no instructions"
    );
    assert_eq!(
        refusal(compact_responses_body(
            br#"{"model":"m","instructions":"i"}"#,
            "m"
        )),
        "Responses body has no input"
    );
    // Any other spelling of the model is another model.
    for actual in [r#""m-mini""#, r#""M""#, r#""m ""#, r#""""#, r#""m\u0000""#] {
        let body = format!(r#"{{"model":{actual},"instructions":"i","input":[]}}"#);
        assert_eq!(
            refusal(compact_responses_body(body.as_bytes(), "m")),
            "Responses compaction body model does not match its request",
            "{actual}"
        );
    }
    // An escaped spelling of the same model matches, and its raw bytes survive.
    let escaped = br#"{"model":"m\u002d1","instructions":"i","input":[]}"#;
    assert_eq!(
        compact_responses_body(escaped, "m-1").expect("an escaped model matches"),
        escaped
    );
    assert!(
        refusal(compact_responses_body(&[0xff, 0xfe], "m")).starts_with("invalid request body")
    );
    assert!(
        refusal(compact_responses_body(
            br#"{"model":"m","instructions":"i","input":["#,
            "m"
        ))
        .starts_with("invalid request body")
    );
    assert!(
        refusal(compact_responses_body(
            br#"{"model":7,"instructions":"i","input":[]}"#,
            "m"
        ))
        .starts_with("invalid model")
    );
}

#[test]
fn codex_trigger_lands_after_the_last_input_item_whatever_the_input_shape() {
    for (request, expected) in [
        (
            r#"{"model":"m","input":[],"store":false}"#,
            r#"{"model":"m","input":[{"type":"compaction_trigger"}],"store":false}"#,
        ),
        (
            r#"{"model":"m","input":[ ],"store":false}"#,
            r#"{"model":"m","input":[{"type":"compaction_trigger"} ],"store":false}"#,
        ),
        (
            r#"{"input":[{"a":1} , {"b":2} ],"model":"m"}"#,
            r#"{"input":[{"a":1} , {"b":2},{"type":"compaction_trigger"} ],"model":"m"}"#,
        ),
    ] {
        let (retained, body) =
            codex_compaction_body(request.as_bytes()).expect("a valid Codex body");
        assert!(retained.is_empty(), "{request}");
        assert_eq!(
            String::from_utf8(body).expect("UTF-8"),
            expected,
            "{request}"
        );
    }
}

#[test]
fn codex_compaction_body_refuses_a_missing_or_non_array_input() {
    assert_eq!(
        refusal(codex_compaction_body(br#"{"model":"m"}"#)),
        "Responses body has no input"
    );
    for input in [r#""x""#, "{}", "7", "null"] {
        let body = format!(r#"{{"model":"m","input":{input}}}"#);
        let message = refusal(codex_compaction_body(body.as_bytes()));
        assert!(
            message.starts_with("invalid input array"),
            "{input}: {message}"
        );
    }
    assert!(refusal(codex_compaction_body(&[0xff])).starts_with("invalid request body"));
}

#[test]
fn stream_error_redaction_covers_every_message_bearing_variant_and_skips_blank_secrets() {
    let secrets = [Box::<str>::from("tok-1"), Box::<str>::from("")];
    let text = String::from("tok-1 and again tok-1");
    let redacted = "<redacted> and again <redacted>";
    assert!(matches!(
        redact_stream_error(
            ProviderError::Status { family: Family::Codex, status: 502, message: text.clone() },
            &secrets
        ),
        ProviderError::Status { family: Family::Codex, status: 502, message }
            if message == redacted
    ));
    assert!(matches!(
        redact_stream_error(
            ProviderError::ContextOverflow {
                family: Family::Codex,
                code: String::from("context_length_exceeded"),
                message: text.clone(),
            },
            &secrets
        ),
        ProviderError::ContextOverflow { family: Family::Codex, code, message }
            if code == "context_length_exceeded" && message == redacted
    ));
    assert!(matches!(
        redact_stream_error(
            ProviderError::RateLimited {
                message: text.clone(),
                retry_after: Some(Duration::from_secs(5)),
            },
            &secrets
        ),
        ProviderError::RateLimited { message, retry_after: Some(wait) }
            if message == redacted && wait == Duration::from_secs(5)
    ));
    assert!(matches!(
        redact_stream_error(ProviderError::UsageNotIncluded { message: text }, &secrets),
        ProviderError::UsageNotIncluded { message } if message == redacted
    ));
    // A blank secret must not turn every gap into a placeholder.
    assert!(matches!(
        redact_stream_error(
            ProviderError::Status {
                family: Family::Codex,
                status: 500,
                message: String::from("plain failure"),
            },
            &[Box::<str>::from("")]
        ),
        ProviderError::Status { message, .. } if message == "plain failure"
    ));
}

#[test]
fn header_redaction_strips_the_bearer_scheme_and_ignores_blank_values() {
    let mut message = String::from("Bearer tok-1 was refused; tok-1 expired");
    redact(&mut message, &[("authorization", "Bearer tok-1")]);
    assert_eq!(message, "Bearer <redacted> was refused; <redacted> expired");
    let mut message = String::from("unchanged");
    redact(
        &mut message,
        &[
            ("x-api-key", ""),
            ("authorization", "Bearer "),
            ("chatgpt-account-id", ""),
        ],
    );
    assert_eq!(message, "unchanged");
}

fn codex_sse(data: &str) -> SseEvent {
    SseEvent {
        name: None,
        data: data.to_owned(),
    }
}

fn compaction_done(item: &str) -> String {
    format!(r#"{{"type":"response.output_item.done","item":{item}}}"#)
}

const CODEX_COMPLETED: &str = r#"{"type":"response.completed","response":{"output":[]}}"#;
const CODEX_INCOMPLETE: &str = r#"{"type":"response.incomplete","response":{"output":[],"incomplete_details":{"reason":"max_output_tokens"}}}"#;

async fn decode_codex_with(
    events: &[&str],
    retained: Vec<RawJson>,
    secrets: &[Box<str>],
    cancel: &CancellationToken,
    read_failed: &AtomicBool,
) -> AttemptResult {
    decode_codex_events(
        futures::stream::iter(events.iter().map(|data| Ok(codex_sse(data)))),
        String::from("gpt-6").into_boxed_str(),
        retained,
        read_failed,
        secrets,
        cancel,
    )
    .await
}

/// Decodes `events` as one Codex compaction response whose body read never failed.
async fn decode_codex(
    events: &[&str],
    retained: Vec<RawJson>,
    secrets: &[Box<str>],
    cancel: &CancellationToken,
) -> AttemptResult {
    decode_codex_with(events, retained, secrets, cancel, &AtomicBool::new(false)).await
}

#[tokio::test]
async fn every_compaction_item_alias_is_recognized_and_near_misses_are_not() {
    for kind in ["compaction", "compaction_summary", "context_compaction"] {
        let item = format!(r#"{{"type":"{kind}","encrypted_content":"enc"}}"#);
        let done = compaction_done(&item);
        let history = decode_codex(
            &[&done, CODEX_COMPLETED],
            Vec::new(),
            &[],
            &CancellationToken::new(),
        )
        .await
        .expect("a complete compaction stream")
        .expect("not cancelled");
        assert_eq!(
            history
                .items
                .iter()
                .map(RawJson::as_str)
                .collect::<Vec<_>>(),
            [item.as_str()],
            "{kind}"
        );
    }
    for kind in [
        "compactions",
        "Compaction",
        "compaction_trigger",
        "message",
        "",
    ] {
        let done = compaction_done(&format!(r#"{{"type":"{kind}"}}"#));
        let result = decode_codex(
            &[&done, CODEX_COMPLETED],
            Vec::new(),
            &[],
            &CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(
                result,
                Err(AttemptFailure::Provider(ProviderError::CompactionMissing {
                    family: Family::Codex,
                    noun: "item",
                }))
            ),
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn retained_user_messages_precede_the_compaction_item_in_the_history() {
    let first = RawJson::parse(r#"{"role":"user","text":"first"}"#).expect("valid user item");
    let second = RawJson::parse(r#"{"role":"user","text":"second"}"#).expect("valid user item");
    let item = r#"{"type":"compaction","encrypted_content":"enc"}"#;
    let done = compaction_done(item);
    let history = decode_codex(
        &[&done, CODEX_COMPLETED],
        vec![first.clone(), second.clone()],
        &[],
        &CancellationToken::new(),
    )
    .await
    .expect("a complete compaction stream")
    .expect("not cancelled");
    assert_eq!(history.family, Family::Codex);
    assert_eq!(history.model.as_ref(), "gpt-6");
    assert_eq!(
        history
            .items
            .iter()
            .map(RawJson::as_str)
            .collect::<Vec<_>>(),
        [first.as_str(), second.as_str(), item]
    );
}

#[tokio::test]
async fn an_incomplete_terminal_never_publishes_a_history() {
    let done = compaction_done(r#"{"type":"compaction","encrypted_content":"enc"}"#);
    let with_item = decode_codex(
        &[&done, CODEX_INCOMPLETE],
        Vec::new(),
        &[],
        &CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(
            &with_item,
            Err(AttemptFailure::Provider(ProviderError::Protocol { family: Family::Codex, detail }))
                if detail == "compaction response did not complete"
        ),
        "{with_item:?}"
    );
    let without_item = decode_codex(
        &[CODEX_INCOMPLETE],
        Vec::new(),
        &[],
        &CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(
            &without_item,
            Err(AttemptFailure::Provider(ProviderError::CompactionMissing {
                family: Family::Codex,
                noun: "item",
            }))
        ),
        "{without_item:?}"
    );
}

#[tokio::test]
async fn a_stream_that_ends_before_its_terminal_is_cut_or_read_failed_never_a_history() {
    let done = compaction_done(r#"{"type":"compaction","encrypted_content":"enc"}"#);
    let cut = decode_codex(&[&done], Vec::new(), &[], &CancellationToken::new()).await;
    assert!(
        matches!(
            &cut,
            Err(AttemptFailure::Provider(ProviderError::StreamCut))
        ),
        "{cut:?}"
    );
    let failed = decode_codex_with(
        &[&done],
        Vec::new(),
        &[],
        &CancellationToken::new(),
        &AtomicBool::new(true),
    )
    .await;
    assert!(
        matches!(
            &failed,
            Err(AttemptFailure::Provider(ProviderError::Transport { family: Family::Codex, reason }))
                if reason == "Codex response body read failed"
        ),
        "{failed:?}"
    );
}

#[tokio::test]
async fn a_cancelled_attempt_publishes_no_history_even_when_the_stream_is_complete() {
    let done = compaction_done(r#"{"type":"compaction","encrypted_content":"enc"}"#);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = decode_codex(&[&done, CODEX_COMPLETED], Vec::new(), &[], &cancel).await;
    assert!(matches!(result, Ok(None)), "{result:?}");
}

#[tokio::test]
async fn an_in_stream_failure_is_redacted_before_it_surfaces() {
    let secrets = [Box::<str>::from("secret")];
    let limited = r#"{"type":"response.failed","response":{"output":[],"error":{"code":"rate_limit_exceeded","message":"secret over quota"}}}"#;
    let result = decode_codex(&[limited], Vec::new(), &secrets, &CancellationToken::new()).await;
    assert!(
        matches!(
            &result,
            Err(AttemptFailure::Provider(ProviderError::RateLimited { message, retry_after: None }))
                if message == "<redacted> over quota"
        ),
        "{result:?}"
    );
    let broken =
        r#"{"type":"response.failed","response":{"output":[],"error":{"message":"secret broke"}}}"#;
    let result = decode_codex(&[broken], Vec::new(), &secrets, &CancellationToken::new()).await;
    assert!(
        matches!(
            &result,
            Err(AttemptFailure::Provider(ProviderError::Status { family: Family::Codex, message, .. }))
                if message == "<redacted> broke"
        ),
        "{result:?}"
    );
}

#[test]
fn anthropic_replies_without_a_genuine_compaction_block_are_refused_by_kind() {
    for reply in [
        r#"{"stop_reason":"compaction","content":[]}"#,
        r#"{"stop_reason":"compaction"}"#,
        r#"{"stop_reason":"compaction","content":[{"type":"text","text":"t"}]}"#,
        r#"{"stop_reason":"compaction","content":[{"type":"compaction_summary"}]}"#,
        r#"{"stop_reason":"compaction","content":[{"type":"Compaction"}]}"#,
        r#"{"stop_reason":"Compaction","content":[{"type":"compaction"}]}"#,
        r#"{"stop_reason":null,"content":[{"type":"compaction"}]}"#,
        r#"{"content":[{"type":"compaction"}]}"#,
    ] {
        let error = parse_anthropic_block(reply.as_bytes(), "claude-sonnet-5")
            .expect_err("not a compaction reply");
        assert!(
            matches!(
                &error,
                ProviderError::CompactionMissing {
                    family: Family::Anthropic,
                    noun: "block"
                }
            ),
            "{reply}: {error}"
        );
    }
    for reply in [
        r#"{"stop_reason":"compaction","content":[{"text":"no type"}]}"#,
        r#"{"stop_reason":"compaction","content":[7]}"#,
    ] {
        let error = parse_anthropic_block(reply.as_bytes(), "claude-sonnet-5")
            .expect_err("a block without a kind is refused");
        assert!(
            matches!(
                &error,
                ProviderError::Protocol { family: Family::Anthropic, detail }
                    if detail.starts_with("malformed compaction block")
            ),
            "{reply}: {error}"
        );
    }
    for reply in ["", "{", "[]", r#"{"stop_reason":7}"#, r#"{"content":"x"}"#] {
        let error = parse_anthropic_block(reply.as_bytes(), "claude-sonnet-5")
            .expect_err("a malformed reply is refused");
        assert!(
            matches!(
                &error,
                ProviderError::Protocol { family: Family::Anthropic, detail }
                    if detail.starts_with("malformed compact response")
            ),
            "{reply:?}: {error}"
        );
    }
    let genuine = parse_anthropic_block(
        br#"{"stop_reason":"compaction","content":[{"type":"compaction","content":"s"}]}"#,
        "claude-sonnet-5",
    )
    .expect("a genuine compaction block");
    assert_eq!(genuine.family, Family::Anthropic);
    assert_eq!(genuine.model.as_ref(), "claude-sonnet-5");
}

#[test]
fn a_malformed_responses_compact_reply_is_a_typed_protocol_error_never_an_empty_history() {
    for reply in [
        "",
        "{",
        "{}",
        "[]",
        r#"{"output":"x"}"#,
        r#"{"output":{"a":1}}"#,
    ] {
        let error = parse_responses_output(reply.as_bytes(), "gpt-6")
            .expect_err("a malformed reply is refused");
        assert!(
            matches!(
                &error,
                ProviderError::Protocol { family: Family::Responses, detail }
                    if detail.starts_with("malformed compact response")
            ),
            "{reply:?}: {error}"
        );
    }
}

#[test]
fn the_compaction_binding_is_exact_on_family_and_model_and_the_refusal_names_both_sides() {
    let history = CompactedHistory {
        family: Family::Anthropic,
        model: "claude-sonnet-5".into(),
        items: vec![RawJson::parse(r#"{"type":"compaction"}"#).expect("valid item")],
    };
    for model in [
        "claude-sonnet-5-20260101",
        "Claude-Sonnet-5",
        "claude-sonnet-5 ",
        "claude-sonnet-",
        "",
    ] {
        let error =
            items_for(&history, Family::Anthropic, model).expect_err("another model is refused");
        assert!(
            matches!(
                &error,
                ProviderError::CompactionForeign { bound_family: Family::Anthropic, bound_model, family: Family::Anthropic, model: asked }
                    if bound_model == "claude-sonnet-5" && asked == model
            ),
            "{model:?}: {error}"
        );
    }
    let error = items_for(&history, Family::Responses, "claude-sonnet-5")
        .expect_err("another family is refused");
    assert!(matches!(
        &error,
        ProviderError::CompactionForeign { bound_family: Family::Anthropic, bound_model, family: Family::Responses, model }
            if bound_model == "claude-sonnet-5" && model == "claude-sonnet-5"
    ));
}

#[tokio::test]
async fn anthropic_compaction_refuses_a_body_for_another_model_before_any_request() {
    let wire = |body: &[u8]| AnthropicWire {
        path: "v1/messages",
        headers: Vec::new(),
        user_agent: None,
        body: body.to_vec(),
        prefix: Box::from(""),
    };
    let run = |body: &[u8]| {
        let wire = wire(body);
        async move {
            anthropic(
                &http::build_client(),
                "http://127.0.0.1:9",
                "claude-sonnet-5",
                wire,
                "dalgon/test (test test; x64)",
                &CancellationToken::new(),
            )
            .await
        }
    };
    for (body, expected) in [
        (
            br#"{"model":"claude-opus-5"}"#.as_slice(),
            "Anthropic compaction body model does not match its request",
        ),
        (
            br#"{"model":"Claude-Sonnet-5"}"#.as_slice(),
            "Anthropic compaction body model does not match its request",
        ),
        (
            br#"{"model":"claude-sonnet-5 "}"#.as_slice(),
            "Anthropic compaction body model does not match its request",
        ),
        (
            br#"{"system":"x"}"#.as_slice(),
            "provider request body has no model member",
        ),
    ] {
        let result = run(body).await;
        assert!(
            matches!(
                &result,
                Err(AttemptFailure::Provider(ProviderError::InvalidRequest { message }))
                    if message == expected
            ),
            "{}: {result:?}",
            String::from_utf8_lossy(body)
        );
    }
}
