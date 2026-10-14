//! Codex WebSocket transport replay proofs.
//!
//! Real local [`ReplayServer`](replay_server::ReplayServer) WebSocket exchanges
//! driven through the actual `ProviderSet` / `Provider::open` path with a
//! seeded `auth.json` Codex OAuth credential. Each case covers one retained
//! obligation of change 11 of the provider layer behavior part that the
//! crate-level state-machine tests already pin with a hand-rolled server and a
//! rigged clock; these tests prove the same behavior over the shared replay
//! transport, asserting neutral event values and order, terminal errors,
//! cancellation, and consumed exchanges.
//!
//! Covered here: reuse then model-change rotation (`websocket-reuse`), close
//! after a delivered delta (`websocket-close-before-completion`),
//! connection-limit retry before delivery (`websocket-limit-reconnect`),
//! six refused handshakes then HTTPS with a model-change reattempt
//! (`websocket-https-fallback`), and cancellation with a waiting second turn
//! (`websocket-cancel-race`). The pure time-idle expiry half of reuse (idle
//! past five minutes, age past fifty-five) stays covered by the crate-level
//! test with the injected clock: [`WsSessions`](dal_provider#) exposes no
//! integration-reachable clock, and `std::time::Instant` does not follow a
//! paused Tokio clock, so no public-path proof can advance it without a new
//! seam (reported to the parent as a requested shared edit).
//!
//! The replay harness has no per-frame pacing seam: server frames arrive as
//! fast as loopback allows. The fifty cancel points are therefore
//! consumer-side observation counts from a fixed seed, varying task
//! interleaving rather than wire states; the no-contamination invariant is
//! identical at every point.

use super::replay_server;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use dal_core::{
    ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RequestParams, SessionId,
    ThinkingLevel,
};
use dal_provider::{
    AuthStyle, CatalogEntry, EnvSnapshot, EventStream, Listing, NoticeSink, Provider,
    ProviderConfig, ProviderEntry, ProviderError, ProviderIdentity, ProviderSet, ResolvedModel,
    StopReason, StreamEvent, ThinkingSupport, ToolSupport, Transport,
};
use tokio_util::sync::CancellationToken;

/// Fixed session carried by every proof turn, also as `prompt_cache_key`, so
/// client frames are byte-stable across runs.
const SESSION_ID: &str = "0196a3b1-2c4d-7e5f-9a6b-7c8d9e0f1a2b";
const MODEL_A: &str = "gpt-6-luna";
const MODEL_B: &str = "gpt-5.6-luna";
const CANCEL_SUITE_DEADLINE: Duration = Duration::from_secs(120);
const CANCEL_ROUND_DEADLINE: Duration = Duration::from_secs(30);

/// The neutral shape of one successful text turn.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Text,
    CallsDone,
    Usage,
    Stop,
    Other,
}

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: "test".into(),
        os: "linux".into(),
        os_version: "test".into(),
        arch: "x86_64".into(),
    }
}

fn entry(model: &str) -> CatalogEntry {
    CatalogEntry {
        provider: "openai-codex".into(),
        id: model.into(),
        display: model.into(),
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
    }
}

fn codex_request(model: &str, session: SessionId) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Codex,
            model: model.into(),
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
        cache_key: Some(session.to_string().into()),
    }
}

fn resolved(model: &str) -> ResolvedModel {
    ResolvedModel {
        provider: "openai-codex".into(),
        route: ModelRoute::Api {
            family: Family::Codex,
            model: model.into(),
        },
        entry: entry(model),
    }
}

fn notices() -> (NoticeSink, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let target = Arc::clone(&seen);
    let sink: NoticeSink = Arc::new(move |notice| {
        target.lock().expect("notices lock").push(notice);
    });
    (sink, seen)
}

