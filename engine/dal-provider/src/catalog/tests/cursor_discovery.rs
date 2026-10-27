//! Cursor catalog discovery: the two unary model-list RPCs joined against a
//! scripted loopback server that answers exact protobuf wire frames and
//! records every request.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use super::*;
use crate::{
    auth::credential::Credential,
    catalog::{
        cursor::{self, aiserver, decode_cursor_models},
        fetch::fetch_cursor_under,
    },
    family::cursor::proto::{ModelDetails, ThinkingDetails},
};

/// One request the scripted server answered.
struct Captured {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }
}

/// What the script saw: every request it answered.
struct Seen {
    requests: Vec<Captured>,
}

struct Script {
    port: u16,
    handle: thread::JoinHandle<Seen>,
}

impl Script {
    fn finish(self) -> Seen {
        self.handle.join().expect("the scripted server completes")
    }
}

fn read_request(stream: &mut TcpStream) -> Captured {
    stream
        .set_nonblocking(false)
        .expect("make the replay socket blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set the replay socket read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("clone the replay socket"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("read the request line");
    let mut parts = line.split_whitespace();
    let method = parts.next().expect("a method").to_owned();
    let path = parts.next().expect("a path").to_owned();
    let mut headers = Vec::new();
    let mut length = 0_usize;
    loop {
        let mut header_line = String::new();
        let bytes = reader
            .read_line(&mut header_line)
            .expect("read a request header");
        if bytes == 0 || header_line == "\r\n" {
            break;
        }
        let (name, value) = header_line.split_once(':').expect("a header has a colon");
        let name = name.trim().to_ascii_lowercase();
        if name == "content-length" {
            length = value.trim().parse().expect("a content length");
        }
        headers.push((name, value.trim().to_owned()));
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).expect("read the request body");
    Captured {
        method,
        path,
        headers,
        body,
    }
}

/// Answers exactly `expected` requests on one loopback port, each on its own
/// thread, every reply a `content-type: application/proto` binary body.
fn serve(
    expected: usize,
    route: impl Fn(&Captured) -> (u16, Vec<u8>) + Send + Sync + 'static,
) -> Script {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the scripted listener");
    listener
        .set_nonblocking(true)
        .expect("make the scripted listener nonblocking");
    let port = listener
        .local_addr()
        .expect("read the listener port")
        .port();
    let route = Arc::new(route);
    let log = Arc::new(Mutex::new(Vec::new()));
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut workers = Vec::new();
        while workers.len() < expected {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let route = Arc::clone(&route);
                    let log = Arc::clone(&log);
                    workers.push(thread::spawn(move || {
                        let request = read_request(&mut stream);
                        let (status, body) = route(&request);
                        let reply = format!(
                            "HTTP/1.1 {status} Scripted\r\ncontent-type: \
                             application/proto\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        );
                        // A cancelled client is gone before the scripted
                        // answer; the write failure is the point.
                        let _ = stream.write_all(reply.as_bytes());
                        let _ = stream.write_all(&body);
                        log.lock().expect("lock the request log").push(request);
                    }));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "the scripted server saw {} of {expected} requests",
                        workers.len()
                    );
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
        for worker in workers {
            worker.join().expect("answer one scripted request");
        }
        Seen {
            requests: std::mem::take(&mut *log.lock().expect("lock the request log")),
        }
    });
    Script { port, handle }
}

/// The whole join scripted per RPC path.
fn join_route(
    usable: (u16, Vec<u8>),
    available: (u16, Vec<u8>),
) -> impl Fn(&Captured) -> (u16, Vec<u8>) + Send + Sync + 'static {
    move |request| scripted_join(request, &usable, &available, None)
}

/// [`join_route`] with both model lists held before answering.
fn hanging_join_route(
    usable: (u16, Vec<u8>),
    available: (u16, Vec<u8>),
    hang: Duration,
) -> impl Fn(&Captured) -> (u16, Vec<u8>) + Send + Sync + 'static {
    move |request| scripted_join(request, &usable, &available, Some(hang))
}

