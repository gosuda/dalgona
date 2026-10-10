use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    auth::credential::{Credential, OAuthCredential},
    catalog::{CatalogEntry, ResolvedModel},
    compact::{CompactOutcome, CompactedHistory},
    error::ProviderError,
    stream::{EventStream, NoticeSink, StreamEvent},
    thinking::ThinkingSupport,
};
use dal_core::{
    ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part, Purpose, RawJson,
    RequestParams, SessionId, ThinkingLevel, Usage,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::auth::credential::EnvSnapshot;

type TestResult = Result<(), Box<dyn Error>>;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!("dalgona-provider-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
}

async fn server(
    path_prefix: &str,
    replies: Vec<Reply>,
) -> io::Result<(String, tokio::task::JoinSet<io::Result<Vec<Vec<u8>>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let url = format!("http://{address}{path_prefix}");
    let mut task = tokio::task::JoinSet::new();
    task.spawn(async move {
        let mut requests = Vec::with_capacity(replies.len());
        for reply in replies {
            let (mut socket, _) = listener.accept().await?;
            requests.push(read_request(&mut socket).await?);
            write_reply(&mut socket, reply).await?;
        }
        Ok(requests)
    });
    Ok((url, task))
}

async fn read_request(socket: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 2048];
    loop {
        if let Some(separator) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_end = separator + 4;
            let headers = String::from_utf8_lossy(&bytes[..separator]);
            let body_len = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + body_len {
                return Ok(bytes);
            }
        }
        let count = socket.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "provider closed before sending its request body",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

async fn write_reply(socket: &mut TcpStream, reply: Reply) -> io::Result<()> {
    let reason = match reply.status {
        200 => "OK",
        401 => "Unauthorized",
        500 => "Internal Server Error",
        _ => "Test",
    };
    let headers = format!(
        "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reply.status,
        reply.content_type,
        reply.body.len()
    );
    socket.write_all(headers.as_bytes()).await?;
    socket.write_all(reply.body.as_bytes()).await?;
    socket.flush().await
}

fn entry(
    id: &str,
    family: Family,
    base_url: String,
    key_env: Option<&str>,
    max_concurrent_requests: u32,
    transport: Transport,
) -> ProviderEntry {
    ProviderEntry {
        id: id.into(),
        def: crate::find(id),
        family,
        base_url: base_url.into(),
        transport,
        key_env: key_env.map(Into::into),
        auth: AuthStyle::Bearer,
        max_concurrent_requests,
    }
}

fn config(providers: Vec<ProviderEntry>, stream_max_retries: u32) -> ProviderConfig {
    ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries,
        providers,
        scripted: None,
    }
}

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        version: "test".into(),
        os: "linux".into(),
        os_version: "test".into(),
        arch: "x86_64".into(),
    }
}

fn resolved(provider: &str, family: Family, model: &str) -> ResolvedModel {
    let entry = CatalogEntry {
        provider: provider.into(),
        id: model.into(),
        display: model.into(),
        listing: crate::catalog::Listing::Listed,
        context_window: Some(32_000),
        max_output: Some(4_096),
        thinking: ThinkingSupport::OpenAi {
            accepted: vec![ThinkingLevel::Low, ThinkingLevel::Medium],
            none_supported: true,
        },
        image_input: true,
        image_profile: None,
        remote_compact: true,
        supports_reasoning_summaries: false,
        tool_support: crate::catalog::ToolSupport::Any,
        custom_grammar: false,
        temperature_allowed: true,
        display_supported: false,
    };
    ResolvedModel {
        provider: provider.into(),
        route: ModelRoute::Api {
            family,
            model: model.into(),
        },
        entry,
    }
}

fn request(family: Family, model: &str, context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family,
            model: model.into(),
        },
        system: Arc::from("Follow the request."),
        tools: Arc::<[ModelToolSpec]>::from(Vec::new()),
        context: Arc::<[ContextItem]>::from(context),
        params: RequestParams {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: Some("session-cache:1".into()),
    }
}

fn user_text(text: &str) -> ContextItem {
    ContextItem::User {
        parts: vec![Part::Text { text: text.into() }],
    }
}