/// Unpadded base64url for the seeded JWT only; the fixed header vector below
/// pins the encoder.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(char::from(ALPHABET[((word >> 18) & 63) as usize]));
        out.push(char::from(ALPHABET[((word >> 12) & 63) as usize]));
        if chunk.len() > 1 {
            out.push(char::from(ALPHABET[((word >> 6) & 63) as usize]));
        }
        if chunk.len() > 2 {
            out.push(char::from(ALPHABET[(word & 63) as usize]));
        }
    }
    out
}

/// An unsigned JWT naming `account`. Every credential value is generated at
/// runtime from the run stamp: nothing committed here could authenticate
/// anywhere, and the fixtures only match secrets as `<redacted>`.
fn id_token(account: &str) -> String {
    let header = base64url(br#"{"alg":"none"}"#);
    assert_eq!(header, "eyJhbGciOiJub25lIn0");
    let payload = base64url(
        format!(r#"{{"https://api.openai.com/auth":{{"chatgpt_account_id":"{account}"}}}}"#)
            .as_bytes(),
    );
    format!("{header}.{payload}.x")
}

fn auth_json(stamp: u128) -> String {
    let account = format!("acct-ws-test-{stamp}");
    let token = id_token(&account);
    format!(
        r#"{{"openai-codex":{{"kind":"oauth","access_token":"ws-test-access-{stamp}","refresh_token":"ws-test-refresh-{stamp}","expires_at":2000000000,"id_token":"{token}","account_id":"{account}"}}}}"#
    )
}

/// A provider set pointed at the replay server with a seeded Codex OAuth
/// credential. The far-future expiry keeps every turn clear of the refresh
/// path, so exchanges are consumed only by turn handshakes and HTTPS turns.
fn start_set(name: &str, base: &str) -> (ProviderSet, PathBuf) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("wall clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dalgona-ws-{name}-{stamp}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let auth_path = dir.join("auth.json");
    std::fs::write(&auth_path, auth_json(stamp)).expect("seed auth.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
            .expect("lock auth.json");
    }
    let config = ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries: 5,
        providers: vec![ProviderEntry {
            id: "openai-codex".into(),
            def: None,
            family: Family::Codex,
            base_url: base.into(),
            transport: Transport::Websocket,
            key_env: None,
            auth: AuthStyle::Bearer,
            max_concurrent_requests: 4,
        }],
        scripted: None,
    };
    let set = ProviderSet::new(&config, identity(), EnvSnapshot::test(&[]), &dir, &dir)
        .expect("provider set builds");
    (set, dir)
}

async fn open_turn(
    provider: &Provider,
    session: SessionId,
    request: &ModelRequest,
    notices: NoticeSink,
    cancel: &CancellationToken,
) -> EventStream {
    provider
        .open(session, request, &[], notices, cancel)
        .await
        .expect("turn opens")
}

/// Maps one neutral event to its kind, collecting text. The wildcard keeps
/// unexpected events visible as `Other` instead of failing to compile when
/// the event enum grows; the sequence assertion then fails loudly.
fn push_event(kinds: &mut Vec<Kind>, texts: &mut Vec<String>, event: StreamEvent) {
    match event {
        StreamEvent::TextDelta { text } => {
            kinds.push(Kind::Text);
            texts.push(text);
        }
        StreamEvent::ToolCallsDone { calls } => {
            assert!(calls.is_empty(), "expected no calls, got {calls:?}");
            kinds.push(Kind::CallsDone);
        }
        StreamEvent::Usage { .. } => kinds.push(Kind::Usage),
        StreamEvent::Stop { reason } => {
            assert!(matches!(reason, StopReason::EndTurn));
            kinds.push(Kind::Stop);
        }
        _ => kinds.push(Kind::Other),
    }
}

async fn drain(stream: EventStream) -> (Vec<Kind>, Vec<String>, Option<String>) {
    let mut stream = Box::pin(stream);
    let mut kinds = Vec::new();
    let mut texts = Vec::new();
    let mut terminal = None;
    while terminal.is_none() {
        match stream.next().await {
            Some(Ok(event)) => push_event(&mut kinds, &mut texts, event),
            Some(Err(error)) => terminal = Some(error.to_string()),
            None => break,
        }
    }
    (kinds, texts, terminal)
}

async fn expect_text_turn(stream: EventStream) -> Vec<String> {
    let (kinds, texts, terminal) = drain(stream).await;
    assert_eq!(terminal, None);
    assert_eq!(
        kinds,
        [Kind::Text, Kind::CallsDone, Kind::Usage, Kind::Stop]
    );
    texts
}

fn retry_notices(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    seen.lock()
        .expect("notices lock")
        .iter()
        .filter(|notice| notice.starts_with("Retrying in "))
        .cloned()
        .collect()
}

fn fallback_notices(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    seen.lock()
        .expect("notices lock")
        .iter()
        .filter(|notice| notice.starts_with("Falling back from WebSockets to HTTPS transport."))
        .cloned()
        .collect()
}

#[tokio::test]
async fn websocket_reuse_then_model_change_rotates() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let case = replay_server::fixture_dir("websocket", "reuse");
        let server = replay_server::ReplayServer::start(&case)
            .await
            .expect("replay server starts");
        let (set, dir) = start_set("reuse", &server.base_url());
        let session = SessionId::parse(SESSION_ID).expect("fixed session id");
        let (notices, seen) = notices();

        for model in [MODEL_A, MODEL_A] {
            let provider = set.provider(resolved(model)).expect("provider binds");
            let cancel = CancellationToken::new();
            let stream = open_turn(
                &provider,
                session,
                &codex_request(model, session),
                notices.clone(),
                &cancel,
            )
            .await;
            assert_eq!(expect_text_turn(stream).await, [String::from("x")]);
        }
        assert_eq!(server.consumed(), 1);

        let provider = set.provider(resolved(MODEL_B)).expect("provider binds");
        let cancel = CancellationToken::new();
        let stream = open_turn(
            &provider,
            session,
            &codex_request(MODEL_B, session),
            notices.clone(),
            &cancel,
        )
        .await;
        assert_eq!(expect_text_turn(stream).await, [String::from("x")]);
        assert_eq!(server.consumed(), 2);

        assert!(
            server.mismatches().is_empty(),
            "replay mismatches: {:?}",
            server.mismatches()
        );
        assert!(
            retry_notices(&seen).is_empty(),
            "unexpected retry notices: {:?}",
            retry_notices(&seen)
        );
        assert!(
            fallback_notices(&seen).is_empty(),
            "unexpected fallback notices: {:?}",
            fallback_notices(&seen)
        );
        server.finish().await.expect("replay consumed");
        std::fs::remove_dir_all(&dir).ok();
    })
    .await
    .expect("reuse proof finishes");
}