fn scripted_join(
    request: &Captured,
    usable: &(u16, Vec<u8>),
    available: &(u16, Vec<u8>),
    hang: Option<Duration>,
) -> (u16, Vec<u8>) {
    if request.method != "POST" {
        return (405, Vec::new());
    }
    let answer = if request.path == cursor::USABLE_PATH {
        usable.clone()
    } else if request.path == cursor::AVAILABLE_PATH {
        available.clone()
    } else {
        (404, Vec::new())
    };
    // The model lists hang; the client-side deadline is what must end them.
    if let Some(hang) = hang
        && (request.path == cursor::USABLE_PATH || request.path == cursor::AVAILABLE_PATH)
    {
        thread::sleep(hang);
    }
    answer
}

/// One Connect unary frame carrying `body`.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(0_u8);
    let length = u32::try_from(body.len()).expect("a frame length");
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// One end-of-stream frame carrying a JSON trailer.
fn end_stream(trailer: &str) -> Vec<u8> {
    let mut out = vec![0x02_u8];
    let length = u32::try_from(trailer.len()).expect("a trailer length");
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(trailer.as_bytes());
    out
}

async fn refresh(
    provider: &ProviderEntry,
    credential: &Credential,
    cache: &std::path::Path,
) -> CatalogFetch {
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider,
        credential,
        cache_dir: cache,
        user_agent: "dalgon/test",
        version: "test",
        env: empty_env(),
    };
    load_models(&fetch, |_| std::future::pending::<()>()).await
}

fn cursor_provider(port: u16) -> ProviderEntry {
    provider(
        "cursor",
        Family::Cursor,
        &format!("http://127.0.0.1:{port}"),
    )
}

fn api_key(token: &str) -> Credential {
    Credential::ApiKey {
        key: SecretString::from(String::from(token)),
    }
}

fn oauth(token: &str) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(String::from(token)),
        refresh_token: SecretString::from(String::from("refresh")),
        expires_at: None,
        id_token: None,
        account_id: None,
        project_id: None,
    })
}

fn legacy_details(id: &str) -> ModelDetails {
    ModelDetails {
        model_id: String::from(id),
        display_model_id: String::new(),
        display_name: String::new(),
        display_name_short: String::new(),
        aliases: Vec::new(),
        thinking_details: None,
        max_mode: None,
    }
}

fn usable_response(models: &[ModelDetails]) -> Vec<u8> {
    cursor::GetUsableModelsResponse {
        models: models.to_vec(),
    }
    .encode_to_vec()
}

fn rich(name: &str) -> aiserver::AvailableModelsModelDetails {
    aiserver::AvailableModelsModelDetails {
        name: String::from(name),
        is_chat_only: None,
        supports_agent: None,
        supports_images: None,
        context_token_limit: None,
        context_token_limit_for_max_mode: None,
        client_display_name: None,
        server_model_name: None,
        parameter_definitions: Vec::new(),
        variants: Vec::new(),
        legacy_slugs: Vec::new(),
        id_aliases: Vec::new(),
        requires_data_retention: None,
    }
}

fn variant(legacy_slug: &str) -> aiserver::ModelVariantConfig {
    aiserver::ModelVariantConfig {
        parameter_values: Vec::new(),
        display_name: None,
        is_max_mode: None,
        variant_string_representation: None,
        legacy_slug: Some(String::from(legacy_slug)),
    }
}

fn parameter(id: &str, value: &str) -> aiserver::ModelParameterValue {
    aiserver::ModelParameterValue {
        id: String::from(id),
        value: String::from(value),
    }
}

fn rich_response(models: &[aiserver::AvailableModelsModelDetails]) -> Vec<u8> {
    aiserver::AvailableModelsResponse {
        models: models.to_vec(),
    }
    .encode_to_vec()
}

