#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::panic, reason = "integration tests fail loudly")]
#![expect(
    clippy::disallowed_methods,
    reason = "replay tests drive loopback servers on background tasks"
)]
//! Provider replay integration suite.

#[path = "replay/anthropic.rs"]
mod anthropic;
#[path = "replay/azure.rs"]
mod azure;
#[path = "replay/chat.rs"]
mod chat;
#[path = "replay/codex.rs"]
mod codex;
#[path = "replay/copilot.rs"]
mod copilot;
#[path = "replay/google.rs"]
mod google;
#[path = "support/replay_server.rs"]
mod replay_server;
#[path = "replay/responses.rs"]
mod responses;
#[path = "replay/signature_carriage.rs"]
mod signature_carriage;
#[path = "replay/websocket.rs"]
mod websocket;

use std::sync::Arc;

use dal_core::{
    Config, ConfigProduct, ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose,
    RequestParams, ThinkingLevel,
};
use dal_provider::{
    CatalogEntry, EnvSnapshot, Listing, NoticeSink, ProviderConfig, ProviderIdentity, ProviderSet,
    ResolvedModel, StopReason, StreamEvent, ThinkingSupport, ToolSupport,
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

fn entry() -> CatalogEntry {
    CatalogEntry {
        provider: "chat-test".into(),
        id: "gpt-6-astra".into(),
        display: "gpt-6-astra".into(),
        listing: Listing::Listed,
        context_window: Some(32_000),
        max_output: Some(4_096),
        thinking: ThinkingSupport::OpenAi {
            accepted: Vec::new(),
            none_supported: true,
        },
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        tool_support: ToolSupport::Any,
        temperature_allowed: false,
        display_supported: false,
        custom_grammar: false,
        premium_multiplier: None,
        price: None,
    }
}

fn chat_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-6-astra".into(),
            provider: None,
        },
        system: Arc::from("You are terse."),
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

#[tokio::test]
async fn chat_text_turn() {
    let case = replay_server::fixture_dir("chat", "text_turn");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let base = server.base_url();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dalgona-replay-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let table = format!(
        "[providers.chat-test]\napi = \"openai_chat\"\nbase_url = \"{base}\"\nkey_env = \"CHAT_TEST_KEY\"\nauth = \"bearer\"\ntransport = \"https\"\nmax_concurrent_requests = 4\n"
    );
    let core =
        Config::load(ConfigProduct::Dalgon, &dir, "", Some(&table)).expect("provider table parses");
    let mut config = ProviderConfig::from_config(&core).expect("providers decode");
    config.request_max_retries = 0;
    config.stream_max_retries = 0;
    config
        .providers
        .retain(|provider| &*provider.id == "chat-test");
    let set = ProviderSet::new(
        &config,
        identity(),
        EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
        &dir,
        &dir,
    )
    .expect("provider set builds");
    let resolved = ResolvedModel {
        provider: "chat-test".into(),
        route: ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-6-astra".into(),
            provider: None,
        },
        entry: entry(),
    };
    let provider = set.provider(resolved).expect("provider binds");
    let notices: NoticeSink = Arc::new(|_| {});
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            dal_core::SessionId::new_v7(),
            &chat_request(),
            &[],
            notices,
            &cancel,
        )
        .await
        .expect("chat turn opens");
    let mut texts = String::new();
    let mut saw_done = false;
    let mut saw_usage = false;
    let mut stop = None;
    let mut events = std::pin::pin!(stream);
    while let Some(event) = events.next().await {
        match event.expect("no stream error") {
            StreamEvent::TextDelta { text } => texts.push_str(&text),
            StreamEvent::ToolCallsDone { calls } => {
                assert_eq!(calls, [] as [dal_provider::ToolCall; 0]);
                saw_done = true;
            }
            StreamEvent::Usage { usage } => {
                assert_eq!(usage.input_tokens, 19);
                saw_usage = true;
            }
            StreamEvent::Stop { reason } => stop = Some(reason),
            _ => {}
        }
    }
    assert_eq!(texts, "Hello");
    assert!(saw_done && saw_usage);
    assert!(matches!(stop, Some(StopReason::EndTurn)));
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert_eq!(server.mismatches(), [] as [std::string::String; 0]);
    assert_eq!(server.consumed(), 1);
    assert_eq!(server.open_requests(), 0);
    assert!(server.max_open_requests() >= 1);
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

/// Sends one HTTP/1.1 request with `connection: close` and returns the status
/// code and the answer text.
async fn send_raw(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, String) {
    use std::fmt::Write as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nhost: {addr}\r\nuser-agent: dalgon/test\r\nconnection: close\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        write!(head, "{name}: {value}\r\n").expect("formatting into a String");
    }
    head.push_str("\r\n");
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("client connects");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("client writes the head");
    stream
        .write_all(body)
        .await
        .expect("client writes the body");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("client reads the answer");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("answer carries a status line");
    (status, text)
}

/// The synthetic key the drift cases send; no refusal text may carry it.
const REPLAY_KEY: &str = "synthetic-replay-key-0001";

/// The headers of the faithful `request_snapshot` request.
const FAITHFUL: [(&str, &str); 4] = [
    ("content-type", "application/json"),
    ("accept", "text/event-stream"),
    ("anthropic-version", "2023-06-01"),
    ("x-api-key", REPLAY_KEY),
];

/// A request that differs from the faithful one in exactly one compared field.
struct Drift {
    label: &'static str,
    method: &'static str,
    path: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    body: Vec<u8>,
    /// The field the server must name in its refusal.
    field: &'static str,
}

