use super::*;
use super::{
    attempt::{chat, openai_responses},
    bodies::{
        codex_compaction_body, compact_responses_body, decode_codex_events, parse_anthropic_block,
        parse_responses_output, retain_user_messages,
    },
    support::redact_stream_error,
};
use std::time::Duration;
use std::{borrow::Cow, sync::atomic::AtomicBool};

use dal_core::{Family, RawJson};
use tokio_util::sync::CancellationToken;

use crate::{error::ProviderError, http, lifecycle::AttemptFailure, sse::SseEvent};

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
        "",
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
        "secret",
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
        "secret",
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
    assert!(items_for(&history, Family::Responses, "gpt-6").is_ok());
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