fn notice_sink() -> NoticeSink {
    Arc::new(|_| {})
}

async fn collect(mut stream: EventStream) -> Result<Vec<StreamEvent>, ProviderError> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }
    Ok(events)
}

fn chat_reply() -> String {
    String::from(
        r#"data: {"choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]

"#,
    )
}

fn responses_reply() -> String {
    String::from(
        r#"data: {"type":"response.output_text.delta","delta":"Hello"}

data: {"type":"response.completed","response":{"output":[],"usage":null}}

"#,
    )
}

fn stream_reply(body: String) -> Reply {
    Reply {
        status: 200,
        content_type: "text/event-stream",
        body,
    }
}

#[tokio::test]
async fn chat_and_responses_complete_through_the_real_http_lifecycle() -> TestResult {
    let dir = TestDir::new()?;
    let (chat_base, chat_server) = server("/v1", vec![stream_reply(chat_reply())]).await?;
    let (responses_base, responses_server) =
        server("/v1", vec![stream_reply(responses_reply())]).await?;
    let chat_entry = entry(
        "chat-test",
        Family::Chat,
        chat_base,
        Some("CHAT_TEST_KEY"),
        2,
        Transport::Https,
    );
    let responses_entry = entry(
        "responses-test",
        Family::Responses,
        responses_base,
        Some("RESPONSES_TEST_KEY"),
        2,
        Transport::Https,
    );
    let set = ProviderSet::new(
        &config(vec![chat_entry, responses_entry], 0),
        identity(),
        EnvSnapshot::test(&[
            ("CHAT_TEST_KEY", "chat-secret"),
            ("RESPONSES_TEST_KEY", "responses-secret"),
        ]),
        dir.path(),
        dir.path(),
    )?;

    for (id, family, model, expected_path, expected_key, mut result) in [
        (
            "chat-test",
            Family::Chat,
            "gpt-chat-test",
            "/v1/chat/completions",
            "chat-secret",
            chat_server,
        ),
        (
            "responses-test",
            Family::Responses,
            "gpt-responses-test",
            "/v1/responses",
            "responses-secret",
            responses_server,
        ),
    ] {
        let provider = set.provider(resolved(id, family, model))?;
        let request = request(family, model, vec![user_text("Read the file.")]);
        let events = collect(
            provider
                .open(
                    SessionId::new_v7(),
                    &request,
                    &[],
                    notice_sink(),
                    &CancellationToken::new(),
                )
                .await?,
        )
        .await?;
        assert!(
            events.iter().any(|event| {
                matches!(event, StreamEvent::TextDelta { text } if text == "Hello")
            })
        );
        assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
        let captured = result.join_next().await.expect("server completes")??;
        let request_bytes = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
        assert!(request_bytes.starts_with(&format!("post {expected_path} ")));
        assert!(request_bytes.contains(&format!("authorization: bearer {expected_key}")));
        assert!(request_bytes.contains("accept: text/event-stream"));
        assert!(request_bytes.contains("content-type: application/json"));
        assert!(request_bytes.contains("\"prompt_cache_key\":\"session-cache:1\""));
    }
    Ok(())
}

fn tool_reply(family: Family, wire: &str) -> String {
    match family {
        Family::Chat => format!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{{\"name\":\"{wire}\",\"arguments\":\"{{}}\"}}}}]}},\"finish_reason\":null}}]}}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
        ),
        Family::Responses | Family::Codex => format!(
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"id\":\"fc-1\",\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"{wire}\",\"arguments\":\"\"}}}}\n\ndata: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc-1\",\"delta\":\"{{}}\"}}\n\ndata: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc-1\",\"arguments\":\"{{}}\"}}\n\ndata: {{\"type\":\"response.output_item.done\",\"item\":{{\"id\":\"fc-1\",\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"{wire}\",\"arguments\":\"{{}}\"}}}}\n\ndata: {{\"type\":\"response.completed\",\"response\":{{\"output\":[{{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"{wire}\",\"arguments\":\"{{}}\"}}],\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n\n"
        ),
        Family::Anthropic => format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-test\",\"stop_reason\":null,\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n\nevent: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"{wire}\",\"input\":{{}}}}}}\n\nevent: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\nevent: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}},\"usage\":{{\"output_tokens\":1}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
        ),
    }
}