fn row_of<'a>(fetched: &'a CatalogFetch, id: &str) -> &'a CatalogEntry {
    fetched
        .entries
        .iter()
        .find(|entry| entry.id.as_ref() == id)
        .unwrap_or_else(|| panic!("no row for {id}"))
}

fn ids_of(fetched: &CatalogFetch) -> Vec<String> {
    fetched
        .entries
        .iter()
        .map(|entry| entry.id.to_string())
        .collect()
}

fn ladder(entry: &CatalogEntry) -> (Vec<dal_core::ThinkingLevel>, bool) {
    match &entry.thinking {
        ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        } => (accepted.clone(), *none_supported),
        other => panic!("{} carries {other:?}", entry.id),
    }
}

const TOKEN: &str = "cursor-access-token";

#[tokio::test]
async fn usable_rows_publish_account_slugs_without_invented_limits() {
    let directory = TestDir::new();
    let mut opus = legacy_details("claude-opus-4-8-high");
    opus.display_name = String::from("Claude Opus 4.8 High");
    let mut gpt = legacy_details("gpt-5.5");
    gpt.thinking_details = Some(ThinkingDetails {});
    let script = serve(
        2,
        join_route(
            (200, frame(&usable_response(&[opus, gpt]))),
            (500, Vec::new()),
        ),
    );
    let provider = cursor_provider(script.port);
    let fetched = refresh(&provider, &api_key(TOKEN), directory.path()).await;
    let seen = script.finish();

    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(ids_of(&fetched), ["claude-opus-4-8-high", "gpt-5.5"]);
    let opus_row = row_of(&fetched, "claude-opus-4-8-high");
    assert_eq!(opus_row.display.as_ref(), "Claude Opus 4.8 High");
    // GetUsableModels carries no limit: none is invented.
    assert_eq!(opus_row.context_window, None);
    assert_eq!(opus_row.max_output, None);
    assert_eq!(ladder(opus_row), (Vec::new(), true));
    assert_eq!(row_of(&fetched, "gpt-5.5").display.as_ref(), "gpt-5.5");

    // Exactly one request per pinned path, each a POST with the pinned
    // client identity over the bearer credential.
    assert_eq!(seen.requests.len(), 2);
    for path in [cursor::USABLE_PATH, cursor::AVAILABLE_PATH] {
        let request = seen
            .requests
            .iter()
            .find(|request| request.path == path)
            .unwrap_or_else(|| panic!("no request for {path}"));
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.header("authorization"),
            Some("Bearer cursor-access-token")
        );
        assert_eq!(request.header("content-type"), Some("application/proto"));
        assert_eq!(request.header("x-cursor-client-type"), Some("cli"));
        assert_eq!(
            request.header("x-cursor-client-version"),
            Some("cli-2026.09.02-c22c1a3")
        );
        assert_eq!(request.header("x-ghost-mode"), Some("true"));
        assert_eq!(request.header("connect-protocol-version"), Some("1"));
    }
    // The requests carry the pinned shapes.
    let usable = seen
        .requests
        .iter()
        .find(|request| request.path == cursor::USABLE_PATH)
        .expect("the usable request");
    let decoded =
        cursor::GetUsableModelsRequest::decode(usable.body.as_slice()).expect("usable request");
    assert_eq!(decoded.custom_model_ids, [] as [String; 0]);
    let available = seen
        .requests
        .iter()
        .find(|request| request.path == cursor::AVAILABLE_PATH)
        .expect("the available request");
    let decoded = aiserver::AvailableModelsRequest::decode(available.body.as_slice())
        .expect("available request");
    assert!(decoded.include_long_context_models);
    assert_eq!(decoded.use_model_parameters, Some(true));
    assert_eq!(decoded.do_not_use_markdown, Some(true));
    assert_eq!(decoded.use_cloud_agent_effort_modes, Some(true));
}

