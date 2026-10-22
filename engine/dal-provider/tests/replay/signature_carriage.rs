//! Signature-carriage integration suite: thinking-signature `replay` payloads
//! travel from a scripted JSONL fixture through the public provider surface
//! (`ProviderSet` with `[providers.scripted]` → `Provider::open` →
//! `EventStream`) with byte-exact items, in stream order, ahead of the
//! terminal.
//!
//! The decoder-level emission proofs live in the family unit tests
//! (`family/pi_messages/tests.rs::thinking_end_with_signature_emits_replay`
//! and the anthropic signed-thinking suites); the entry/collect codec proof
//! lives in `src/scripted/tests.rs::replay_event_carries_a_thinking_signature_verbatim_from_jsonl`.
//! These cases prove the next hop on the exact path a session opens, for two
//! signature spellings (Anthropic `signature`, pi-messages
//! `thinkingSignature`). Every item assertion is an exact byte equality, so
//! any re-encoding, truncation, or field drop on the path fails.

#![expect(clippy::expect_used, reason = "integration tests fail loudly")]

use std::sync::Arc;

use dal_core::{
    ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RequestParams, ThinkingLevel,
};
use dal_provider::{
    EnvSnapshot, EventStream, NoticeSink, ProviderConfig, ProviderIdentity, ProviderSet,
    ScriptedSelection, StopReason, StreamEvent,
};

use tokio_util::sync::CancellationToken;

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: "test".into(),
        os: "linux".into(),
        os_version: "test".into(),
        arch: "x86_64".into(),
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-6".into(),
        },
        system: Arc::from("Be brief."),
        tools: Arc::from([]),
        context: Arc::from([ContextItem::User {
            parts: vec![Part::Text { text: "Hi".into() }],
        }]),
        params: RequestParams {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

fn usage_tail() -> &'static str {
    r#"{"type":"usage","usage":{"input_tokens":25,"cached_input_tokens":0,"output_tokens":12,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#
}

/// Opens one scripted turn from `fixture` through the public provider surface
/// and drains it, exactly as a session does.
async fn open_scripted(fixture: &str) -> (Vec<StreamEvent>, ProviderSet, std::path::PathBuf) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dalgona-replay-signature-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("script.jsonl");
    std::fs::write(&path, fixture).expect("fixture writes");
    let config = ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Off,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries: 0,
        providers: Vec::new(),
        scripted: Some(ScriptedSelection {
            fixture: path.to_string_lossy().into_owned().into_boxed_str(),
        }),
    };
    let set = ProviderSet::new(&config, identity(), EnvSnapshot::test(&[]), &dir, &dir)
        .expect("provider set builds");
    let provider = set
        .scripted_provider()
        .expect("the scripted provider is configured");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let mut stream: EventStream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("scripted turn opens");
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("no stream error"));
    }
    (events, set, dir)
}

fn replay_item(events: &[StreamEvent]) -> &dal_provider::ReplayPayload {
    let replay = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Replay { payload } => Some(payload),
            _ => None,
        })
        .expect("the turn carries a signature replay");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::Replay { .. }))
            .count(),
        1,
        "exactly one signature replay delivers: {events:?}"
    );
    replay
}

#[tokio::test]
async fn scripted_open_delivers_an_anthropic_thinking_signature_verbatim() {
    const ITEM: &str =
        r#"{"type":"thinking","thinking":"deep thought","signature":"EqQBCgIYAhIM-synth-01"}"#;
    let tail = usage_tail();
    let fixture = format!(
        "{head}{tail}\n",
        head = concat!(
            r#"{"kind":"events","events":["#,
            r#"{"type":"reasoning_delta","text":"deep "},"#,
            r#"{"type":"reasoning_delta","text":"thought"},"#,
            r#"{"type":"replay","payload":{"family":"anthropic","model":"claude-test-1","item":{"type":"thinking","thinking":"deep thought","signature":"EqQBCgIYAhIM-synth-01"}}},"#,
            r#"{"type":"text_delta","text":"The answer is 21."},"#,
            r#"{"type":"tool_calls_done","calls":[]},"#,
        ),
    );
    let (events, _set, dir) = open_scripted(&fixture).await;
    assert_eq!(
        events.len(),
        7,
        "the full turn delivers in order: {events:?}"
    );
    assert!(
        matches!(&events[0], StreamEvent::ReasoningDelta { text } if text == "deep "),
        "reasoning leads: {events:?}"
    );
    let payload = replay_item(&events);
    assert_eq!(payload.family, Family::Anthropic);
    assert_eq!(payload.model.as_ref(), "claude-test-1");
    assert_eq!(
        payload.item.as_str(),
        ITEM,
        "the signature item was re-encoded: {}",
        payload.item.as_str()
    );
    assert!(
        matches!(&events[3], StreamEvent::TextDelta { text } if text == "The answer is 21."),
        "text follows the signature: {events:?}"
    );
    assert!(matches!(events[4], StreamEvent::ToolCallsDone { .. }));
    assert!(matches!(events[5], StreamEvent::Usage { .. }));
    assert!(matches!(
        events[6],
        StreamEvent::Stop {
            reason: StopReason::EndTurn
        }
    ));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn scripted_open_delivers_a_pi_messages_thinking_signature_verbatim() {
    const ITEM: &str = r#"{"thinkingSignature":"sig-pi-synth-02","redacted":false}"#;
    let tail = usage_tail();
    let fixture = format!(
        "{head}{tail}\n",
        head = concat!(
            r#"{"kind":"events","events":["#,
            r#"{"type":"reasoning_delta","text":"pondering"},"#,
            r#"{"type":"replay","payload":{"family":"pi-messages","model":"balanced","item":{"thinkingSignature":"sig-pi-synth-02","redacted":false}}},"#,
            r#"{"type":"tool_calls_done","calls":[]},"#,
        ),
    );
    let (events, _set, dir) = open_scripted(&fixture).await;
    assert_eq!(
        events.len(),
        5,
        "the full turn delivers in order: {events:?}"
    );
    assert!(
        matches!(&events[0], StreamEvent::ReasoningDelta { text } if text == "pondering"),
        "reasoning leads: {events:?}"
    );
    let payload = replay_item(&events);
    assert_eq!(payload.family, Family::PiMessages);
    assert_eq!(payload.model.as_ref(), "balanced");
    assert_eq!(
        payload.item.as_str(),
        ITEM,
        "the signature item was re-encoded: {}",
        payload.item.as_str()
    );
    assert!(matches!(events[2], StreamEvent::ToolCallsDone { .. }));
    assert!(matches!(events[3], StreamEvent::Usage { .. }));
    assert!(matches!(
        events[4],
        StreamEvent::Stop {
            reason: StopReason::EndTurn
        }
    ));
    std::fs::remove_dir_all(&dir).ok();
}