type CapturedServer = tokio::task::JoinSet<io::Result<Vec<Vec<u8>>>>;

async fn wire_named_servers(wire: &str) -> io::Result<[(&'static str, String, CapturedServer); 4]> {
    let (chat_base, chat_server) =
        server("/v1", vec![stream_reply(tool_reply(Family::Chat, wire))]).await?;
    let (responses_base, responses_server) = server(
        "/v1",
        vec![stream_reply(tool_reply(Family::Responses, wire))],
    )
    .await?;
    let (anthropic_base, anthropic_server) =
        server("", vec![stream_reply(tool_reply(Family::Anthropic, wire))]).await?;
    let (codex_base, codex_server) = server(
        "/backend-api/codex",
        vec![stream_reply(tool_reply(Family::Codex, wire))],
    )
    .await?;
    Ok([
        ("chat-test", chat_base, chat_server),
        ("responses-test", responses_base, responses_server),
        ("anthropic-test", anthropic_base, anthropic_server),
        ("openai-codex", codex_base, codex_server),
    ])
}

fn mapped_entries(bases: &[(&'static str, String, CapturedServer)]) -> Vec<ProviderEntry> {
    let entry_for = |index: usize| {
        let (id, base, _) = &bases[index];
        let family = match *id {
            "chat-test" => Family::Chat,
            "responses-test" => Family::Responses,
            "anthropic-test" => Family::Anthropic,
            _ => Family::Codex,
        };
        entry(
            id,
            family,
            base.clone(),
            match *id {
                "openai-codex" => None,
                _ => Some(match *id {
                    "chat-test" => "CHAT_TEST_KEY",
                    "responses-test" => "RESPONSES_TEST_KEY",
                    _ => "ANTHROPIC_TEST_KEY",
                }),
            },
            1,
            Transport::Https,
        )
    };
    let mut entries: Vec<_> = (0..4).map(entry_for).collect();
    entries[2].auth = AuthStyle::XApiKey;
    entries
}

async fn assert_mapped_round_trip(
    set: &ProviderSet,
    internal: &str,
    wire: &str,
    id: &'static str,
    family: Family,
    model: &str,
    requests: &mut CapturedServer,
) -> TestResult {
    let mut resolved = resolved(id, family, model);
    if family == Family::Anthropic {
        resolved.entry.thinking = ThinkingSupport::UnknownAdaptive;
    }
    let provider = set.provider(resolved)?;
    let mut request = request(family, model, vec![user_text("Call the mapped tool.")]);
    request.tools = Arc::from([dal_core::ModelToolSpec {
        name: internal.into(),
        description: "Mapped tool.".into(),
        parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)?,
        grammar: None,
    }]);
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &request,
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?,
    )
    .await?;
    assert!(events.iter().any(
        |event| matches!(event, StreamEvent::ToolCallStarted { name, .. } if name == internal)
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolCallsDone { calls }
            if calls.len() == 1 && calls[0].name == internal
    )));
    let captured = requests.join_next().await.expect("server completes")??;
    let body = String::from_utf8_lossy(&captured[0]);
    assert!(body.contains(wire));
    assert!(!body.contains(internal));
    Ok(())
}