#[tokio::test]
async fn rich_variants_publish_wire_named_slugs_with_wire_limits() {
    let directory = TestDir::new();
    let mut claude = rich("claude-opus-4-8");
    claude.client_display_name = Some(String::from("Claude Opus 4.8"));
    claude.supports_images = Some(true);
    let mut high = variant("claude-opus-4-8-high");
    high.parameter_values = vec![parameter("context", "2m")];
    claude.variants = vec![high, variant("claude-opus-4-8-low")];
    let mut gpt = rich("gpt-5-5");
    gpt.context_token_limit = Some(400_000);
    gpt.variants = vec![variant("gpt-5-5-medium")];
    let mut chat = rich("chat-only-model");
    chat.is_chat_only = Some(true);
    let mut refused = rich("no-agent-model");
    refused.supports_agent = Some(false);
    let mut zdr = rich("zdr-model");
    zdr.requires_data_retention = Some(true);
    let script = serve(
        2,
        join_route(
            (404, Vec::new()),
            (
                200,
                frame(&rich_response(&[claude, gpt, chat, refused, zdr])),
            ),
        ),
    );
    let provider = cursor_provider(script.port);
    let fetched = refresh(&provider, &api_key(TOKEN), directory.path()).await;
    script.finish();

    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    // Chat-only, agent-refusing, and retention-required models never publish.
    assert_eq!(
        ids_of(&fetched),
        [
            "claude-opus-4-8-high",
            "claude-opus-4-8-low",
            "gpt-5-5-medium"
        ]
    );
    let high = row_of(&fetched, "claude-opus-4-8-high");
    // The `context` parameter says 2m: the wire's own number.
    assert_eq!(high.context_window, Some(2_000_000));
    assert!(high.image_input);
    // The variant names no display of its own: the model's picker name.
    assert_eq!(high.display.as_ref(), "Claude Opus 4.8");
    // No wire number for the low lane: none is invented.
    assert_eq!(row_of(&fetched, "claude-opus-4-8-low").context_window, None);
    assert_eq!(
        row_of(&fetched, "gpt-5-5-medium").context_window,
        Some(400_000)
    );
}

#[tokio::test]
async fn entitlement_join_keeps_only_usable_variants_and_unrepresented_slugs() {
    let directory = TestDir::new();
    let mut claude = rich("claude-opus-4-8");
    claude.variants = vec![
        variant("claude-opus-4-8-high"),
        variant("claude-opus-4-8-xhigh"),
    ];
    let script = serve(
        2,
        join_route(
            (
                200,
                frame(&usable_response(&[
                    legacy_details("claude-opus-4-8-high"),
                    legacy_details("legacy-only-slug"),
                ])),
            ),
            (200, frame(&rich_response(&[claude]))),
        ),
    );
    let provider = cursor_provider(script.port);
    let fetched = refresh(&provider, &api_key(TOKEN), directory.path()).await;
    script.finish();

    // The entitled rich slug publishes; the unentitled one drops; the usable
    // slug no rich model represents is retained.
    assert_eq!(
        ids_of(&fetched),
        ["claude-opus-4-8-high", "legacy-only-slug"]
    );
}

#[tokio::test]
async fn both_model_lists_failing_falls_back_to_typed() {
    let directory = TestDir::new();
    let script = serve(2, join_route((500, Vec::new()), (503, Vec::new())));
    let provider = cursor_provider(script.port);
    let fetched = refresh(&provider, &api_key(TOKEN), directory.path()).await;
    script.finish();

    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
    let Some(error) = &fetched.live_error else {
        panic!("a double model-list failure keeps its error");
    };
    assert!(matches!(error, ProviderError::Status { status: 500, .. }));
    // The typed error carries no body material.
    assert!(!format!("{error:?}").contains(TOKEN));
    assert!(!error.to_string().contains(TOKEN));
}