#[tokio::test]
async fn websocket_close_after_delta_is_terminal() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let case = replay_server::fixture_dir("websocket", "close_after_delta");
        let server = replay_server::ReplayServer::start(&case)
            .await
            .expect("replay server starts");
        let (set, dir) = start_set("close", &server.base_url());
        let session = SessionId::parse(SESSION_ID).expect("fixed session id");
        let (notices, _) = notices();
        let provider = set.provider(resolved(MODEL_A)).expect("provider binds");
        let cancel = CancellationToken::new();
        let stream = open_turn(
            &provider,
            session,
            &codex_request(MODEL_A, session),
            notices,
            &cancel,
        )
        .await;
        let mut stream = Box::pin(stream);
        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::TextDelta { text })) if text == "x"
        ));
        let error = stream
            .next()
            .await
            .expect("close arrives")
            .expect_err("close before terminal fails");
        assert!(matches!(error, ProviderError::WsClosed { .. }));
        assert_eq!(
            error.to_string(),
            "websocket closed by server before response.completed. (code 1011: busy)"
        );
        assert_eq!(server.consumed(), 1);
        assert!(
            server.mismatches().is_empty(),
            "replay mismatches: {:?}",
            server.mismatches()
        );
        server.finish().await.expect("replay consumed");
        std::fs::remove_dir_all(&dir).ok();
    })
    .await
    .expect("close proof finishes");
}