#[tokio::test]
async fn mapped_tool_names_round_trip_through_all_http_families() -> TestResult {
    let dir = TestDir::new()?;
    let internal = "deploy.web-x.list";
    let wire = crate::tool_names::wire_name(internal, 64).into_owned();
    assert!(
        wire.bytes()
            .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') })
    );
    let mut servers = wire_named_servers(&wire).await?;
    let credential = OAuthCredential {
        access_token: crate::auth::credential::SecretString::from("codex-access"),
        refresh_token: crate::auth::credential::SecretString::from("codex-refresh"),
        expires_at: None,
        id_token: Some(String::from("codex-id-token")),
        account_id: Some(String::from("acct-1")),
    };
    let mut auth = crate::auth::credential::AuthStore::empty(dir.path().join("auth.json"));
    auth.set("openai-codex", Credential::OAuth(credential))?;
    auth.store()?;
    let endpoints = crate::auth::refresh::TokenEndpoints::with_bases(
        "http://127.0.0.1:1/anthropic",
        "http://127.0.0.1:1/codex",
    )?;
    let set = ProviderSet::build(
        &config(mapped_entries(&servers), 0),
        identity(),
        EnvSnapshot::test(&[
            ("CHAT_TEST_KEY", "chat-secret"),
            ("RESPONSES_TEST_KEY", "responses-secret"),
            ("ANTHROPIC_TEST_KEY", "anthropic-secret"),
        ]),
        dir.path(),
        dir.path(),
        endpoints,
    )?;

    for (index, model) in [
        "gpt-chat-test",
        "gpt-responses-test",
        "claude-test",
        "gpt-codex-test",
    ]
    .into_iter()
    .enumerate()
    {
        let (id, _, requests) = &mut servers[index];
        let family = match *id {
            "chat-test" => Family::Chat,
            "responses-test" => Family::Responses,
            "anthropic-test" => Family::Anthropic,
            _ => Family::Codex,
        };
        assert_mapped_round_trip(&set, internal, &wire, id, family, model, requests).await?;
    }
    Ok(())
}

#[tokio::test]
async fn scripted_usage_and_compaction_keep_their_typed_values() -> TestResult {
    let usage = Usage {
        input_tokens: 11,
        cached_input_tokens: 3,
        output_tokens: 5,
        reasoning_tokens: Some(2),
        cache_write_tokens: 1,
        cost_usd: Some(0.002),
    };
    let raw = RawJson::parse(r#"{"type":"summary","text":"keep raw"}"#)?;
    let history = CompactedHistory {
        family: Family::Responses,
        model: "gpt-test".into(),
        items: vec![raw.clone()],
    };
    let script = crate::scripted::Script::new(vec![
        crate::scripted::ScriptStep::Usage(usage),
        crate::scripted::ScriptStep::Compact(CompactOutcome::Compacted(history)),
    ])?;
    let provider = Provider::Scripted(script);
    assert_eq!(provider.scripted_usage()?, usage);
    let result = provider
        .compact(
            SessionId::new_v7(),
            &request(Family::Responses, "gpt-test", Vec::new()),
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    let Some(CompactOutcome::Compacted(history)) = result else {
        return Err(io::Error::other("Scripted compaction did not return its history").into());
    };
    assert_eq!(history.family, Family::Responses);
    assert_eq!(history.model.as_ref(), "gpt-test");
    assert_eq!(history.items.as_slice(), &[raw]);
    Ok(())
}

#[tokio::test]
async fn unresolved_blob_is_typed_and_sends_no_request() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(chat_reply())]).await?;
    let config = config(
        vec![entry(
            "chat-test",
            Family::Chat,
            base,
            Some("CHAT_TEST_KEY"),
            1,
            Transport::Https,
        )],
        0,
    );
    let set = ProviderSet::new(
        &config,
        identity(),
        EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("chat-test", Family::Chat, "gpt-chat-test"))?;
    let blob_id = dal_core::BlobId::from_bytes(b"stored image bytes");
    let request = request(
        Family::Chat,
        "gpt-chat-test",
        vec![ContextItem::User {
            parts: vec![Part::Blob {
                blob_id,
                mime: "image/png".into(),
                bytes: 18,
            }],
        }],
    );
    let result = provider
        .open(
            SessionId::new_v7(),
            &request,
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(
        result,
        Err(ProviderError::UnresolvedBlob { blob_id: found }) if found == blob_id
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
            .await
            .is_err()
    );
    server_task.abort_all();
    Ok(())
}
struct GatedServer {
    base: String,
    accepted: mpsc::Receiver<Vec<u8>>,
    ready: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
    task: tokio::task::JoinSet<io::Result<Vec<Vec<u8>>>>,
}