#[tokio::test]
async fn undecodable_lists_fail_without_publishing_rows() {
    let directory = TestDir::new();
    let script = serve(
        2,
        join_route(
            (200, vec![0xFF, 0xFF, 0xFF]),
            (200, frame(b"not-a-message")),
        ),
    );
    let provider = cursor_provider(script.port);
    let fetched = refresh(&provider, &api_key(TOKEN), directory.path()).await;
    script.finish();

    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
    let Some(error) = &fetched.live_error else {
        panic!("a join with no decodable model list fails");
    };
    assert!(error.to_string().contains("cursor catalog"));
}

#[tokio::test]
async fn empty_catalog_answers_publish_live_zero_rows() {
    let directory = TestDir::new();
    let script = serve(2, join_route((200, frame(&[])), (200, frame(&[]))));
    let provider = cursor_provider(script.port);
    let fetched = refresh(&provider, &api_key(TOKEN), directory.path()).await;
    script.finish();

    // A successful empty catalog is live and empty, so model-manager retry
    // and typed-id fallback keep their semantics.
    assert_eq!(fetched.source, CatalogSource::Live);
    assert!(fetched.entries.is_empty());
    assert!(fetched.live_error.is_none());
}

#[tokio::test]
async fn join_deadline_cancels_hanging_model_lists() {
    let directory = TestDir::new();
    let script = serve(
        2,
        hanging_join_route(
            (200, frame(&usable_response(&[legacy_details("slow-slug")]))),
            (200, frame(&[])),
            Duration::from_millis(800),
        ),
    );
    let provider = cursor_provider(script.port);
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &api_key(TOKEN),
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
        env: empty_env(),
    };
    let started = Instant::now();
    let error = fetch_cursor_under(
        &fetch,
        &|_: Duration| std::future::pending::<()>(),
        Duration::from_millis(150),
        Duration::from_millis(100),
    )
    .await
    .expect_err("a join past its deadline cancels");
    let elapsed = started.elapsed();
    script.finish();

    assert!(
        matches!(error, ProviderError::Transport { .. }),
        "{error:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the global deadline cancelled, not the per-request totals: {elapsed:?}"
    );
    assert!(error.to_string().contains("deadline"));
}

#[tokio::test]
async fn no_credential_refuses_the_join_before_any_request() {
    let directory = TestDir::new();
    let provider = cursor_provider(9);
    let fetched = refresh(&provider, &Credential::None, directory.path()).await;

    // The bearer check runs before any URL is built: no request leaves, and
    // the typed fallback carries the credential error.
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::NoCredentials { provider })
                if provider == "cursor"
        ),
        "{:?}",
        fetched.live_error
    );
}