impl Drift {
    fn faithful(label: &'static str, field: &'static str, body: &[u8]) -> Self {
        Self {
            label,
            method: "POST",
            path: "/v1/messages",
            headers: FAITHFUL.to_vec(),
            body: body.to_vec(),
            field,
        }
    }

    /// The faithful headers with `name` removed, or replaced when `value` is given.
    fn header(mut self, name: &'static str, value: Option<&'static str>) -> Self {
        self.headers.retain(|(header, _)| *header != name);
        self.headers.extend(value.map(|value| (name, value)));
        self
    }
}

/// One drift per compared field, with the body drifts at N-1, N, and N+1 bytes
/// around the faithful length.
fn drifts(body: &[u8]) -> Vec<Drift> {
    let mut flipped = body.to_vec();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    let mut longer = body.to_vec();
    longer.push(b' ');
    let shorter = &body[..body.len() - 1];
    let mut one_byte_changed = Drift::faithful("one body byte", "body", body);
    one_byte_changed.body = flipped;
    let mut method = Drift::faithful("method", "method", body);
    method.method = "GET";
    let mut query = Drift::faithful("query", "path", body);
    query.path = "/v1/messages?beta=1";
    let mut path_case = Drift::faithful("path case", "path", body);
    path_case.path = "/V1/messages";
    vec![
        one_byte_changed,
        Drift::faithful("one byte short", "body", shorter),
        Drift::faithful("one byte long", "body", &longer),
        method,
        query,
        path_case,
        Drift::faithful("missing key header", "header x-api-key", body).header("x-api-key", None),
        Drift::faithful("wrong version header", "header anthropic-version", body)
            .header("anthropic-version", Some("2023-06-02")),
        Drift::faithful("wrong content type", "header content-type", body)
            .header("content-type", Some("text/plain")),
    ]
}

/// Sends `drift` to a fresh server and names every way the server let it down.
async fn drift_failures(case: &std::path::Path, drift: &Drift) -> Vec<String> {
    let label = drift.label;
    let named = format!("replay mismatch: 0: {}:", drift.field);
    let server = replay_server::ReplayServer::start(case)
        .await
        .expect("replay server starts");
    let (status, text) = send_raw(
        server.addr(),
        drift.method,
        drift.path,
        &drift.headers,
        &drift.body,
    )
    .await;
    let mut failures = Vec::new();
    if status != 418 {
        failures.push(format!("{label}: answered {status}, not 418"));
    }
    if !text.contains(&named) {
        failures.push(format!(
            "{label}: the refusal does not name {}",
            drift.field
        ));
    }
    if text.contains(REPLAY_KEY) {
        failures.push(format!("{label}: the refusal leaks the key"));
    }
    let recorded = server.mismatches();
    if recorded.len() != 1 || !recorded[0].starts_with(&named) {
        failures.push(format!("{label}: recorded {recorded:?}"));
    }
    match server.finish().await {
        Ok(()) => failures.push(format!("{label}: the verdict passed")),
        Err(verdict) if !verdict.to_string().contains("replay mismatch") => {
            failures.push(format!("{label}: the verdict is {verdict}"));
        }
        Err(_) => {}
    }
    failures
}

/// Every replay case ends by asking the server whether anything drifted, so
/// the verdict machinery is the oracle of the whole suite: a drift in any one
/// compared field must answer 418, be recorded, and fail `finish`, while the
/// faithful request is accepted and a request past the last exchange is not.
#[tokio::test]
async fn drift_in_each_compared_field_is_refused_recorded_and_fails_the_verdict() {
    let case = replay_server::fixture_dir("anthropic", "request_snapshot");
    let body = std::fs::read(case.join("request.json")).expect("fixture body reads");

    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let send_faithful = || send_raw(server.addr(), "POST", "/v1/messages", &FAITHFUL, &body);
    let (status, _) = send_faithful().await;
    assert_eq!(status, 200, "the faithful request is accepted");
    assert_eq!(server.mismatches(), [] as [String; 0]);
    let (status, text) = send_faithful().await;
    assert_eq!(status, 418, "a request past the last exchange is refused");
    assert!(text.contains("replay mismatch: 1: exchange"), "{text}");
    assert!(
        server.finish().await.is_err(),
        "an overrun fails the verdict"
    );

    // Every drift is judged before the test fails, so one failure report
    // names every compared field the server let through.
    let mut failures = Vec::new();
    for drift in drifts(&body) {
        failures.extend(drift_failures(&case, &drift).await);
    }
    assert!(failures.is_empty(), "drift let through: {failures:#?}");
}

#[tokio::test]
async fn reserved_port_refuses_until_listen() {
    let case = replay_server::fixture_dir("chat", "text_turn");
    let reserved = replay_server::ReplayServer::reserve(&case).expect("reserve binds");
    assert_eq!(reserved.base_url(), format!("http://{}", reserved.addr()));
    assert_eq!(reserved.addr().ip().to_string(), "127.0.0.1");
    let refused = tokio::net::TcpStream::connect(reserved.addr()).await;
    assert!(refused.is_err(), "bound reservation refuses connects");
    let server = reserved.listen().expect("reservation listens");
    let error = server.finish().await.expect_err("untouched fixture");
    assert!(
        error
            .to_string()
            .contains("replay incomplete: 0 of 1 exchanges consumed"),
        "unexpected verdict: {error}"
    );
}