async fn gated_server(body: String) -> io::Result<GatedServer> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (accepted_sender, accepted) = mpsc::channel(2);
    let (ready_sender, ready) = oneshot::channel();
    let (release, release_receiver) = oneshot::channel();
    let mut task = tokio::task::JoinSet::new();
    task.spawn(async move {
        let mut ready_sender = Some(ready_sender);
        let mut release_receiver = Some(release_receiver);
        let mut handlers = tokio::task::JoinSet::new();
        for index in 0..2 {
            let (mut socket, _) = listener.accept().await?;
            let request = read_request(&mut socket).await?;
            accepted_sender
                .send(request.clone())
                .await
                .map_err(|_| io::Error::other("request observer was dropped"))?;
            let body = body.clone();
            let ready = if index == 0 {
                ready_sender.take()
            } else {
                None
            };
            let release = if index == 0 {
                release_receiver.take()
            } else {
                None
            };
            handlers.spawn(async move {
                if index == 0 {
                    write_reply_head(&mut socket, 200, "text/event-stream", body.len()).await?;
                    if let Some(ready) = ready {
                        let _ = ready.send(());
                    }
                    if let Some(release) = release {
                        let _ = release.await;
                    }
                    socket.write_all(body.as_bytes()).await?;
                    socket.flush().await?;
                } else {
                    write_reply(
                        &mut socket,
                        Reply {
                            status: 200,
                            content_type: "text/event-stream",
                            body,
                        },
                    )
                    .await?;
                }
                Ok::<_, io::Error>(request)
            });
        }
        let mut requests = Vec::new();
        while let Some(joined) = handlers.join_next().await {
            match joined {
                Ok(Ok(request)) => requests.push(request),
                Ok(Err(error)) => return Err(error),
                Err(error) => return Err(io::Error::other(error.to_string())),
            }
        }
        Ok(requests)
    });
    Ok(GatedServer {
        base: format!("http://{address}/v1"),
        accepted,
        ready,
        release,
        task,
    })
}

async fn write_reply_head(
    socket: &mut TcpStream,
    status: u16,
    content_type: &str,
    body_len: usize,
) -> io::Result<()> {
    let reason = if status == 200 { "OK" } else { "Test" };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {body_len}\r\nconnection: close\r\n\r\n"
    );
    socket.write_all(headers.as_bytes()).await?;
    socket.flush().await
}

async fn stalled_server() -> io::Result<(
    String,
    oneshot::Receiver<()>,
    tokio::task::JoinSet<io::Result<bool>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (headers_sent, headers_received) = oneshot::channel();
    let mut task = tokio::task::JoinSet::new();
    task.spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let _request = read_request(&mut socket).await?;
        write_reply_head(&mut socket, 200, "text/event-stream", 4096).await?;
        let _ = headers_sent.send(());
        let mut byte = [0_u8; 1];
        let closed =
            match tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte)).await {
                Ok(Ok(0) | Err(_)) => true,
                Ok(Ok(_)) | Err(_) => false,
            };
        Ok(closed)
    });
    Ok((format!("http://{address}/v1"), headers_received, task))
}

#[tokio::test]
async fn provider_semaphore_bounds_concurrent_local_requests() -> TestResult {
    let dir = TestDir::new()?;
    let mut gated = gated_server(chat_reply()).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "chat-test",
                Family::Chat,
                gated.base.clone(),
                Some("CHAT_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("chat-test", Family::Chat, "gpt-chat-test"))?;
    let first_request = request(Family::Chat, "gpt-chat-test", vec![user_text("first")]);
    let first_stream = provider
        .open(
            SessionId::new_v7(),
            &first_request,
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    let mut first_tasks = tokio::task::JoinSet::new();
    first_tasks.spawn(collect(first_stream));
    let first_wire = tokio::time::timeout(Duration::from_secs(1), gated.accepted.recv())
        .await?
        .ok_or_else(|| io::Error::other("first provider request was not accepted"))?;
    assert!(String::from_utf8_lossy(&first_wire).starts_with("POST /v1/chat/completions "));
    gated.ready.await?;

    let second_request = request(Family::Chat, "gpt-chat-test", vec![user_text("second")]);
    let second_stream = provider
        .open(
            SessionId::new_v7(),
            &second_request,
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    let mut second_tasks = tokio::task::JoinSet::new();
    second_tasks.spawn(collect(second_stream));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), gated.accepted.recv())
            .await
            .is_err(),
        "the provider exceeded its configured concurrency cap"
    );

    assert!(gated.release.send(()).is_ok());
    let second_wire = tokio::time::timeout(Duration::from_secs(1), gated.accepted.recv())
        .await?
        .ok_or_else(|| io::Error::other("queued provider request was not admitted"))?;
    assert!(String::from_utf8_lossy(&second_wire).starts_with("POST /v1/chat/completions "));
    assert!(
        first_tasks
            .join_next()
            .await
            .expect("first collect completes")??
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop { .. }))
    );
    assert!(
        second_tasks
            .join_next()
            .await
            .expect("second collect completes")??
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop { .. }))
    );
    let _requests = gated.task.join_next().await.expect("server completes")??;
    Ok(())
}