#[tokio::test]
async fn websocket_limit_error_retries_before_delivery() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let case = replay_server::fixture_dir("websocket", "limit_retry");
        let server = replay_server::ReplayServer::start(&case)
            .await
            .expect("replay server starts");
        let (set, dir) = start_set("limit", &server.base_url());
        let session = SessionId::parse(SESSION_ID).expect("fixed session id");
        let (notices, seen) = notices();
        let provider = set.provider(resolved(MODEL_A)).expect("provider binds");
        let cancel = CancellationToken::new();
        let stream = open_turn(
            &provider,
            session,
            &codex_request(MODEL_A, session),
            notices,
            &cancel,
        )
        .await;
        assert_eq!(expect_text_turn(stream).await, [String::from("x")]);
        assert_eq!(server.consumed(), 2);
        let retries = retry_notices(&seen);
        assert_eq!(retries.len(), 1);
        assert!(retries[0].ends_with("(attempt 1/5)."));
        assert!(
            fallback_notices(&seen).is_empty(),
            "unexpected fallback notices: {:?}",
            fallback_notices(&seen)
        );
        assert!(
            server.mismatches().is_empty(),
            "replay mismatches: {:?}",
            server.mismatches()
        );
        server.finish().await.expect("replay consumed");
        std::fs::remove_dir_all(&dir).ok();
    })
    .await
    .expect("limit proof finishes");
}

#[tokio::test]
async fn websocket_fallback_after_six_refused_handshakes() {
    tokio::time::timeout(Duration::from_secs(150), fallback_body())
        .await
        .expect("fallback proof finishes");
}

/// A connection whose user-agent is not the harness `dalgon/` agent is
/// refused as foreign before it can claim an exchange. Ambient loopback
/// probers of freshly bound ports (observed: `Go-http-client/1.1` sending
/// `GET /` during WebSocket backoff sleeps) must never advance the replay
/// cursor or fail a proof; the probe gets one 503, the server records the
/// drop as foreign telemetry, and no fixture exchange is claimed.
#[tokio::test]
async fn websocket_foreign_probe_never_claims_an_exchange() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let case = replay_server::fixture_dir("websocket", "https_fallback");
        let server = replay_server::ReplayServer::start(&case)
            .await
            .expect("replay server starts");
        let (status, body) = probe_foreign(&server).await;
        assert_eq!(
            status,
            503,
            "foreign probe answered {status}: {}",
            String::from_utf8_lossy(&body)
        );
        assert!(
            body.windows(14).any(|window| window == b"replay foreign"),
            "probe body does not name the foreign refusal: {:?}",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(server.consumed(), 0, "probe claimed an exchange");
        assert!(
            server.mismatches().is_empty(),
            "replay mismatches: {:?}",
            server.mismatches()
        );
        let foreign = server.foreign_requests();
        assert_eq!(foreign.len(), 1, "foreign notes: {foreign:?}");
        assert!(
            foreign[0].starts_with(
                "replay foreign: GET \"/\" (upgrade=false, agent=\"Go-http-client/1.1\"",
            ),
            "unexpected foreign note: {}",
            foreign[0]
        );
        let verdict = server.finish().await.expect_err("untouched fixture");
        assert!(
            verdict.to_string().contains("replay incomplete: 0 of 9"),
            "unexpected verdict: {verdict}"
        );
    })
    .await
    .expect("foreign probe proof finishes");
}