#[tokio::test]
async fn optional_rpcs_survive_end_stream_and_bare_bodies() {
    let directory = TestDir::new();
    let mut claude = rich("claude-opus-4-8");
    claude.variants = vec![variant("claude-opus-4-8-high")];
    let mut encoded = claude.encode_to_vec();
    // Unknown field 7, wire type 1 (fixed64): skipped, never fatal.
    encoded.extend_from_slice(&[0x39, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut available_body = end_stream(r#"{"error":null}"#);
    available_body.extend_from_slice(&frame(&encoded));
    let script = serve(
        2,
        join_route(
            // A bare, unframed protobuf body: the whole-body fallback. Both
            // slugs are entitled, because this fixture answers the framing
            // and unknown-field shapes, not the entitlement filter.
            (
                200,
                frame(&usable_response(&[
                    legacy_details("bare-slug"),
                    legacy_details("claude-opus-4-8-high"),
                ])),
            ),
            (200, available_body),
        ),
    );
    let provider = cursor_provider(script.port);
    let fetched = refresh(&provider, &api_key(TOKEN), directory.path()).await;
    script.finish();

    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(ids_of(&fetched), ["bare-slug", "claude-opus-4-8-high"]);
}

#[tokio::test]
async fn api_key_and_oauth_bearers_are_both_accepted_and_never_logged() {
    for (name, credential, token) in [
        ("api key", api_key("cursor-api-key"), "cursor-api-key"),
        ("oauth", oauth("cursor-oauth-token"), "cursor-oauth-token"),
    ] {
        let directory = TestDir::new();
        let script = serve(
            2,
            join_route(
                (
                    200,
                    frame(&usable_response(&[legacy_details("claude-opus-4-8-high")])),
                ),
                (401, format!("token {token} rejected").into_bytes()),
            ),
        );
        let provider = cursor_provider(script.port);
        let fetched = refresh(&provider, &credential, directory.path()).await;
        let seen = script.finish();

        assert_eq!(
            fetched.source,
            CatalogSource::Live,
            "{name}: {:?}",
            fetched.live_error
        );
        let expected = format!("Bearer {token}");
        for request in &seen.requests {
            assert_eq!(
                request.header("authorization"),
                Some(expected.as_str()),
                "{name} on {}",
                request.path
            );
        }
        // The rejected metadata RPC leaks nothing into the error.
        let text = match &fetched.live_error {
            Some(error) => format!("{error:?}; {error}"),
            None => String::from("no error"),
        };
        assert!(!text.contains(token), "{name} leaked the bearer: {text}");
    }
}

#[tokio::test]
async fn public_resolve_binds_discovered_rows_and_typed_fallbacks() {
    let directory = TestDir::new();
    let mut opus = legacy_details("claude-opus-4-8-high");
    opus.display_name = String::from("Claude Opus 4.8 High");
    let script = serve(
        2,
        join_route((200, frame(&usable_response(&[opus]))), (404, Vec::new())),
    );
    let provider = cursor_provider(script.port);
    let fetched = refresh(&provider, &api_key(TOKEN), directory.path()).await;
    script.finish();

    let catalog = Catalog::with_sources(
        vec![(provider.clone(), CatalogSource::Live)],
        fetched.entries,
    );
    let resolved =
        resolve(&catalog, &[], "cursor/claude-opus-4-8-high").expect("the discovered row resolves");
    assert_eq!(resolved.entry.id.as_ref(), "claude-opus-4-8-high");
    assert_eq!(resolved.entry.provider.as_ref(), "cursor");
    assert_eq!(resolved.entry.display.as_ref(), "Claude Opus 4.8 High");
    // A healthy, authoritative live list rejects an id it does not list.
    assert!(matches!(
        resolve(&catalog, &[], "cursor/not-listed"),
        Err(ResolveError::UnknownModel { .. })
    ));
    // The typed fallback belongs to a Typed-source catalog: with no fetched
    // list, an unlisted id resolves on family defaults with no limit invented.
    let typed_catalog =
        Catalog::with_sources(vec![(provider.clone(), CatalogSource::Typed)], Vec::new());
    let typed = resolve(&typed_catalog, &[], "cursor/not-listed").expect("typed fallback resolves");
    assert_eq!(typed.entry.id.as_ref(), "not-listed");
    assert_eq!(typed.entry.context_window, None);
}

#[test]
fn context_parameters_and_wire_limits_normalize() {
    let provider = provider("cursor", Family::Cursor, "https://api2.cursor.sh");
    let mut claude = rich("claude-opus-4-8");
    claude.context_token_limit = Some(200_000);
    claude.context_token_limit_for_max_mode = Some(1_000_000);
    let mut standard = variant("claude-opus-4-8-high");
    standard.parameter_values = vec![parameter("context", "250k")];
    let mut maxed = variant("claude-opus-4-8-max");
    maxed.is_max_mode = Some(true);
    let mut broken = variant("claude-opus-4-8-low");
    broken.parameter_values = vec![parameter("context", "not-a-number")];
    claude.variants = vec![standard, maxed, broken];
    let rows = decode_cursor_models(
        &provider,
        None,
        Some(&rich_response(&[claude])),
        Some(ProviderError::StreamCut),
    )
    .expect("one rich model decodes");
    assert_eq!(
        ids_of_rows(&rows),
        [
            "claude-opus-4-8-high",
            "claude-opus-4-8-low",
            "claude-opus-4-8-max"
        ]
    );
    assert_eq!(
        row_in(&rows, "claude-opus-4-8-high").context_window,
        Some(250_000)
    );
    // Max mode reads the max-mode limit.
    assert_eq!(
        row_in(&rows, "claude-opus-4-8-max").context_window,
        Some(1_000_000)
    );
    // An unparsable parameter falls to the wire limit, never a guess.
    assert_eq!(
        row_in(&rows, "claude-opus-4-8-low").context_window,
        Some(200_000)
    );
}

#[test]
fn variant_ids_come_from_the_wire_only() {
    let provider = provider("cursor", Family::Cursor, "https://api2.cursor.sh");
    let mut model = rich("composer-2-5");
    let mut spelled = variant("");
    spelled.legacy_slug = None;
    spelled.variant_string_representation = Some(String::from("composer-2-5-fast"));
    let unnamed = variant("");
    model.variants = vec![spelled, unnamed];
    let bare = rich("bare-model");
    let rows = decode_cursor_models(
        &provider,
        None,
        Some(&rich_response(&[model, bare])),
        Some(ProviderError::StreamCut),
    )
    .expect("the rich models decode");
    // The wire-spelled variant publishes as its own row; the unnamed variant
    // collapses into the bare model row, published because no variant of its
    // model survived and no usable list decoded to contradict it.
    assert_eq!(ids_of_rows(&rows), ["bare-model", "composer-2-5-fast"]);
}

#[test]
fn admin_blocked_variants_and_display_precedence_normalize() {
    let provider = provider("cursor", Family::Cursor, "https://api2.cursor.sh");
    let mut model = rich("glm-5-2");
    model.client_display_name = Some(String::from("GLM 5.2"));
    model.parameter_definitions = vec![aiserver::ModelParameterDefinition {
        id: String::from("effort"),
        parameter_type: Some(aiserver::ModelParameterType {
            boolean_parameter: None,
            enum_parameter: Some(aiserver::EnumParameterDefinition {
                values: vec![aiserver::EnumParameterValue {
                    value: String::from("locked"),
                    blocked_by_admin_allowlist: Some(true),
                }],
            }),
        }),
    }];
    let mut blocked = variant("glm-5-2-locked");
    blocked.parameter_values = vec![parameter("effort", "locked")];
    let mut named = variant("glm-5-2-high");
    named.display_name = Some(String::from("GLM 5.2 High"));
    model.variants = vec![blocked, named];
    let rows = decode_cursor_models(
        &provider,
        None,
        Some(&rich_response(&[model])),
        Some(ProviderError::StreamCut),
    )
    .expect("the rich model decodes");
    assert_eq!(ids_of_rows(&rows), ["glm-5-2-high"]);
    assert_eq!(
        row_in(&rows, "glm-5-2-high").display.as_ref(),
        "GLM 5.2 High"
    );
}

#[test]
fn usable_display_picks_the_first_named_spelling() {
    let provider = provider("cursor", Family::Cursor, "https://api2.cursor.sh");
    let mut details = legacy_details("k3");
    details.display_name = String::from("  ");
    details.display_name_short = String::from("Kimi K3");
    details.display_model_id = String::from("kimi-k3");
    details.aliases = vec![String::from("moonshot-k3")];
    let rows = decode_cursor_models(
        &provider,
        Some(&usable_response(&[details])),
        None,
        Some(ProviderError::StreamCut),
    )
    .expect("the usable list decodes");
    assert_eq!(ids_of_rows(&rows), ["k3"]);
    assert_eq!(row_in(&rows, "k3").display.as_ref(), "Kimi K3");
}

fn ids_of_rows(rows: &[CatalogEntry]) -> Vec<String> {
    rows.iter().map(|entry| entry.id.to_string()).collect()
}

fn row_in<'a>(rows: &'a [CatalogEntry], id: &str) -> &'a CatalogEntry {
    rows.iter()
        .find(|entry| entry.id.as_ref() == id)
        .unwrap_or_else(|| panic!("no row for {id}"))
}