#[tokio::test]
async fn cancellation_closes_the_active_response_and_releases_its_permit() -> TestResult {
    let dir = TestDir::new()?;
    let (base, headers_received, mut server_task) = stalled_server().await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "chat-test",
                Family::Chat,
                base,
                Some("CHAT_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("chat-test", Family::Chat, "gpt-chat-test"))?;
    let cancel = CancellationToken::new();
    let stream = provider
        .open(
            SessionId::new_v7(),
            &request(Family::Chat, "gpt-chat-test", vec![user_text("wait")]),
            &[],
            notice_sink(),
            &cancel,
        )
        .await?;
    let mut consumer_tasks = tokio::task::JoinSet::new();
    consumer_tasks.spawn(async move {
        let mut stream = stream;
        stream.next().await
    });
    headers_received.await?;
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), server_task.join_next())
            .await?
            .expect("server completes")??
    );
    consumer_tasks.abort_all();
    while consumer_tasks.join_next().await.is_some() {}
    assert_eq!(
        set.inner
            .providers
            .get("chat-test")
            .map(|slot| slot.permits.available_permits()),
        Some(1)
    );
    Ok(())
}
#[tokio::test]
async fn codex_401_refreshes_then_retries_with_the_stored_new_credential() -> TestResult {
    let dir = TestDir::new()?;
    let (codex_base, mut codex_server) = server(
        "/backend-api/codex",
        vec![
            Reply {
                status: 401,
                content_type: "application/json",
                body: String::from(r#"{"error":{"type":"invalid_token","message":"expired"}}"#),
            },
            stream_reply(responses_reply()),
        ],
    )
    .await?;
    let (token_base, mut token_server) = server(
        "/codex",
        vec![Reply {
            status: 200,
            content_type: "application/json",
            body: String::from(
                r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
            ),
        }],
    )
    .await?;

    let credential = OAuthCredential {
        access_token: crate::auth::credential::SecretString::from("old-access"),
        refresh_token: crate::auth::credential::SecretString::from("old-refresh"),
        expires_at: None,
        id_token: Some(String::from("old-id-token")),
        account_id: Some(String::from("acct-1")),
    };
    let mut auth = crate::auth::credential::AuthStore::empty(dir.path().join("auth.json"));
    auth.set("openai-codex", Credential::OAuth(credential))?;
    auth.store()?;

    let codex_entry = entry(
        "openai-codex",
        Family::Codex,
        codex_base,
        None,
        1,
        Transport::Https,
    );
    let config = config(vec![codex_entry], 0);
    let endpoints = crate::auth::refresh::TokenEndpoints::with_bases(
        "http://127.0.0.1:1/anthropic",
        &token_base,
    )?;
    let set = ProviderSet::build(
        &config,
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
        endpoints,
    )?;
    let provider = set.provider(resolved("openai-codex", Family::Codex, "gpt-codex-test"))?;
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &request(Family::Codex, "gpt-codex-test", vec![user_text("continue")]),
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?,
    )
    .await?;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop { .. }))
    );

    let codex_requests = codex_server
        .join_next()
        .await
        .expect("server completes")??;
    assert_eq!(codex_requests.len(), 2);
    let first = String::from_utf8_lossy(&codex_requests[0]).to_ascii_lowercase();
    let second = String::from_utf8_lossy(&codex_requests[1]).to_ascii_lowercase();
    assert!(first.contains("authorization: bearer old-access"));
    assert!(second.contains("authorization: bearer new-access"));
    assert!(second.contains("\"prompt_cache_key\":\"session-cache:1\""));
    let token_requests = token_server
        .join_next()
        .await
        .expect("server completes")??;
    assert_eq!(token_requests.len(), 1);
    assert!(String::from_utf8_lossy(&token_requests[0]).contains("old-refresh"));
    let stored = crate::auth::credential::AuthStore::load(dir.path().join("auth.json"))?;
    let Some(Credential::OAuth(stored)) = stored.credential("openai-codex") else {
        return Err(io::Error::other("refreshed OAuth credential was not stored").into());
    };
    assert_eq!(stored.access_token.expose(), "new-access");
    Ok(())
}