/// Sends one plain `GET /` with a non-harness user-agent, the exact shape
/// the ambient prober sends, and returns the status code and body bytes.
async fn probe_foreign(server: &replay_server::ReplayServer) -> (u16, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = server.addr();
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("probe connects");
    stream
        .write_all(
            format!("GET / HTTP/1.1\r\nhost: {addr}\r\nuser-agent: Go-http-client/1.1\r\nconnection: close\r\naccept-encoding: gzip\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("probe writes");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("probe reads the response");
    let head_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response carries a header terminator");
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("response carries a status code");
    (status, raw[head_end + 4..].to_vec())
}

/// Six refused upgrades latch the session fallback with the exact notice; the
/// turn continues over HTTPS, the next same-model turn goes straight to HTTPS,
/// and a model change tries WebSocket again. The five backoff sleeps between
/// the six attempts run on real time (about thirty-one seconds), because the
/// retry sleeper has no integration-reachable virtual control.
async fn fallback_body() {
    let case = replay_server::fixture_dir("websocket", "https_fallback");
    let server = replay_server::ReplayServer::start(&case)
        .await
        .expect("replay server starts");
    let (set, dir) = start_set("fallback", &server.base_url());
    let session = SessionId::parse(SESSION_ID).expect("fixed session id");
    let (notices, seen) = notices();

    let provider = set.provider(resolved(MODEL_A)).expect("provider binds");
    let cancel = CancellationToken::new();
    let stream = provider
        .open(session, &codex_request(MODEL_A, session), &[], notices.clone(), &cancel)
        .await
        .unwrap_or_else(|error| panic!(
            "fallback first turn: {error:?}; consumed={}; mismatches={:?}; retries={:?}; fallback={:?}",
            server.consumed(), server.mismatches(), retry_notices(&seen), fallback_notices(&seen)
        ));
    assert_eq!(expect_text_turn(stream).await, [String::from("x")]);
    assert_eq!(server.consumed(), 7);
    let retries = retry_notices(&seen);
    assert_eq!(retries.len(), 5);
    for attempt in 1..=5 {
        assert!(
            retries
                .iter()
                .any(|notice| notice.ends_with(&format!("(attempt {attempt}/5)."))),
            "missing retry notice for attempt {attempt}"
        );
    }
    assert_eq!(
        fallback_notices(&seen),
        [
            "Falling back from WebSockets to HTTPS transport. codex error 503: WebSocket handshake failed"
        ]
    );

    let provider = set.provider(resolved(MODEL_A)).expect("provider binds");
    let cancel = CancellationToken::new();
    let stream = provider
        .open(session, &codex_request(MODEL_A, session), &[], notices.clone(), &cancel)
        .await
        .unwrap_or_else(|error| panic!(
            "fallback latched turn: {error:?}; consumed={}; mismatches={:?}; retries={:?}; fallback={:?}",
            server.consumed(), server.mismatches(), retry_notices(&seen), fallback_notices(&seen)
        ));
    assert_eq!(expect_text_turn(stream).await, [String::from("x")]);
    assert_eq!(server.consumed(), 8);
    assert_eq!(retry_notices(&seen).len(), 5);
    assert_eq!(fallback_notices(&seen).len(), 1);

    let provider = set.provider(resolved(MODEL_B)).expect("provider binds");
    let cancel = CancellationToken::new();
    let stream = provider
        .open(session, &codex_request(MODEL_B, session), &[], notices, &cancel)
        .await
        .unwrap_or_else(|error| panic!(
            "fallback model-change turn: {error:?}; consumed={}; mismatches={:?}; retries={:?}; fallback={:?}",
            server.consumed(), server.mismatches(), retry_notices(&seen), fallback_notices(&seen)
        ));
    assert_eq!(expect_text_turn(stream).await, [String::from("x")]);
    assert_eq!(server.consumed(), 9);
    assert_eq!(retry_notices(&seen).len(), 5);
    assert_eq!(fallback_notices(&seen).len(), 1);

    assert!(
        server.mismatches().is_empty(),
        "replay mismatches: {:?}",
        server.mismatches()
    );
    for foreign in server.foreign_requests() {
        println!("replay foreign connection dropped: {foreign}");
    }
    server.finish().await.expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

/// Fifty fixed-seed observation counts in `1..=60` from a splitmix64 stream.
/// The seed and algorithm pin the sequence; no assertion depends on wall time.
fn cancel_points() -> Vec<usize> {
    const SEED: u64 = 0x9E37_79B9_7F4A_7C15;
    const FRAMES: usize = 60;
    const ROUNDS: usize = 50;
    let mut state = SEED;
    let mut order: Vec<usize> = (1..=FRAMES).collect();
    let mut remaining = order.len();
    while remaining > 1 {
        state = state.wrapping_add(SEED);
        let mut draw = state;
        draw = (draw ^ (draw >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        draw = (draw ^ (draw >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        draw ^= draw >> 31;
        let span = u64::try_from(remaining).expect("sixty points fit");
        let pick = usize::try_from(draw % span).expect("pick lands inside");
        remaining -= 1;
        order.swap(remaining, pick);
    }
    order.truncate(ROUNDS);
    order
}

/// One cancel point: the first turn holds the session mutex (proven by its
/// first delivered event, which releases the waiting second turn), is cancelled
/// after `point` observed deltas, and must then yield the transport cancellation
/// error. The second turn must then deliver exactly its own events on a new
/// connection, which `consumed() == 2` proves since a reused dirty socket would
/// claim no new exchange.
async fn cancel_round(point: usize, round: usize) {
    let case = replay_server::fixture_dir("websocket", "cancel_race");
    let server = tokio::time::timeout(
        CANCEL_ROUND_DEADLINE,
        replay_server::ReplayServer::start(&case),
    )
    .await
    .expect("replay server starts before round deadline")
    .expect("replay server starts");
    let (set, dir) = start_set(&format!("cancel-{round}"), &server.base_url());
    let session = SessionId::parse(SESSION_ID).expect("fixed session id");
    let provider = set.provider(resolved(MODEL_A)).expect("provider binds");
    let (notices, _) = notices();
    let cancel = CancellationToken::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let mut started_tx = Some(started_tx);
    let first_req = codex_request(MODEL_A, session);
    let second_req = codex_request(MODEL_A, session);

    let first = async {
        let mut stream =
            Box::pin(open_turn(&provider, session, &first_req, notices.clone(), &cancel).await);
        let mut texts = Vec::new();
        let mut terminal = None;
        let mut released = false;
        while terminal.is_none() {
            match stream.next().await {
                Some(Ok(StreamEvent::TextDelta { text })) => texts.push(text),
                Some(Ok(_)) => panic!("cancelled turn emits text only"),
                Some(Err(error)) => terminal = Some(error.to_string()),
                None => panic!("cancelled turn ends in a cancel error"),
            }
            if !released && !texts.is_empty() {
                released = true;
                if let Some(sender) = started_tx.take() {
                    sender.send(()).ok();
                }
            }
            if texts.len() == point {
                cancel.cancel();
            }
        }
        (texts, terminal.expect("cancel error arrives"))
    };
    let second = async {
        started_rx.await.expect("first turn holds the session");
        expect_text_turn(
            open_turn(
                &provider,
                session,
                &second_req,
                notices.clone(),
                &CancellationToken::new(),
            )
            .await,
        )
        .await
    };
    let ((first_texts, first_error), second_texts) =
        tokio::time::timeout(CANCEL_ROUND_DEADLINE, async { tokio::join!(first, second) })
            .await
            .expect("round finishes promptly");

    assert!(first_texts.len() >= point);
    for (index, text) in first_texts.iter().enumerate() {
        let expected = format!("f{:02}", index + 1);
        assert_eq!(text, &expected);
    }
    assert_eq!(first_error, "codex request failed: request cancelled");
    assert_eq!(second_texts, [String::from("x")]);
    assert_eq!(server.consumed(), 2);
    assert!(
        server.mismatches().is_empty(),
        "replay mismatches: {:?}",
        server.mismatches()
    );
    tokio::time::timeout(CANCEL_ROUND_DEADLINE, server.finish())
        .await
        .expect("replay server finishes before round deadline")
        .expect("replay consumed");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn websocket_cancel_race_keeps_second_turn_clean() {
    tokio::time::timeout(CANCEL_SUITE_DEADLINE, async {
        for (round, point) in cancel_points().into_iter().enumerate() {
            cancel_round(point, round).await;
        }
    })
    .await
    .expect("cancellation replay suite finishes before its deadline");
}