fn reply(status: u16, content_type: &str, body: Vec<u8>) -> reqwest::Response {
    reqwest::Response::from(
        hyper::http::Response::builder()
            .status(status)
            .header("content-type", content_type)
            .body(reqwest::Body::from(body))
            .unwrap(),
    )
}

async fn failure_of(response: reqwest::Response, key: &str) -> (u16, Option<String>, String) {
    let credential = Credential::ApiKey {
        key: crate::auth::credential::SecretString::from(key),
    };
    let failure = super::request::status_failure(
        response,
        Family::Chat,
        &credential,
        &[],
        &CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|_| panic!("status_failure errored"))
    .expect("not cancelled");
    match failure {
        crate::lifecycle::AttemptFailure::Response {
            status,
            code,
            message,
            ..
        } => (status, code, message),
        other @ crate::lifecycle::AttemptFailure::Provider(_) => {
            panic!("expected a response failure, got {other:?}")
        }
    }
}

#[tokio::test]
async fn an_error_body_never_echoes_the_key_even_behind_json_escapes() {
    let body = br#"{"error":{"code":"invalid_api_key","message":"bad key sk\u002dsecret-123"}}"#;
    let (status, code, message) = failure_of(
        reply(401, "application/json", body.to_vec()),
        "sk-secret-123",
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(code.as_deref(), Some("invalid_api_key"));
    assert!(!message.contains("sk-secret-123"), "{message}");
}

#[tokio::test]
async fn error_bodies_of_any_shape_or_content_type_become_one_bounded_line() {
    let (status, code, message) = failure_of(
        reply(
            502,
            "text/html",
            b"<html>Bad Gateway</html>\n<p>more</p>".to_vec(),
        ),
        "k",
    )
    .await;
    assert_eq!((status, code.as_deref()), (502, None));
    assert_eq!(message, "<html>Bad Gateway</html>");

    let (_, code, message) = failure_of(
        reply(
            500,
            "application/json",
            br#"{"error":"flat string"}"#.to_vec(),
        ),
        "k",
    )
    .await;
    assert_eq!(code, None);
    assert_eq!(message, r#"{"error":"flat string"}"#);

    let (_, _, message) = failure_of(reply(500, "text/plain", vec![0xFF, 0xFE, b'x']), "k").await;
    assert_eq!(message, "\u{FFFD}\u{FFFD}x");

    let long = format!(r#"{{"message":"{}"}}"#, "é".repeat(200));
    let (_, _, message) = failure_of(reply(400, "application/json", long.into_bytes()), "k").await;
    assert_eq!(message.len(), 300);
    assert!(message.chars().all(|c| c == 'é'));

    let (_, code, message) = failure_of(reply(503, "text/plain", Vec::new()), "k").await;
    assert_eq!((code, message.as_str()), (None, ""));
}

async fn drain(mut stream: EventStream) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

#[tokio::test]
async fn a_two_hundred_that_is_not_an_sse_stream_never_ends_in_stop() {
    for body in [
        br#"{"error":{"message":"upstream failed"}}"#.to_vec(),
        b"<html>captive portal</html>".to_vec(),
        Vec::new(),
    ] {
        for family in [
            Family::Chat,
            Family::Responses,
            Family::Codex,
            Family::Anthropic,
        ] {
            let events = drain(super::transport::decode_response(
                reply(200, "text/html", body.clone()),
                family,
                "p",
                "m",
                false,
                None,
                Vec::new(),
            ))
            .await;
            assert!(
                matches!(events.as_slice(), [Err(_)]),
                "{family:?} {body:?}: {events:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_body_read_failure_mid_stream_is_a_retryable_transport_error_at_any_cut_point() {
    let frame = br#"data: {"choices":[{"delta":{"content":"hi"}}]}"#;
    for (cut, delivered) in [
        (frame.to_vec(), true),
        (frame[..frame.len() - 9].to_vec(), false),
    ] {
        let mut first = cut;
        if delivered {
            first.extend_from_slice(b"\n\n");
        }
        let chunks: Vec<Result<Vec<u8>, io::Error>> =
            vec![Ok(first), Err(io::Error::other("connection reset"))];
        let response = reqwest::Response::from(hyper::http::Response::new(
            reqwest::Body::wrap_stream(futures::stream::iter(chunks)),
        ));
        let events = drain(super::transport::decode_response(
            response,
            Family::Chat,
            "p",
            "m",
            false,
            None,
            Vec::new(),
        ))
        .await;
        assert_eq!(
            matches!(events.first(), Some(Ok(StreamEvent::TextDelta { .. }))),
            delivered
        );
        let Some(Err(ProviderError::Transport { family, reason })) = events.last() else {
            panic!("expected a transport error last, got {events:?}");
        };
        assert_eq!(*family, Family::Chat);
        assert!(reason.contains("connection reset"), "{reason}");
        assert_eq!(events.iter().filter(|event| event.is_err()).count(), 1);
    }
}

#[tokio::test]
async fn credential_failures_redact_every_oauth_secret_including_account_id() {
    let credential = Credential::OAuth(OAuthCredential {
        access_token: crate::auth::credential::SecretString::from("tok-access"),
        refresh_token: crate::auth::credential::SecretString::from("tok-refresh"),
        expires_at: None,
        id_token: Some(String::from("tok-id")),
        account_id: Some(String::from("acct-secret")),
    });
    for body in [
        r#"{"error":{"code":"echo-acct-secret","message":"tok-access acct-secret"}}"#,
        r#"{"error":{"code":"echo-acct\u002dsecret","message":"tok\u002daccess acct\u002dsecret"}}"#,
    ] {
        let response = reqwest::Response::from(
            hyper::http::Response::builder()
                .status(400)
                .body(reqwest::Body::from(body.to_owned()))
                .expect("valid response"),
        );
        let failure = super::request::status_failure(
            response,
            Family::Responses,
            &credential,
            &[],
            &CancellationToken::new(),
        )
        .await
        .expect("body reads")
        .expect("not cancelled");
        assert!(matches!(
            &failure,
            crate::lifecycle::AttemptFailure::Response { code: Some(code), message, .. }
                if code == "echo-<redacted>" && message == "<redacted> <redacted>"
        ));
    }
}

#[tokio::test]
async fn credential_failures_redact_an_account_id_taken_from_the_id_token() {
    let claims = r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-claim-9d2e"}}"#;
    let credential = Credential::OAuth(OAuthCredential {
        access_token: crate::auth::credential::SecretString::from("tok-access"),
        refresh_token: crate::auth::credential::SecretString::from("tok-refresh"),
        expires_at: None,
        id_token: Some(format!(
            "h.{}.s",
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, claims)
        )),
        account_id: None,
    });
    let response = reqwest::Response::from(
        hyper::http::Response::builder()
            .status(400)
            .body(reqwest::Body::from(
                r#"{"error":{"code":"bad_request","message":"echo acct-claim-9d2e"}}"#.to_owned(),
            ))
            .expect("valid response"),
    );
    let failure = super::request::status_failure(
        response,
        Family::Responses,
        &credential,
        &[],
        &CancellationToken::new(),
    )
    .await
    .expect("body reads")
    .expect("not cancelled");
    assert!(matches!(
        &failure,
        crate::lifecycle::AttemptFailure::Response { message, .. } if message == "echo <redacted>"
    ));
    assert!(!format!("{failure:?}").contains("acct-claim-9d2e"));
}
