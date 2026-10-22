use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    auth::{
        credential::{Credential, OAuthCredential, SecretString},
        login::{LoginIo, LoginSite, Method, login},
    },
    catalog::{CatalogEntry, CatalogSource, ResolvedModel},
    compact::{CompactOutcome, CompactedHistory},
    error::ProviderError,
    family::devin::{
        connect::frame_message,
        proto::{
            AssignModelRequest, AssignModelResponse, ClientModelConfig, GetChatMessageRequest,
            GetChatMessageResponse, GetCliModelConfigsResponse, GetUserJwtRequest,
            GetUserJwtResponse, ModelAssignment, ModelInfo, stop_reason,
        },
    },
    stream::{EventStream, NoticeSink, StreamEvent},
    thinking::ThinkingSupport,
};
use dal_core::{
    Config, ConfigProduct, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part,
    Purpose, RawJson, RequestParams, SessionId, ThinkingLevel, Usage,
};
use prost::Message as _;
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
    body: Vec<u8>,
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
    socket.write_all(&reply.body).await?;
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
        settings: ProviderSettings::default(),
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
        premium_multiplier: None,
        price: None,
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

async fn drain_stream(mut stream: EventStream) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let mut events = Vec::new();
    let mut failure = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    (events, failure)
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
        body: body.into_bytes(),
    }
}

fn proto_reply(body: Vec<u8>) -> Reply {
    Reply {
        status: 200,
        content_type: "application/proto",
        body,
    }
}

fn denied_reply() -> Reply {
    Reply {
        status: 403,
        content_type: "text/plain",
        body: Vec::from(b"denied".as_slice()),
    }
}

/// The body bytes after the request head's blank line.
fn request_body(captured: &[u8]) -> &[u8] {
    let head_end = captured
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a request head ends");
    &captured[head_end + 4..]
}

fn stored_oauth(dir: &TestDir, id: &str, access: &str) -> Result<(), Box<dyn Error>> {
    let mut auth = crate::auth::credential::AuthStore::empty(dir.path().join("auth.json"));
    auth.set(
        id,
        Credential::OAuth(OAuthCredential {
            access_token: SecretString::from(access),
            refresh_token: SecretString::from("stored-refresh"),
            expires_at: None,
            id_token: None,
            account_id: None,
        }),
    )?;
    auth.store()?;
    Ok(())
}

fn mistral_reply() -> String {
    String::from(
        "data: {\"type\":\"conversation.response.started\",\"conversation_id\":\"conv-1\"}\n\ndata: {\"type\":\"message.output.delta\",\"content\":\"Hello\"}\n\ndata: {\"type\":\"conversation.response.done\",\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n",
    )
}

fn google_reply() -> String {
    String::from(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":4,\"thoughtsTokenCount\":1,\"totalTokenCount\":8}}\n\n",
    )
}

fn google_cca_reply() -> String {
    String::from(
        "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}]},\"finishReason\":\"STOP\"}]}}\n\n",
    )
}

fn ollama_reply() -> String {
    String::from(
        "{\"message\":{\"content\":\"Hello\"},\"done\":false}\n{\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":10,\"eval_count\":5}\n",
    )
}

fn devin_jwt_reply(token: &str) -> Vec<u8> {
    GetUserJwtResponse {
        user_jwt: token.to_owned(),
        custom_api_server_url: String::new(),
    }
    .encode_to_vec()
}

fn devin_assign_reply() -> Vec<u8> {
    AssignModelResponse {
        assignment: Some(ModelAssignment {
            assignment_jwt: String::from("assign-1"),
            model_uid: String::from("devin-serve-1"),
            ..Default::default()
        }),
    }
    .encode_to_vec()
}

fn devin_chat_reply() -> Vec<u8> {
    let message = GetChatMessageResponse {
        message_id: String::from("m1"),
        delta_text: String::from("Hello"),
        stop_reason: stop_reason::STOP_PATTERN,
        ..Default::default()
    };
    frame_message(&message.encode_to_vec(), 0)
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
        Family::Bedrock
        | Family::Cursor
        | Family::Devin
        | Family::Droid
        | Family::GitLab
        | Family::GoogleGenerativeAi
        | Family::GoogleVertex
        | Family::GoogleGeminiCli
        | Family::GoogleAntigravity
        | Family::Mistral
        | Family::Ollama
        | Family::PiMessages => panic!("no tool-call reply fixture for {family:?}"),
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
        defer_loading: false,
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
        "http://127.0.0.1:1/workos",
        "http://127.0.0.1:1/gitlab",
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
        premium_requests: None,
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
                            body: body.into_bytes(),
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
                body: String::from(r#"{"error":{"type":"invalid_token","message":"expired"}}"#)
                    .into_bytes(),
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
            )
            .into_bytes(),
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
        "http://127.0.0.1:1/workos",
        "http://127.0.0.1:1/gitlab",
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
                None,
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
            None,
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

#[tokio::test]
async fn stream_limit_releases_the_provider_admission_permit() -> TestResult {
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = Arc::clone(&gate).acquire_owned().await?;
    let source = EventStream::new(
        futures::stream::iter([Err(ProviderError::Limit(
            crate::error::LimitError::StreamEvents,
        ))]),
        || {},
    );
    let mut held = super::transport::hold_permit(source, permit);
    assert_eq!(gate.available_permits(), 0);
    assert!(matches!(
        held.next().await,
        Some(Err(ProviderError::Limit(
            crate::error::LimitError::StreamEvents
        )))
    ));
    assert_eq!(gate.available_permits(), 1);
    Ok(())
}

fn anthropic_reply() -> String {
    String::from(
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","content":[],"model":"preset-model","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}

event: message_stop
data: {"type":"message_stop"}

"#,
    )
}

/// A provider set decoded from `toml` through the strict configuration, so a
/// preset row reaches the request lifecycle the way a host configures it.
fn preset_set(
    dir: &TestDir,
    toml: &str,
    env: &[(&str, &str)],
) -> Result<ProviderSet, Box<dyn Error>> {
    let config = Config::load(ConfigProduct::Dalgon, dir.path(), "", Some(toml))?;
    let providers = ProviderConfig::from_config(&config)?;
    Ok(ProviderSet::new(
        &providers,
        identity(),
        EnvSnapshot::test(env),
        dir.path(),
        dir.path(),
    )?)
}

/// Opens one text turn on `id`, checks its events, and returns the request
/// head the loopback server captured, lowercased.
async fn say_hello(
    set: &ProviderSet,
    id: &str,
    family: Family,
    captured: &mut CapturedServer,
) -> Result<String, Box<dyn Error>> {
    let mut route = resolved(id, family, "preset-model");
    if family == Family::Anthropic {
        route.entry.thinking = ThinkingSupport::UnknownAdaptive;
    }
    let provider = set.provider(route)?;
    let request = request(family, "preset-model", vec![user_text("Say hello.")]);
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello")),
        "{id} streamed no text"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::Stop { .. })),
        "{id} did not stop"
    );
    let request = captured.join_next().await.expect("server completes")??;
    Ok(String::from_utf8_lossy(&request[0]).to_ascii_lowercase())
}

/// One preset row and what its request must look like on the wire.
struct PresetCase {
    id: &'static str,
    family: Family,
    /// The path prefix of the loopback base URL the configuration selects.
    prefix: &'static str,
    env: &'static [(&'static str, &'static str)],
    reply: fn() -> String,
    path: &'static str,
    headers: &'static [&'static str],
    forbidden: &'static [&'static str],
}

fn preset_cases() -> [PresetCase; 6] {
    [
        // A Chat row whose base has no version segment, keyed by its first variable.
        PresetCase {
            id: "deepseek",
            family: Family::Chat,
            prefix: "",
            env: &[("DEEPSEEK_API_KEY", "deepseek-secret")],
            reply: chat_reply,
            path: "/chat/completions",
            headers: &["authorization: bearer deepseek-secret"],
            forbidden: &["\nx-api-key:", "\nanthropic-version:"],
        },
        // Both variables are set: the first one named by the row wins.
        PresetCase {
            id: "huggingface",
            family: Family::Chat,
            prefix: "/v1",
            env: &[
                ("HF_TOKEN", "hf-secret"),
                ("HUGGINGFACE_HUB_TOKEN", "hub-secret"),
            ],
            reply: chat_reply,
            path: "/v1/chat/completions",
            headers: &["authorization: bearer hub-secret"],
            forbidden: &["hf-secret"],
        },
        // Only the second variable is set: it still keys the row.
        PresetCase {
            id: "moonshot",
            family: Family::Chat,
            prefix: "/v1",
            env: &[("KIMI_API_KEY", "kimi-secret")],
            reply: chat_reply,
            path: "/v1/chat/completions",
            headers: &["authorization: bearer kimi-secret"],
            forbidden: &["\nx-api-key:"],
        },
        PresetCase {
            id: "xai",
            family: Family::Responses,
            prefix: "/v1",
            env: &[("XAI_API_KEY", "xai-secret")],
            reply: responses_reply,
            path: "/v1/responses",
            headers: &["authorization: bearer xai-secret"],
            forbidden: &["\nx-api-key:"],
        },
        // A Messages host that expects a bearer token.
        PresetCase {
            id: "minimax",
            family: Family::Anthropic,
            prefix: "/anthropic",
            env: &[("MINIMAX_API_KEY", "minimax-secret")],
            reply: anthropic_reply,
            path: "/anthropic/v1/messages",
            headers: &[
                "authorization: bearer minimax-secret",
                "anthropic-version: 2023-06-01",
            ],
            forbidden: &["\nx-api-key:"],
        },
        // A Messages host that expects `x-api-key`.
        PresetCase {
            id: "umans",
            family: Family::Anthropic,
            prefix: "",
            env: &[("UMANS_AI_CODING_PLAN_API_KEY", "umans-secret")],
            reply: anthropic_reply,
            path: "/v1/messages",
            headers: &["x-api-key: umans-secret", "anthropic-version: 2023-06-01"],
            forbidden: &["\nauthorization:"],
        },
    ]
}

#[tokio::test]
async fn presets_reach_their_wire_through_strict_config_and_the_real_lifecycle() -> TestResult {
    let dir = TestDir::new()?;
    for case in preset_cases() {
        let id = case.id;
        let (base, mut captured) = server(case.prefix, vec![stream_reply((case.reply)())]).await?;
        let toml = format!("[providers.{id}]\nbase_url = '{base}'\n");
        let set = preset_set(&dir, &toml, case.env)?;
        let head = say_hello(&set, id, case.family, &mut captured).await?;
        assert!(
            head.starts_with(&format!("post {} ", case.path)),
            "{id}: {head}"
        );
        for line in case.headers {
            assert!(head.contains(line), "{id}: {line} is not in {head}");
        }
        for text in case.forbidden {
            assert!(!head.contains(text), "{id}: {text:?} is in {head}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_preset_without_a_key_refuses_with_the_typed_error_and_asks_nothing() -> TestResult {
    let dir = TestDir::new()?;
    // `xai` names its own variable, which replaces the row's; the row's
    // variable is set but unused. An empty value counts as unset.
    let set = preset_set(
        &dir,
        "[providers.xai]\nkey_env = 'ELSEWHERE_KEY'\n",
        &[
            ("OPENAI_API_KEY", "openai-secret"),
            ("DEEPSEEK_API_KEY", "deepseek-secret"),
            ("GROQ_API_KEY", ""),
            ("XAI_API_KEY", "xai-secret"),
        ],
    )?;
    for (id, family) in [
        ("groq", Family::Chat),
        ("minimax", Family::Anthropic),
        ("xai", Family::Responses),
    ] {
        let Err(error) = set.provider(resolved(id, family, "preset-model")) else {
            return Err(io::Error::other(format!("{id} resolved without a key")).into());
        };
        assert!(
            matches!(&error, ProviderError::NoCredentials { provider } if provider == id),
            "{id}: {error:?}"
        );
        assert_eq!(error.fix(), Some(format!("Run dalgon login {id}.")));
        // No credential means no model-list request: the list is typed.
        let fetch = set.load_models(id).await?;
        assert_eq!(fetch.source, CatalogSource::Typed, "{id}");
        assert!(
            matches!(&fetch.live_error, Some(ProviderError::NoCredentials { provider }) if provider == id),
            "{id}: {:?}",
            fetch.live_error
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_signed_in_preset_key_outranks_the_environment_and_takes_no_oauth() -> TestResult {
    let dir = TestDir::new()?;
    let (paste, pasted) = oneshot::channel();
    paste
        .send(String::from("  deepseek-stored \n"))
        .map_err(|_| io::Error::other("the key receiver was dropped"))?;
    let (login_io, _progress) = LoginIo::channel(Some(pasted), CancellationToken::new());
    let site = LoginSite::new(
        dir.path().join("auth.json"),
        dir.path().join("cache"),
        "dal/test",
        &EnvSnapshot::test(&[]),
    )?;
    let signed_in = login("deepseek", Method::ApiKey, login_io, &site).await?;
    assert_eq!(
        signed_in,
        Credential::ApiKey {
            key: SecretString::from("deepseek-stored"),
        }
    );

    let (base, mut captured) = server("/v1", vec![stream_reply(chat_reply())]).await?;
    let toml = format!("[providers.deepseek]\nbase_url = '{base}'\n");
    let set = preset_set(&dir, &toml, &[("DEEPSEEK_API_KEY", "deepseek-env")])?;
    let head = say_hello(&set, "deepseek", Family::Chat, &mut captured).await?;
    assert!(
        head.contains("authorization: bearer deepseek-stored"),
        "{head}"
    );
    assert!(!head.contains("deepseek-env"), "{head}");

    let mut store = crate::auth::credential::AuthStore::load(dir.path().join("auth.json"))?;
    let oauth = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from("access"),
        refresh_token: SecretString::from("refresh"),
        expires_at: None,
        id_token: None,
        account_id: None,
    });
    assert!(matches!(
        store.set("deepseek", oauth),
        Err(ProviderError::AuthWrite { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn preset_model_lists_ask_with_the_header_set_of_their_family() -> TestResult {
    let dir = TestDir::new()?;
    let listing = |body: &str| Reply {
        status: 200,
        content_type: "application/json",
        body: body.as_bytes().to_vec(),
    };
    let (chat_base, mut chat_asked) = server(
        "/v1",
        vec![listing(r#"{"data":[{"id":"deepseek-listed"}]}"#)],
    )
    .await?;
    let (claude_base, mut claude_asked) = server(
        "/anthropic",
        vec![listing(
            r#"{"data":[{"id":"minimax-listed"}],"has_more":false}"#,
        )],
    )
    .await?;
    let toml = format!(
        "[providers.deepseek]\nbase_url = '{chat_base}'\n[providers.minimax]\nbase_url = '{claude_base}'\n"
    );
    let set = preset_set(
        &dir,
        &toml,
        &[
            ("DEEPSEEK_API_KEY", "deepseek-secret"),
            ("MINIMAX_API_KEY", "minimax-secret"),
        ],
    )?;

    let chat = set.load_models("deepseek").await?;
    assert_eq!(chat.source, CatalogSource::Live, "{:?}", chat.live_error);
    assert_eq!(chat.entries.len(), 1);
    assert_eq!(&*chat.entries[0].id, "deepseek-listed");
    let request = chat_asked.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&request[0]).to_ascii_lowercase();
    assert!(head.starts_with("get /v1/models "), "{head}");
    assert!(
        head.contains("authorization: bearer deepseek-secret"),
        "{head}"
    );
    assert!(!head.contains("\nanthropic-version:"), "{head}");

    let claude = set.load_models("minimax").await?;
    assert_eq!(
        claude.source,
        CatalogSource::Live,
        "{:?}",
        claude.live_error
    );
    assert_eq!(claude.entries.len(), 1);
    assert_eq!(&*claude.entries[0].id, "minimax-listed");
    let request = claude_asked
        .join_next()
        .await
        .expect("server completes")??;
    let head = String::from_utf8_lossy(&request[0]).to_ascii_lowercase();
    assert!(head.starts_with("get /anthropic/v1/models"), "{head}");
    assert!(
        head.contains("authorization: bearer minimax-secret"),
        "{head}"
    );
    assert!(head.contains("anthropic-version: 2023-06-01"), "{head}");
    assert!(!head.contains("\nx-api-key:"), "{head}");
    Ok(())
}

#[test]
fn preset_api_overrides_decode_like_the_named_provider_path() {
    // A preset with an explicit `api` decodes the way the named-provider
    // path did before the row existed: the named family wins, the new
    // family's header default applies, and the row keeps its endpoint
    // default, key material, and key-only sign-in.
    let config = Config::load(
        ConfigProduct::Dalgon,
        Path::new("/tmp/dal-provider-preset-test"),
        "",
        Some("[providers.deepseek]\napi = 'anthropic'\n"),
    )
    .expect("test dal.toml parses");
    let decoded =
        ProviderConfig::from_config(&config).expect("a preset keeps an explicit api override");
    let route = decoded
        .providers
        .iter()
        .find(|provider| &*provider.id == "deepseek")
        .expect("deepseek decoded");
    assert_eq!(route.family, Family::Anthropic);
    assert_eq!(route.auth, AuthStyle::XApiKey);
    assert_eq!(&*route.base_url, "https://api.deepseek.com");
    assert!(route.key_env.is_none());
    assert_eq!(route.def.map(|row| row.id), Some("deepseek"));
    assert_eq!(route.shape(), Shape::Claude);

    let refusal = |toml: &str| {
        let config = Config::load(
            ConfigProduct::Dalgon,
            Path::new("/tmp/dal-provider-preset-test"),
            "",
            Some(toml),
        )
        .expect("test dal.toml parses");
        ProviderConfig::from_config(&config)
            .expect_err("the provider table must be refused")
            .to_string()
    };
    assert_eq!(
        refusal("[providers.minimax]\ntransport = 'websocket'\n"),
        "providers.minimax: transport websocket needs api openai_responses."
    );
    assert_eq!(
        refusal("[providers.xai]\nbogus = 1\n"),
        "providers.xai.bogus: unknown key"
    );
}

fn api_key_credential(key: &str) -> Credential {
    Credential::ApiKey {
        key: SecretString::from(key),
    }
}

/// A stored OAuth login: the credential resolution finds before any key.
fn stored_login(access: &str) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(access),
        refresh_token: SecretString::from("stored-refresh"),
        expires_at: None,
        id_token: None,
        account_id: Some(String::from("acct-1")),
    })
}

/// A stored `WorkOS` device sign-in: this provider carries neither an ID token nor an account id.
fn stored_workos_login(access: &str) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(access),
        refresh_token: SecretString::from("stored-refresh"),
        expires_at: None,
        id_token: None,
        account_id: None,
    })
}

/// The error of a lowering that had to refuse, for wire types without `Debug`.
fn refusal<T>(result: Result<T, ProviderError>) -> ProviderError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("the wire was built for an entry that must refuse it"),
    }
}

#[test]
fn plain_shape_headers_emit_the_configured_api_key_style() {
    for (auth, name, value) in [
        (AuthStyle::Bearer, "authorization", "Bearer style-test-key"),
        (AuthStyle::XApiKey, "x-api-key", "style-test-key"),
        (AuthStyle::ApiKey, "api-key", "style-test-key"),
    ] {
        let routed = entry(
            "openai",
            Family::Chat,
            String::from("https://api.openai.com/v1"),
            None,
            4,
            Transport::Https,
        );
        let routed = ProviderEntry { auth, ..routed };
        let credential = api_key_credential("style-test-key");
        let headers = routed
            .shape()
            .headers(&routed, &credential)
            .expect("a key header");
        assert_eq!(
            headers,
            [(String::from(name), String::from(value))],
            "{auth:?} sends its own header"
        );
    }
}

#[test]
fn none_auth_entries_never_carry_a_credential_header() {
    let none_entry = |id: &str, family: Family| ProviderEntry {
        auth: AuthStyle::None,
        ..entry(
            id,
            family,
            String::from("http://localhost:11434"),
            None,
            4,
            Transport::Https,
        )
    };
    let no_credential = Credential::None;
    let api_key = api_key_credential("unused-key");
    // A stored login is what credential resolution finds first, so it is the
    // credential most likely to reach a `none` entry.
    let oauth = stored_login("stored-access");

    // Every header shape refuses both held credential kinds with one fixed
    // text: Plain (Ollama), Claude (the anthropic row), Codex (the codex row).
    for (id, family) in [
        ("local-ollama", Family::Ollama),
        ("anthropic", Family::Anthropic),
        ("openai-codex", Family::Codex),
    ] {
        let routed = none_entry(id, family);
        let expected = format!(
            "invalid request: {id} sends no credential, so it cannot send an API key or an OAuth login"
        );
        for held in [&api_key, &oauth] {
            let error = routed.shape().headers(&routed, held).unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }

    // No credential means no header: Plain adds nothing, Claude adds only its
    // version marker (no OAuth betas), and Codex, which cannot run without a
    // login, keeps its NoCredentials refusal.
    let plain = none_entry("local-ollama", Family::Ollama);
    let headers = plain.shape().headers(&plain, &no_credential).unwrap();
    assert_eq!(headers, [] as [(String, String); 0]);
    let claude = none_entry("anthropic", Family::Anthropic);
    let headers = claude.shape().headers(&claude, &no_credential).unwrap();
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].0, "anthropic-version");
    let codex = none_entry("openai-codex", Family::Codex);
    let error = codex.shape().headers(&codex, &no_credential).unwrap_err();
    assert!(matches!(error, ProviderError::NoCredentials { .. }));

    // The refusal belongs to `none` alone: a credentialed style still needs a
    // credential and still sends its own header.
    let keyed = ProviderEntry {
        auth: AuthStyle::XApiKey,
        ..plain
    };
    let error = keyed.shape().headers(&keyed, &no_credential).unwrap_err();
    assert!(matches!(error, ProviderError::NoCredentials { .. }));
    let headers = keyed.shape().headers(&keyed, &oauth).unwrap();
    assert_eq!(headers[0].0, "authorization");
    assert_eq!(headers[0].1, "Bearer stored-access");
}

#[test]
fn anthropic_wire_refuses_styles_it_has_no_header_for_whatever_secret_is_held() {
    use crate::thinking::{AnthropicThinking, ThinkingPlan, WireThinking};

    let model = "claude-test";
    let request = request(Family::Anthropic, model, vec![user_text("hello")]);
    let catalog = resolved("anthropic", Family::Anthropic, model).entry;
    let thinking = ThinkingPlan {
        level: ThinkingLevel::Off,
        wire: WireThinking::Anthropic {
            thinking: AnthropicThinking::Omit,
            effort: None,
        },
        temperature: None,
        notices: Vec::new(),
    };
    let api_key = api_key_credential("anthropic-key");
    let oauth = stored_login("stored-access");
    let build = |style: AuthStyle, held: &Credential| {
        super::request::build_anthropic_wire(&request, &catalog, style, &thinking, held, false)
    };

    // The wire has no `api-key` header form, and `none` sends no credential.
    // Both are refused before the credential kind is looked at, so a stored
    // OAuth login cannot slip through to the Claude OAuth wire. The refusal
    // text is not pinned, but it must never echo a secret.
    for (style, held) in [
        (AuthStyle::ApiKey, &oauth),
        (AuthStyle::ApiKey, &api_key),
        (AuthStyle::None, &oauth),
        (AuthStyle::None, &api_key),
    ] {
        let error = refusal(build(style, held));
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{style:?}"
        );
        let text = error.to_string();
        assert!(!text.contains("stored-access"));
        assert!(!text.contains("anthropic-key"));
    }

    // Supported styles keep building: an OAuth login is still lowered to the
    // Claude OAuth wire, the only one that carries a user-agent override.
    for style in [AuthStyle::XApiKey, AuthStyle::Bearer] {
        let wire = build(style, &oauth).map_err(|error| error.to_string());
        let wire = wire.expect("a supported style builds");
        assert!(wire.user_agent.is_some());
    }
}

#[test]
fn responses_wire_refuses_secrets_it_cannot_carry_and_keeps_keyless_https() {
    use crate::{
        family::responses::{self, ResponsesRequest},
        thinking::WireThinking,
    };

    let request = request(Family::Responses, "gpt-test", vec![user_text("hello")]);
    let api_key = api_key_credential("responses-key");
    let oauth = stored_login("stored-access");
    let build = |style: AuthStyle, held: &Credential| {
        responses::wire(&ResponsesRequest {
            request: &request,
            thinking: WireThinking::OpenAi { effort: None },
            reasoning_summary: false,
            auth: style,
            credential: held,
            session_id: SessionId::new_v7(),
            user_agent: "dalgon/test",
        })
    };

    // `none` sends no credential, and an OAuth login always rides a bearer
    // header, which an `api-key` entry cannot carry. Each is refused before
    // the credential kind is looked at, without echoing a secret.
    for (style, held) in [
        (AuthStyle::None, &oauth),
        (AuthStyle::None, &api_key),
        (AuthStyle::ApiKey, &oauth),
    ] {
        let error = refusal(build(style, held));
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{style:?}"
        );
        let text = error.to_string();
        assert!(!text.contains("stored-access"));
        assert!(!text.contains("responses-key"));
    }

    // Existing behavior is preserved: keyless HTTPS sends no header, an API
    // key keeps its configured header, and an OAuth login stays a bearer
    // token that the lifecycle may refresh.
    let keyless = build(AuthStyle::None, &Credential::None).map_err(|error| error.to_string());
    let keyless = keyless.expect("a keyless none entry builds");
    assert!(keyless.auth_header.is_none());
    assert!(!keyless.oauth);
    let keyed = build(AuthStyle::XApiKey, &api_key).map_err(|error| error.to_string());
    let keyed = keyed.expect("an API key still builds");
    assert_eq!(
        keyed.auth_header,
        Some(("x-api-key", String::from("responses-key")))
    );
    let login = build(AuthStyle::Bearer, &oauth).map_err(|error| error.to_string());
    let login = login.expect("an OAuth login still builds");
    assert!(login.oauth);
    assert_eq!(
        login.auth_header,
        Some(("authorization", String::from("Bearer stored-access")))
    );
}

#[test]
fn family_labels_are_non_empty_and_stable_for_every_variant() {
    // Every Family variant resolves through family_label; the only shared
    // label is the documented Chat|Responses "openai" pair. The exhaustive
    // match below breaks when a new variant joins, forcing this list and
    // error.rs to move together.
    let families = [
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
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for family in families {
        let label = crate::error::family_label(family);
        assert!(!label.is_empty(), "{family:?} has an empty label");
        assert!(
            label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
            "{family:?} label {label:?} is not stable lowercase"
        );
        let expected = match family {
            Family::Chat | Family::Responses => "openai",
            Family::Codex => "codex",
            Family::Anthropic => "anthropic",
            Family::Bedrock => "bedrock",
            Family::Cursor => "cursor",
            Family::Devin => "devin",
            Family::Droid => "droid",
            Family::GitLab => "gitlab",
            Family::GoogleGenerativeAi => "google-generative-ai",
            Family::GoogleVertex => "google-vertex",
            Family::GoogleGeminiCli => "google-gemini-cli",
            Family::GoogleAntigravity => "google-antigravity",
            Family::Mistral => "mistral",
            Family::Ollama => "ollama",
            Family::PiMessages => "pi-messages",
        };
        assert_eq!(label, expected, "{family:?}");
        *seen.entry(label).or_insert(0) += 1;
    }
    for (label, count) in &seen {
        if *label == "openai" {
            assert_eq!(*count, 2, "the openai label covers Chat and Responses only");
        } else {
            assert_eq!(*count, 1, "label {label:?} must be unique");
        }
    }
    assert_eq!(seen.len(), 15, "sixteen variants share fifteen labels");
}
fn devin_discovery_reply(uid: &str, router: bool) -> Vec<u8> {
    GetCliModelConfigsResponse {
        client_model_configs: vec![ClientModelConfig {
            label: String::from(uid),
            model_uid: String::from(uid),
            model_info: Some(ModelInfo {
                is_model_router: router,
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }
    .encode_to_vec()
}

fn decode_chat_body(captured: &[u8]) -> Result<GetChatMessageRequest, Box<dyn Error>> {
    let body = request_body(captured);
    assert!(body.len() > 5, "the chat call carries a framed body");
    assert_eq!(
        body[0], 1,
        "the chat call rides one compressed Connect frame"
    );
    let len = u32::from_be_bytes(
        body[1..5]
            .try_into()
            .map_err(|_| io::Error::other("the chat frame carries its big-endian length"))?,
    ) as usize;
    assert_eq!(len, body.len() - 5);
    let mut gz = flate2::read::GzDecoder::new(&body[5..]);
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut gz, &mut raw)?;
    Ok(GetChatMessageRequest::decode(raw.as_slice())?)
}

#[tokio::test]
async fn ollama_ndjson_completes_keyless_and_releases_admission() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/loop", vec![stream_reply(ollama_reply())]).await?;
    let mut ollama_entry = entry(
        "local-ollama",
        Family::Ollama,
        format!("{base}/api"),
        None,
        1,
        Transport::Https,
    );
    ollama_entry.auth = AuthStyle::None;
    let set = ProviderSet::new(
        &config(vec![ollama_entry], 0),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("local-ollama", Family::Ollama, "llama-test"))?;
    let request = request(Family::Ollama, "llama-test", vec![user_text("Say hello.")]);
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(
        events.iter().find_map(|event| match event {
            StreamEvent::Usage { usage } => Some(usage),
            _ => None,
        }),
        Some(usage) if usage.input_tokens == 10 && usage.output_tokens == 5
    ));
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    // The configured `/api` suffix is trimmed before joining, never doubled.
    assert!(head.starts_with("post /loop/api/chat "), "{head}");
    assert!(!head.contains("authorization:"), "{head}");
    assert!(
        String::from_utf8_lossy(request_body(&captured[0])).contains("\"model\":\"llama-test\"")
    );
    assert_eq!(
        set.inner
            .providers
            .get("local-ollama")
            .map(|slot| slot.permits.available_permits()),
        Some(1)
    );
    Ok(())
}

fn ollama_reasoning_history() -> ContextItem {
    ContextItem::Assistant {
        source: dal_core::ReplaySource {
            family: Family::Ollama,
            model: "gpt-oss:120b".into(),
        },
        replay: None,
        parts: vec![
            dal_core::AssistantPart::Thinking {
                text: "hidden".into(),
                replay: None,
            },
            dal_core::AssistantPart::Text {
                text: "Checking.".into(),
                replay: None,
            },
        ],
    }
}

/// Sends one Ollama request with a 70,000-token limit and a reasoning-bearing
/// history through a loopback row, and returns the head and body that crossed
/// the wire.
async fn ollama_row_wire(
    id: &str,
    key_env: Option<&str>,
) -> Result<(String, String), Box<dyn Error>> {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/loop", vec![stream_reply(ollama_reply())]).await?;
    let mut row = entry(id, Family::Ollama, base, key_env, 1, Transport::Https);
    if key_env.is_none() {
        row.auth = AuthStyle::None;
    }
    let set = ProviderSet::new(
        &config(vec![row], 0),
        identity(),
        EnvSnapshot::test(&[("OLLAMA_ROW_KEY", "row-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved(id, Family::Ollama, "gpt-oss:120b"))?;
    let mut ask = request(
        Family::Ollama,
        "gpt-oss:120b",
        vec![ollama_reasoning_history(), user_text("Say hello.")],
    );
    ask.params.max_output_tokens = Some(70_000);
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &ask,
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?,
    )
    .await?;
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    let body = String::from_utf8_lossy(request_body(&captured[0])).into_owned();
    Ok((head, body))
}

#[tokio::test]
async fn the_cloud_row_holds_num_predict_to_its_ceiling_and_drops_reasoning_history() -> TestResult
{
    let (head, body) = ollama_row_wire("ollama-cloud", Some("OLLAMA_ROW_KEY")).await?;
    assert!(head.starts_with("post /loop/api/chat "), "{head}");
    assert!(head.contains("authorization: bearer row-secret"), "{head}");
    assert!(
        body.contains(r#""options":{"num_predict":65536}"#),
        "{body}"
    );
    assert!(!body.contains(r#""thinking""#), "{body}");
    assert!(body.contains(r#""content":"Checking.""#), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_self_hosted_row_sends_the_caller_limit_and_replays_reasoning() -> TestResult {
    let (head, body) = ollama_row_wire("local-ollama", None).await?;
    assert!(!head.contains("authorization:"), "{head}");
    assert!(
        body.contains(r#""options":{"num_predict":70000}"#),
        "{body}"
    );
    assert!(
        body.contains(r#""content":"Checking.","thinking":"hidden""#),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn ollama_native_reasoning_and_tool_call_events_stream_through_the_provider() -> TestResult {
    let dir = TestDir::new()?;
    let wire = [
        r#"{"message":{"role":"assistant","thinking":"Let me "},"done":false}"#,
        r#"{"message":{"role":"assistant","thinking":"think."},"done":false}"#,
        r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"read","arguments":{"path":  "a.txt"}}}]},"done":false}"#,
        r#"{"done":true,"done_reason":"stop","prompt_eval_count":12,"prompt_eval_cached_count":4,"eval_count":7}"#,
    ]
    .join("\n")
        + "\n";
    let (base, mut server_task) = server("/loop", vec![stream_reply(wire)]).await?;
    let mut row = entry(
        "local-ollama",
        Family::Ollama,
        base,
        None,
        1,
        Transport::Https,
    );
    row.auth = AuthStyle::None;
    let set = ProviderSet::new(
        &config(vec![row], 0),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("local-ollama", Family::Ollama, "gpt-oss:20b"))?;
    let ask = request(Family::Ollama, "gpt-oss:20b", vec![user_text("Read it.")]);
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &ask,
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?,
    )
    .await?;
    server_task.join_next().await.expect("server completes")??;
    let reasoning: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ReasoningDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "Let me think.");
    let calls = events.iter().find_map(|event| match event {
        StreamEvent::ToolCallsDone { calls } => Some(calls),
        _ => None,
    });
    let [call] = calls.map(Vec::as_slice).unwrap_or_default() else {
        return Err(io::Error::other(format!("expected one call: {events:?}")).into());
    };
    // Ollama provides no call id; the arguments keep their raw bytes.
    assert_eq!(
        (call.id.as_str(), call.name.as_str()),
        ("ollama:0:read", "read")
    );
    assert!(
        matches!(&call.args, crate::stream::ToolArgs::Parsed(raw) if raw.as_str() == r#"{"path":  "a.txt"}"#),
        "{:?}",
        call.args
    );
    let usage = events.iter().find_map(|event| match event {
        StreamEvent::Usage { usage } => Some(usage),
        _ => None,
    });
    assert!(matches!(
        usage,
        Some(usage) if usage.input_tokens == 12 && usage.cached_input_tokens == 4 && usage.output_tokens == 7
    ));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: crate::stream::StopReason::ToolUse
        })
    ));
    Ok(())
}

#[tokio::test]
async fn a_keyless_llamacpp_row_streams_chat_without_an_authorization_header() -> TestResult {
    // The documented local setup: the built-in row with `auth = "none"`. The
    // Chat request builder used to demand a header for every row and refused
    // this one with "credential-less entries carry no auth header".
    let dir = TestDir::new()?;
    let (base, mut captured) = server("/v1", vec![stream_reply(chat_reply())]).await?;
    let toml = format!("[providers.llamacpp]\nbase_url = '{base}'\nauth = 'none'\n");
    let set = preset_set(&dir, &toml, &[])?;
    let head = say_hello(&set, "llamacpp", Family::Chat, &mut captured).await?;
    assert!(head.starts_with("post /v1/chat/completions "), "{head}");
    for header in ["authorization:", "x-api-key:", "api-key:"] {
        assert!(!head.contains(header), "{header} reached the wire: {head}");
    }
    Ok(())
}

#[tokio::test]
async fn mistral_conversations_stream_over_sse() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(mistral_reply())]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "mistral-test",
                Family::Mistral,
                base,
                Some("MISTRAL_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("MISTRAL_TEST_KEY", "mistral-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("mistral-test", Family::Mistral, "mistral-test"))?;
    let request = request(
        Family::Mistral,
        "mistral-test",
        vec![user_text("Say hello.")],
    );
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(
        events.iter().find_map(|event| match event {
            StreamEvent::Usage { usage } => Some(usage),
            _ => None,
        }),
        Some(usage) if usage.input_tokens == 7 && usage.output_tokens == 3
    ));
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(head.starts_with("post /v1/conversations "), "{head}");
    assert!(
        head.contains("authorization: bearer mistral-secret"),
        "{head}"
    );
    Ok(())
}

#[tokio::test]
async fn mistral_cancelled_before_start_sends_nothing() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(mistral_reply())]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "mistral-test",
                Family::Mistral,
                base,
                Some("MISTRAL_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("MISTRAL_TEST_KEY", "mistral-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("mistral-test", Family::Mistral, "mistral-test"))?;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = provider
        .open(
            SessionId::new_v7(),
            &request(
                Family::Mistral,
                "mistral-test",
                vec![user_text("Say hello.")],
            ),
            &[],
            notice_sink(),
            &cancel,
        )
        .await;
    assert!(
        matches!(result, Err(ProviderError::Transport { .. })),
        "{result:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
            .await
            .is_err()
    );
    server_task.abort_all();
    Ok(())
}

/// A conversations stream whose reasoning arrives as two signed chunks, each
/// in its own event with object-valued content (the official Conversations
/// event shape), ahead of the visible answer.
fn mistral_signed_reply() -> String {
    let mut body = String::new();
    for data in [
        r#"{"type":"conversation.response.started","conversation_id":"conv-1"}"#,
        r#"{"type":"message.output.delta","output_index":0,"id":"e1","content":[{"type":"thinking","thinking":[{"type":"text","text":"alpha "}],"signature":"SIG-A"}]}"#,
        r#"{"type":"message.output.delta","output_index":0,"id":"e2","content":[{"type":"thinking","thinking":[{"type":"text","text":"beta"}],"signature":"SIG-B"}]}"#,
        r#"{"type":"message.output.delta","output_index":0,"id":"e3","content":"Answer."}"#,
        r#"{"type":"conversation.response.done","usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
    ] {
        body.push_str("data: ");
        body.push_str(data);
        body.push_str("\n\n");
    }
    body
}

fn mistral_set(dir: &TestDir, base: String) -> Result<ProviderSet, Box<dyn Error>> {
    Ok(ProviderSet::new(
        &config(
            vec![entry(
                "mistral-test",
                Family::Mistral,
                base,
                Some("MISTRAL_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("MISTRAL_TEST_KEY", "mistral-secret")]),
        dir.path(),
        dir.path(),
    )?)
}

#[tokio::test]
async fn mistral_signed_reasoning_round_trips_through_the_http_adapter() -> TestResult {
    use dal_core::{AssistantPart, ReplaySource};

    let dir = TestDir::new()?;
    let (base, mut server_task) = server(
        "/v1",
        vec![
            stream_reply(mistral_signed_reply()),
            stream_reply(mistral_reply()),
            stream_reply(mistral_reply()),
        ],
    )
    .await?;
    let set = mistral_set(&dir, base)?;
    let provider = set.provider(resolved("mistral-test", Family::Mistral, "mistral-test"))?;
    let cancel = CancellationToken::new();

    // Turn one: the signed stream, decoded off the wire.
    let mut first = request(Family::Mistral, "mistral-test", vec![user_text("Think.")]);
    first.params.thinking = ThinkingLevel::Medium;
    let events = collect(
        provider
            .open(SessionId::new_v7(), &first, &[], notice_sink(), &cancel)
            .await?,
    )
    .await?;
    let replay = events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Replay { payload } => Some(payload.item.clone()),
            _ => None,
        })
        .expect("the signed stream carries one replay item");
    let reasoning: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ReasoningDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "alpha beta");
    let parts = vec![
        AssistantPart::Thinking {
            text: reasoning.into(),
            replay: Some(replay),
        },
        AssistantPart::Text {
            text: "Answer.".into(),
            replay: None,
        },
    ];

    // Turns two and three: the recorded answer, produced by this model and by
    // another one.
    for producer in ["mistral-test", "mistral-other"] {
        let mut next = request(
            Family::Mistral,
            "mistral-test",
            vec![
                user_text("Think."),
                ContextItem::Assistant {
                    source: ReplaySource {
                        family: Family::Mistral,
                        model: producer.into(),
                    },
                    parts: parts.clone(),
                    replay: None,
                },
                user_text("More."),
            ],
        );
        next.params.thinking = ThinkingLevel::Medium;
        collect(
            provider
                .open(SessionId::new_v7(), &next, &[], notice_sink(), &cancel)
                .await?,
        )
        .await?;
    }

    let captured = server_task.join_next().await.expect("server completes")??;
    let body = |index: usize| String::from_utf8_lossy(request_body(&captured[index])).into_owned();
    assert!(!body(0).contains("signature"), "{}", body(0));
    let signed = body(1);
    assert!(
        signed.contains(r#""reasoning_effort":"medium""#),
        "{signed}"
    );
    let chunks = concat!(
        r#""content":[{"type":"thinking","thinking":[{"type":"text","text":"alpha "}],"signature":"SIG-A"},"#,
        r#"{"type":"thinking","thinking":[{"type":"text","text":"beta"}],"signature":"SIG-B"},"#,
        r#"{"type":"text","text":"Answer."}]"#,
    );
    assert!(signed.contains(chunks), "{signed}");
    let foreign = body(2);
    let text_only = r#"{"type":"thinking","thinking":[{"type":"text","text":"alpha beta"}]}"#;
    assert!(foreign.contains(text_only), "{foreign}");
    assert!(!foreign.contains("SIG-"), "{foreign}");
    Ok(())
}

fn pi_messages_reply() -> String {
    String::from(
        "data: {\"type\":\"start\"}\n\ndata: {\"type\":\"text_start\",\"contentIndex\":0}\n\ndata: {\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"Hello\"}\n\ndata: {\"type\":\"text_end\",\"contentIndex\":0,\"content\":\"Hello\"}\n\ndata: {\"type\":\"done\",\"reason\":\"stop\",\"usage\":{\"input\":7,\"output\":3,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":10,\"cost\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0}}}\n\n",
    )
}

fn pi_messages_set(dir: &TestDir, base: String) -> Result<ProviderSet, Box<dyn Error>> {
    Ok(ProviderSet::new(
        &config(
            vec![entry(
                "pi-test",
                Family::PiMessages,
                base,
                Some("PI_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("PI_TEST_KEY", "pi-secret")]),
        dir.path(),
        dir.path(),
    )?)
}

#[tokio::test]
async fn pi_messages_stream_over_sse_with_a_bearer_key() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(pi_messages_reply())]).await?;
    let set = pi_messages_set(&dir, base)?;
    let provider = set.provider(resolved("pi-test", Family::PiMessages, "balanced"))?;
    let request = request(
        Family::PiMessages,
        "balanced",
        vec![user_text("Say hello.")],
    );
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(
        events.iter().find_map(|event| match event {
            StreamEvent::Usage { usage } => Some(usage),
            _ => None,
        }),
        Some(usage) if usage.input_tokens == 7 && usage.output_tokens == 3
    ));
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(head.starts_with("post /v1/messages "), "{head}");
    assert!(head.contains("authorization: bearer pi-secret"), "{head}");
    assert!(head.contains("accept: text/event-stream"), "{head}");
    assert!(head.contains("content-type: application/json"), "{head}");
    let body = String::from_utf8_lossy(request_body(&captured[0])).into_owned();
    assert!(
        body.starts_with(r#"{"model":"balanced","context":{"messages":["#),
        "{body}"
    );
    assert!(
        body.contains(r#""role":"system","content":"Follow the request.""#),
        "{body}"
    );
    assert!(
        body.contains(r#""role":"user","content":[{"type":"text","text":"Say hello."}]"#),
        "{body}"
    );
    assert!(body.contains(r#""sessionId":"session-cache:1""#), "{body}");
    Ok(())
}

#[tokio::test]
async fn pi_messages_tool_turn_restores_internal_tool_names() -> TestResult {
    let dir = TestDir::new()?;
    let reply = concat!(
        "data: {\"type\":\"toolcall_start\",\"contentIndex\":0,\"id\":\"call_1\",\"toolName\":\"mcp_search\"}\n\n",
        "data: {\"type\":\"toolcall_delta\",\"contentIndex\":0,\"delta\":\"{\\\"q\\\":\"}\n\n",
        "data: {\"type\":\"toolcall_delta\",\"contentIndex\":0,\"delta\":\"\\\"cats\\\"}\"}\n\n",
        "data: {\"type\":\"toolcall_end\",\"contentIndex\":0,\"toolCall\":{\"type\":\"toolCall\",\"id\":\"call_1\",\"name\":\"mcp_search\",\"arguments\":{\"q\":\"cats\"}}}\n\n",
        "data: {\"type\":\"done\",\"reason\":\"toolUse\",\"usage\":{\"input\":1,\"output\":1,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":2,\"cost\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0}}}\n\n",
    );
    // The internal name holds a dot, which the wire alphabet refuses; the
    // gateway echoes the wire name, and the stream restores the internal one.
    let wire =
        crate::tool_names::wire_name("mcp.search", crate::tool_names::WIRE_NAME_MAX).into_owned();
    assert_ne!(wire, "mcp.search");
    let (base, mut server_task) = server(
        "/v1",
        vec![stream_reply(reply.replace("mcp_search", &wire))],
    )
    .await?;
    let set = pi_messages_set(&dir, base)?;
    let provider = set.provider(resolved("pi-test", Family::PiMessages, "balanced"))?;
    let mut request = request(
        Family::PiMessages,
        "balanced",
        vec![user_text("Find cats.")],
    );
    request.tools = Arc::<[ModelToolSpec]>::from(vec![ModelToolSpec {
        name: "mcp.search".into(),
        description: "Search.".into(),
        parameters: RawJson::parse(r#"{"type":"object"}"#)?,
        grammar: None,
        defer_loading: false,
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
    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::ToolCallStarted { id, name } if id == "call_1" && name == "mcp.search"
        )),
        "{events:?}"
    );
    let calls = events.iter().find_map(|event| match event {
        StreamEvent::ToolCallsDone { calls } => Some(calls),
        _ => None,
    });
    assert!(
        matches!(calls, Some(calls) if calls.len() == 1 && calls[0].name == "mcp.search"),
        "{calls:?}"
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: crate::stream::StopReason::ToolUse
        })
    ));
    let captured = server_task.join_next().await.expect("server completes")??;
    let body = String::from_utf8_lossy(request_body(&captured[0])).into_owned();
    assert!(
        body.contains(&format!(r#""toolsAdded":[{{"name":"{wire}""#)),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn pi_messages_http_error_reports_the_gateway_message() -> TestResult {
    let dir = TestDir::new()?;
    let (base, _server) = server(
        "/v1",
        vec![Reply {
            status: 400,
            content_type: "application/json",
            body: String::from(
                r#"{"error":{"message":"unknown model","code":"model_not_found","details":{"x":1}}}"#,
            )
            .into_bytes(),
        }],
    )
    .await?;
    let set = pi_messages_set(&dir, base)?;
    let text = open_error_text(
        &set,
        "pi-test",
        Family::PiMessages,
        "balanced",
        vec![user_text("Say hello.")],
    )
    .await;
    assert_eq!(text, "invalid request: unknown model");
    Ok(())
}

#[tokio::test]
async fn pi_messages_error_event_ends_the_stream_with_its_message() -> TestResult {
    let dir = TestDir::new()?;
    let reply = "data: {\"type\":\"text_start\",\"contentIndex\":0}\n\ndata: {\"type\":\"error\",\"reason\":\"error\",\"errorMessage\":\"upstream failed\",\"usage\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":0,\"cost\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0}}}\n\n";
    let (base, _server) = server("/v1", vec![stream_reply(String::from(reply))]).await?;
    let set = pi_messages_set(&dir, base)?;
    let text = open_error_text(
        &set,
        "pi-test",
        Family::PiMessages,
        "balanced",
        vec![user_text("Say hello.")],
    )
    .await;
    assert_eq!(text, "pi-messages error 200: upstream failed");
    Ok(())
}

#[tokio::test]
async fn pi_messages_cancelled_before_start_sends_nothing() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(pi_messages_reply())]).await?;
    let set = pi_messages_set(&dir, base)?;
    let provider = set.provider(resolved("pi-test", Family::PiMessages, "balanced"))?;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = provider
        .open(
            SessionId::new_v7(),
            &request(
                Family::PiMessages,
                "balanced",
                vec![user_text("Say hello.")],
            ),
            &[],
            notice_sink(),
            &cancel,
        )
        .await;
    assert!(
        matches!(result, Err(ProviderError::Transport { .. })),
        "{result:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
            .await
            .is_err()
    );
    server_task.abort_all();
    Ok(())
}

#[tokio::test]
async fn google_generative_ai_streams_with_api_key_header() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(google_reply())]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "gemini-test",
                Family::GoogleGenerativeAi,
                base,
                Some("GEMINI_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("GEMINI_TEST_KEY", "gemini-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved(
        "gemini-test",
        Family::GoogleGenerativeAi,
        "gem-test",
    ))?;
    let request = request(
        Family::GoogleGenerativeAi,
        "gem-test",
        vec![user_text("Say hello.")],
    );
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(
        events.iter().find_map(|event| match event {
            StreamEvent::Usage { usage } => Some(usage),
            _ => None,
        }),
        Some(usage)
            if usage.input_tokens == 3
                && usage.output_tokens == 4
                && usage.reasoning_tokens == Some(1)
    ));
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(
        head.starts_with("post /v1/models/gem-test:streamgeneratecontent?alt=sse "),
        "{head}"
    );
    assert!(head.contains("x-goog-api-key: gemini-secret"), "{head}");
    Ok(())
}

#[tokio::test]
async fn google_vertex_without_project_location_refuses_before_any_request() -> TestResult {
    // The Vertex adapter always overrides the base with its location host,
    // so a loopback stream cannot prove the success path here; the dispatch
    // proves the settings gate instead: no project/location means no
    // request leaves at all.
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("", vec![stream_reply(google_reply())]).await?;
    let vertex_entry = entry(
        "factory-droid",
        Family::GoogleVertex,
        base,
        None,
        1,
        Transport::Https,
    );
    stored_oauth(&dir, "factory-droid", "ya29-test")?;
    let set = ProviderSet::new(
        &config(vec![vertex_entry], 0),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("factory-droid", Family::GoogleVertex, "v-test"))?;
    let stream = provider
        .open(
            SessionId::new_v7(),
            &request(
                Family::GoogleVertex,
                "v-test",
                vec![user_text("Say hello.")],
            ),
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    let events = collect(stream).await;
    assert!(
        matches!(&events, Err(ProviderError::InvalidRequest { .. })),
        "{events:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
            .await
            .is_err()
    );
    server_task.abort_all();
    Ok(())
}

#[tokio::test]
async fn google_antigravity_posts_the_cca_envelope() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
    let mut antigravity_entry = entry(
        "factory-droid",
        Family::GoogleAntigravity,
        base,
        None,
        1,
        Transport::Https,
    );
    antigravity_entry.settings.project = Some("cca-proj".into());
    stored_oauth(&dir, "factory-droid", "ya29-test")?;
    let set = ProviderSet::new(
        &config(vec![antigravity_entry], 0),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved(
        "factory-droid",
        Family::GoogleAntigravity,
        "gemini-3-flash-agent",
    ))?;
    let request = request(
        Family::GoogleAntigravity,
        "gemini-3-flash-agent",
        vec![user_text("Say hello.")],
    );
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hi"))
    );
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(
        head.starts_with("post /v1internal:streamgeneratecontent?alt=sse "),
        "{head}"
    );
    assert!(head.contains("authorization: bearer ya29-test"), "{head}");
    assert!(head.contains("user-agent: antigravity/hub/"), "{head}");
    assert!(
        String::from_utf8_lossy(request_body(&captured[0])).contains("cca-proj"),
        "the envelope names the configured project"
    );
    Ok(())
}

/// The stream context of one Antigravity turn, built directly so the
/// failover loop can run over loopback endpoints (a configured base is a
/// single candidate, and the unconfigured pair names real Google hosts).
fn antigravity_failover_context() -> super::request::StreamContext {
    let model = "gemini-3-flash-agent";
    super::request::StreamContext {
        family: Family::GoogleAntigravity,
        provider: "google-antigravity".into(),
        base_url: "".into(),
        auth: AuthStyle::None,
        model: model.into(),
        entry: resolved("google-antigravity", Family::GoogleAntigravity, model).entry,
        request: request(
            Family::GoogleAntigravity,
            model,
            vec![user_text("Say hello.")],
        ),
        thinking: crate::thinking::ThinkingPlan {
            level: ThinkingLevel::Off,
            wire: crate::thinking::WireThinking::OpenAi { effort: None },
            temperature: None,
            notices: Vec::new(),
        },
        session: SessionId::new_v7(),
        client: reqwest::Client::new(),
        user_agent: "dalgon/test".into(),
    }
}

/// The Cloud Code Assist wire the failover loop re-sends to each endpoint;
/// the loop supplies the base.
fn cca_failover_wire() -> crate::family::google::shared::GoogleWire {
    crate::family::google::shared::GoogleWire {
        path: String::from("v1internal:streamGenerateContent?alt=sse"),
        base_override: None,
        headers: vec![("authorization", String::from("Bearer ya29-test"))],
        user_agent: None,
        body: br#"{"request":{}}"#.to_vec(),
        fallback: None,
    }
}

async fn run_antigravity_failover(
    bases: &[String],
    cancel: &CancellationToken,
) -> Result<Option<EventStream>, crate::lifecycle::AttemptFailure> {
    let context = antigravity_failover_context();
    let slot = crate::family::google::shared::ResponseIdSlot::default();
    super::request::antigravity_failover(
        &context,
        &[],
        cancel,
        bases,
        || Ok(cca_failover_wire()),
        &slot,
    )
    .await
}

fn json_reply(status: u16, body: &str) -> Reply {
    Reply {
        status,
        content_type: "application/json",
        body: body.as_bytes().to_vec(),
    }
}

const CCA_RATE_LIMITED: &str =
    r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"slow down"}}"#;
const CCA_QUOTA_EXHAUSTED: &str = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"quota will reset at midnight","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"QUOTA_EXHAUSTED"}]}}"#;

/// Whether the loopback server behind `task` was never contacted.
async fn never_contacted(task: &mut tokio::task::JoinSet<io::Result<Vec<Vec<u8>>>>) -> bool {
    tokio::time::timeout(Duration::from_millis(50), task.join_next())
        .await
        .is_err()
}

#[tokio::test]
async fn antigravity_fails_over_on_a_transient_status_before_any_stream_byte() -> TestResult {
    for (status, body) in [
        (408, "{}"),
        (429, CCA_RATE_LIMITED),
        (500, "{}"),
        (503, "{}"),
    ] {
        let (first, mut first_task) = server("", vec![json_reply(status, body)]).await?;
        let (second, mut second_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
        let result = run_antigravity_failover(&[first, second], &CancellationToken::new()).await;
        let Ok(Some(stream)) = result else {
            return Err(io::Error::other(format!(
                "{status} did not fail over: {}",
                match result {
                    Ok(_) => String::from("cancelled"),
                    Err(failure) => format!("{failure:?}"),
                }
            ))
            .into());
        };
        let events = collect(stream).await?;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hi")),
            "{status}: the second endpoint's stream was not served: {events:?}"
        );
        first_task.join_next().await.expect("first answered")??;
        second_task.join_next().await.expect("second answered")??;
    }
    Ok(())
}

#[tokio::test]
async fn antigravity_returns_the_last_endpoints_failure_to_the_lifecycle() -> TestResult {
    let (first, mut first_task) = server("", vec![json_reply(503, "{}")]).await?;
    let (second, mut second_task) = server("", vec![json_reply(500, "{}")]).await?;
    let result = run_antigravity_failover(&[first, second], &CancellationToken::new()).await;
    assert!(
        matches!(
            result,
            Err(crate::lifecycle::AttemptFailure::Response { status: 500, .. })
        ),
        "the last endpoint's own failure is the attempt's failure"
    );
    first_task.join_next().await.expect("first answered")??;
    second_task.join_next().await.expect("second answered")??;
    Ok(())
}

#[tokio::test]
async fn antigravity_terminal_quota_never_fails_over() -> TestResult {
    let (first, mut first_task) = server("", vec![json_reply(429, CCA_QUOTA_EXHAUSTED)]).await?;
    let (second, mut second_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
    let result = run_antigravity_failover(&[first, second], &CancellationToken::new()).await;
    assert!(
        matches!(
            result,
            Err(crate::lifecycle::AttemptFailure::Provider(
                ProviderError::Quota { .. }
            ))
        ),
        "terminal quota is the attempt's failure"
    );
    first_task.join_next().await.expect("first answered")??;
    assert!(never_contacted(&mut second_task).await, "quota failed over");
    second_task.abort_all();
    Ok(())
}

#[tokio::test]
async fn antigravity_client_errors_never_fail_over() -> TestResult {
    for status in [400, 401, 403, 404] {
        let (first, mut first_task) = server("", vec![json_reply(status, "{}")]).await?;
        let (second, mut second_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
        let result = run_antigravity_failover(&[first, second], &CancellationToken::new()).await;
        match result {
            Err(crate::lifecycle::AttemptFailure::Response { status: seen, .. }) => {
                assert_eq!(seen, status);
            }
            Ok(_) => return Err(io::Error::other(format!("{status} was served")).into()),
            Err(other) => {
                return Err(io::Error::other(format!("{status}: {other:?}")).into());
            }
        }
        first_task.join_next().await.expect("first answered")??;
        assert!(
            never_contacted(&mut second_task).await,
            "{status} failed over"
        );
        second_task.abort_all();
    }
    Ok(())
}

#[tokio::test]
async fn antigravity_still_fails_over_on_a_transport_error() -> TestResult {
    // A refused connection (the port is closed) is a pre-header transport
    // failure; the next endpoint serves the stream.
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        format!("http://{}", listener.local_addr()?)
    };
    let (second, mut second_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
    let result = run_antigravity_failover(&[closed, second], &CancellationToken::new()).await;
    assert!(
        matches!(result, Ok(Some(_))),
        "transport failover regressed"
    );
    second_task.join_next().await.expect("second answered")??;
    Ok(())
}

#[tokio::test]
async fn antigravity_cancellation_returns_at_once_without_trying_the_next_endpoint() -> TestResult {
    let (first, mut first_task) = server("", vec![json_reply(503, "{}")]).await?;
    let (second, mut second_task) = server("", vec![stream_reply(google_cca_reply())]).await?;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = run_antigravity_failover(&[first, second], &cancel).await;
    assert!(matches!(result, Ok(None)));
    assert!(never_contacted(&mut first_task).await);
    assert!(never_contacted(&mut second_task).await);
    first_task.abort_all();
    second_task.abort_all();
    Ok(())
}

async fn antigravity_error_text(status: u16, body: Vec<u8>) -> String {
    let dir = TestDir::new().expect("temp dir");
    let (base, _server) = server(
        "",
        vec![Reply {
            status,
            content_type: "application/json",
            body,
        }],
    )
    .await
    .expect("loopback");
    let mut antigravity_entry = entry(
        "factory-droid",
        Family::GoogleAntigravity,
        base,
        None,
        1,
        Transport::Https,
    );
    antigravity_entry.settings.project = Some("cca-proj".into());
    stored_oauth(&dir, "factory-droid", "ya29-test").expect("oauth");
    let set = ProviderSet::new(
        &config(vec![antigravity_entry], 0),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )
    .expect("provider set");
    open_error_text(
        &set,
        "factory-droid",
        Family::GoogleAntigravity,
        "gemini-3-flash-agent",
        vec![user_text("Say hello.")],
    )
    .await
}

#[tokio::test]
async fn google_antigravity_validation_required_yields_account_verification() -> TestResult {
    let url = "https://accounts.example.com/verify/abc";
    let body = format!(
        "complete your verification {{\"error\":{{\"code\":400,\"status\":\"FAILED_PRECONDITION\",\"message\":\"verification needed\",\"details\":[{{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{{\"validation_url\":\"{url}\"}}}}]}}}}"
    );
    let text = antigravity_error_text(400, body.into_bytes()).await;
    assert!(text.contains("retry your request"), "{text}");
    assert!(text.contains(url), "{text}");
    assert!(!text.contains('@'), "{text}");
    Ok(())
}

#[tokio::test]
async fn google_antigravity_quota_precedes_validation() -> TestResult {
    // Quota signals win even when a validation URL rides along: the failure
    // stays typed quota instead of becoming account verification.
    let body = "{\"error\":{\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\",\"message\":\"quota will reset at midnight\",\"details\":[{\"@type\":\"type.googleapis.com/google.rpc.ErrorInfo\",\"reason\":\"QUOTA_EXHAUSTED\"},{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{\"validation_url\":\"https://accounts.example.com/verify/abc\"}}]}}";
    let dir = TestDir::new()?;
    let (base, _server) = server(
        "",
        vec![Reply {
            status: 429,
            content_type: "application/json",
            body: body.as_bytes().to_vec(),
        }],
    )
    .await?;
    let mut antigravity_entry = entry(
        "factory-droid",
        Family::GoogleAntigravity,
        base,
        None,
        1,
        Transport::Https,
    );
    antigravity_entry.settings.project = Some("cca-proj".into());
    stored_oauth(&dir, "factory-droid", "ya29-test")?;
    let set = ProviderSet::new(
        &config(vec![antigravity_entry], 0),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved(
        "factory-droid",
        Family::GoogleAntigravity,
        "gemini-3-flash-agent",
    ))?;
    let request = request(
        Family::GoogleAntigravity,
        "gemini-3-flash-agent",
        vec![user_text("Say hello.")],
    );
    let error = match provider
        .open(
            SessionId::new_v7(),
            &request,
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await
    {
        Err(error) => error,
        Ok(stream) => match collect(stream).await {
            Err(error) => error,
            Ok(events) => panic!("quota must fail, got success: {events:?}"),
        },
    };
    assert!(matches!(error, ProviderError::Quota { .. }), "{error:?}");
    Ok(())
}

#[tokio::test]
async fn google_antigravity_non_validation_errors_never_become_verification() -> TestResult {
    // A plain documentation link is not account verification.
    let text = antigravity_error_text(
        400,
        "{\"error\":{\"code\":400,\"status\":\"FAILED_PRECONDITION\",\"message\":\"see https://docs.example.com/quota for limits\"}}"
            .as_bytes()
            .to_vec(),
    )
    .await;
    assert!(!text.contains("retry your request"), "{text}");

    // Untrusted validation URLs never become verification either: a wrong
    // scheme, embedded userinfo, and a decoded control character.
    for url in [
        "http://accounts.example.com/verify",
        "https://user:pass@accounts.example.com/",
        "https://accounts.example.com/\\u0001",
    ] {
        let body = format!(
            "{{\"error\":{{\"code\":400,\"status\":\"FAILED_PRECONDITION\",\"message\":\"verification needed\",\"details\":[{{\"reason\":\"VALIDATION_REQUIRED\",\"metadata\":{{\"validation_url\":\"{url}\"}}}}]}}}}"
        );
        let text = antigravity_error_text(400, body.into_bytes()).await;
        assert!(!text.contains("retry your request"), "{url}: {text}");
        assert!(!text.contains("user:pass"), "{url}: {text}");
    }
    Ok(())
}

#[tokio::test]
async fn devin_router_turn_flows_jwt_assign_chat() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server(
        "",
        vec![
            proto_reply(devin_jwt_reply("jwt-1")),
            proto_reply(devin_discovery_reply("adaptive", true)),
            proto_reply(devin_assign_reply()),
            proto_reply(devin_chat_reply()),
        ],
    )
    .await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "devin-test",
                Family::Devin,
                base,
                Some("DEVIN_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("DEVIN_TEST_KEY", "session-token-abc")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("devin-test", Family::Devin, "adaptive"))?;
    let request = request(Family::Devin, "adaptive", vec![user_text("Route me.")]);
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 4);

    let auth_head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(
        auth_head.starts_with("post /exa.auth_pb.authservice/getuserjwt "),
        "{auth_head}"
    );
    assert!(
        auth_head.contains("content-type: application/proto"),
        "{auth_head}"
    );
    let auth = GetUserJwtRequest::decode(request_body(&captured[0]))?;
    assert_eq!(
        auth.metadata.expect("auth metadata").api_key,
        "devin-session-token$session-token-abc"
    );

    let assign_head = String::from_utf8_lossy(&captured[2]).to_ascii_lowercase();
    assert!(
        assign_head.starts_with("post /exa.api_server_pb.apiserverservice/assignmodel "),
        "{assign_head}"
    );
    let assign = AssignModelRequest::decode(request_body(&captured[2]))?;
    assert_eq!(assign.model_router_uid, "adaptive");
    assert_ne!(assign.cascade_id, "");

    let chat_head = String::from_utf8_lossy(&captured[3]).to_ascii_lowercase();
    assert!(
        chat_head.starts_with("post /exa.api_server_pb.apiserverservice/getchatmessage "),
        "{chat_head}"
    );
    assert!(
        chat_head.contains("content-type: application/connect+proto"),
        "{chat_head}"
    );
    let chat = decode_chat_body(&captured[3])?;
    assert_eq!(chat.chat_model_uid, "devin-serve-1");
    assert_eq!(chat.model_assignment_jwt.as_deref(), Some("assign-1"));
    Ok(())
}

#[tokio::test]
async fn devin_serving_turn_chats_directly_without_assignment() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server(
        "",
        vec![
            proto_reply(devin_jwt_reply("jwt-1")),
            proto_reply(devin_discovery_reply("adaptive", true)),
            proto_reply(devin_chat_reply()),
        ],
    )
    .await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "devin-test",
                Family::Devin,
                base,
                Some("DEVIN_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("DEVIN_TEST_KEY", "session-token-abc")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("devin-test", Family::Devin, "devin-direct-1"))?;
    let request = request(
        Family::Devin,
        "devin-direct-1",
        vec![user_text("Answer directly.")],
    );
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    // Discovery lists only the router, so the serving uid chats directly:
    // auth, discovery, chat, and no assignment call.
    let captured = server_task.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 3);
    let chat = decode_chat_body(&captured[2])?;
    assert_eq!(chat.chat_model_uid, "devin-direct-1");
    assert!(chat.model_assignment_jwt.is_none());
    Ok(())
}

#[tokio::test]
async fn devin_401_falls_back_to_the_raw_key() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server(
        "",
        vec![
            Reply {
                status: 401,
                content_type: "application/json",
                body: Vec::from(b"{}".as_slice()),
            },
            proto_reply(devin_jwt_reply("jwt-1")),
            proto_reply(devin_discovery_reply("adaptive", true)),
            proto_reply(devin_assign_reply()),
            proto_reply(devin_chat_reply()),
        ],
    )
    .await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "devin-test",
                Family::Devin,
                base,
                Some("DEVIN_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("DEVIN_TEST_KEY", "session-token-abc")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("devin-test", Family::Devin, "adaptive"))?;
    let request = request(Family::Devin, "adaptive", vec![user_text("Route me.")]);
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
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 5);
    let retry = GetUserJwtRequest::decode(request_body(&captured[1]))?;
    assert_eq!(
        retry.metadata.expect("retry metadata").api_key,
        "session-token-abc",
        "the 401 retry carries the raw key without the session prefix"
    );
    Ok(())
}

async fn devin_failure_of(response: reqwest::Response, key: &str) -> (u16, Option<String>, String) {
    let credential = Credential::ApiKey {
        key: crate::auth::credential::SecretString::from(key),
    };
    let failure = super::request::status_failure(
        response,
        Family::Devin,
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

/// The surfaced text of a turn the loopback server rejects: either `open`
/// refuses or the event stream carries the failure.
async fn open_error_text(
    set: &ProviderSet,
    id: &str,
    family: Family,
    model: &str,
    context: Vec<ContextItem>,
) -> String {
    let provider = set
        .provider(resolved(id, family, model))
        .expect("provider resolves");
    let request = request(family, model, context);
    match provider
        .open(
            SessionId::new_v7(),
            &request,
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await
    {
        Err(error) => error.to_string(),
        Ok(stream) => match collect(stream).await {
            Err(error) => error.to_string(),
            Ok(events) => panic!("expected an error, got success: {events:?}"),
        },
    }
}

async fn devin_chat_error_text(chat: Reply) -> String {
    let dir = TestDir::new().expect("temp dir");
    let (base, _server) = server(
        "",
        vec![
            proto_reply(devin_jwt_reply("jwt-1")),
            proto_reply(devin_discovery_reply("adaptive", true)),
            chat,
        ],
    )
    .await
    .expect("loopback");
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "devin-test",
                Family::Devin,
                base,
                Some("DEVIN_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("DEVIN_TEST_KEY", "session-token-abc")]),
        dir.path(),
        dir.path(),
    )
    .expect("provider set");
    open_error_text(
        &set,
        "devin-test",
        Family::Devin,
        "devin-direct-1",
        vec![user_text("Answer directly.")],
    )
    .await
}

#[tokio::test]
async fn devin_failures_surface_safe_detail_and_refuse_untrusted_bodies() {
    // A safe JSON detail is unwrapped, whitespace-collapsed, and bounded by
    // the helper's own cap rather than the generic first line.
    let (status, _, message) = devin_failure_of(
        reply(
            400,
            "application/json",
            br#"{"error":{"message":"  upstream blew   up "}}"#.to_vec(),
        ),
        "k",
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(message, "upstream blew up");

    // A long safe detail survives intact: 500 e-acutes are 500 UTF-16 units,
    // inside the helper's bound and far past the generic 300-byte cut.
    let long = format!(r#"{{"message":"{}"}}"#, "é".repeat(500));
    let (_, _, message) =
        devin_failure_of(reply(400, "application/json", long.into_bytes()), "k").await;
    assert_eq!(message.len(), 1000);
    assert!(message.chars().all(|c| c == 'é'));

    // Proxy HTML and control-carrying bodies never surface: the typed status
    // is preserved behind the generic fallback.
    for (content_type, body) in [
        ("text/html", b"<html>proxy refused</html>".as_slice()),
        (
            "application/json",
            b"{\"error\":{\"message\":\"bad\x07detail\"}}".as_slice(),
        ),
    ] {
        let (status, _, message) =
            devin_failure_of(reply(400, content_type, body.to_vec()), "k").await;
        assert_eq!(status, 400);
        assert_eq!(message, "devin request failed", "{content_type}");
    }

    // An escaped credential stays redacted after JSON unescaping.
    let (_, _, message) = devin_failure_of(
        reply(
            400,
            "application/json",
            br#"{"error":{"message":"bad key sk\u002dsecret-123"}}"#.to_vec(),
        ),
        "sk-secret-123",
    )
    .await;
    assert!(!message.contains("sk-secret-123"), "{message}");
}

#[tokio::test]
async fn devin_chat_error_surfaces_its_safe_detail_over_loopback() -> TestResult {
    let text = devin_chat_error_text(Reply {
        status: 400,
        content_type: "application/json",
        body: br#"{"error":{"message":"  upstream blew   up "}}"#.to_vec(),
    })
    .await;
    assert!(text.contains("upstream blew up"), "{text}");
    Ok(())
}

#[tokio::test]
async fn devin_chat_html_error_never_echoes_markup_over_loopback() -> TestResult {
    let text = devin_chat_error_text(Reply {
        status: 400,
        content_type: "text/html",
        body: b"<html>proxy refused</html>".to_vec(),
    })
    .await;
    assert!(!text.contains("<html"), "{text}");
    assert!(!text.contains("proxy refused"), "{text}");
    Ok(())
}

#[tokio::test]
async fn droid_completions_wire_carries_registry_upstream() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("", vec![stream_reply(chat_reply())]).await?;
    stored_oauth(&dir, "factory-droid", "workos-token")?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "factory-droid",
                Family::Droid,
                base,
                None,
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("factory-droid", Family::Droid, "kimi-k3"))?;
    let request = request(Family::Droid, "kimi-k3", vec![user_text("Read the file.")]);
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(
        head.starts_with("post /api/llm/o/v1/chat/completions "),
        "{head}"
    );
    assert!(
        head.contains("authorization: bearer workos-token"),
        "{head}"
    );
    assert!(head.contains("x-api-provider: fireworks"), "{head}");
    assert!(
        String::from_utf8_lossy(request_body(&captured[0])).contains("You are Droid,"),
        "the proxy requires the Droid identity prefix"
    );
    Ok(())
}

#[tokio::test]
async fn copilot_chat_denial_resends_with_the_fallback_identity() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) =
        server("/v1", vec![denied_reply(), stream_reply(chat_reply())]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "github-copilot",
                Family::Chat,
                base,
                Some("COPILOT_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("COPILOT_TEST_KEY", "test-token")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("github-copilot", Family::Chat, "gpt-test"))?;
    let request = request(Family::Chat, "gpt-test", vec![user_text("Say hello.")]);
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 2);
    let first = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    let second = String::from_utf8_lossy(&captured[1]).to_ascii_lowercase();
    assert!(first.starts_with("post /v1/chat/completions "), "{first}");
    assert!(
        first.contains("authorization: bearer test-token"),
        "{first}"
    );
    assert!(
        first.contains("copilot-integration-id: copilot-chat"),
        "{first}"
    );
    assert!(first.contains("editor-version: copilot/1.0.82"), "{first}");
    assert!(
        second.contains("copilot-integration-id: copilot-developer-cli"),
        "{second}"
    );
    Ok(())
}

fn resolved_with_premium(
    provider: &str,
    family: Family,
    model: &str,
    multiplier: Option<f64>,
) -> ResolvedModel {
    let mut resolved = resolved(provider, family, model);
    resolved.entry.premium_multiplier = multiplier;
    resolved
}

fn chat_reply_with_usage() -> String {
    String::from(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":2},\"completion_tokens_details\":{\"reasoning_tokens\":1}}}\n\ndata: [DONE]\n\n",
    )
}

#[tokio::test]
async fn copilot_chat_stamps_observed_usage_with_the_entry_multiplier() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) =
        server("/v1", vec![stream_reply(chat_reply_with_usage())]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "github-copilot",
                Family::Chat,
                base,
                Some("COPILOT_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("COPILOT_TEST_KEY", "test-token")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved_with_premium(
        "github-copilot",
        Family::Chat,
        "gpt-test",
        Some(2.5),
    ))?;
    let request = request(Family::Chat, "gpt-test", vec![user_text("Say hello.")]);
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
    let usage = events.iter().find_map(|event| match event {
        StreamEvent::Usage { usage } => Some(usage),
        _ => None,
    });
    let Some(usage) = usage else {
        return Err(io::Error::other("copilot turn reported no usage").into());
    };
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.cached_input_tokens, 2);
    assert_eq!(usage.output_tokens, 4);
    assert_eq!(usage.reasoning_tokens, Some(1));
    assert_eq!(usage.premium_requests, Some(2.5));
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 1);
    server_task.abort_all();
    Ok(())
}

#[tokio::test]
async fn copilot_chat_without_wire_usage_completes_with_premium_only_usage() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(chat_reply())]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "github-copilot",
                Family::Chat,
                base,
                Some("COPILOT_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("COPILOT_TEST_KEY", "test-token")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("github-copilot", Family::Chat, "gpt-test"))?;
    let request = request(Family::Chat, "gpt-test", vec![user_text("Say hello.")]);
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
    let usage = events.iter().find_map(|event| match event {
        StreamEvent::Usage { usage } => Some(usage),
        _ => None,
    });
    let Some(usage) = usage else {
        return Err(io::Error::other("copilot turn reported no usage").into());
    };
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
    assert_eq!(usage.premium_requests, Some(1.0));
    assert_eq!(usage.cost_usd, None);
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 1);
    server_task.abort_all();
    Ok(())
}

fn copilot_test_provider(
    base: String,
    dir: &TestDir,
) -> Result<(ProviderSet, Provider), ProviderError> {
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "github-copilot",
                Family::Chat,
                base,
                Some("COPILOT_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("COPILOT_TEST_KEY", "test-token")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved_with_premium(
        "github-copilot",
        Family::Chat,
        "gpt-test",
        Some(2.5),
    ))?;
    Ok((set, provider))
}

#[tokio::test]
async fn copilot_chat_cancelled_or_failed_never_gains_a_completed_count() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(chat_reply())]).await?;
    let (_set, provider) = copilot_test_provider(base, &dir)?;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = provider
        .open(
            SessionId::new_v7(),
            &request(Family::Chat, "gpt-test", vec![user_text("Say hello.")]),
            &[],
            notice_sink(),
            &cancel,
        )
        .await;
    assert!(
        matches!(result, Err(ProviderError::Transport { .. })),
        "{result:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
            .await
            .is_err()
    );
    server_task.abort_all();

    let dir = TestDir::new()?;
    let (base, mut server_task) = server(
        "/v1",
        vec![Reply {
            status: 500,
            content_type: "text/plain",
            body: Vec::from(b"boom".as_slice()),
        }],
    )
    .await?;
    let (_set2, provider) = copilot_test_provider(base, &dir)?;
    let stream = provider
        .open(
            SessionId::new_v7(),
            &request(Family::Chat, "gpt-test", vec![user_text("Say hello.")]),
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    let (events, error) = drain_stream(stream).await;
    assert!(
        matches!(error, Some(ProviderError::Status { status: 500, .. })),
        "a failed attempt must end in HTTP 500 status error: {error:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Usage { .. })),
        "a failed attempt must never yield a completed usage count: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Stop { .. })),
        "a failed attempt must never yield a stop event: {events:?}"
    );
    let captured = server_task.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 1);
    server_task.abort_all();
    Ok(())
}

#[tokio::test]
async fn azure_chat_uses_deployment_and_api_version() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("/v1", vec![stream_reply(chat_reply())]).await?;
    let mut azure_entry = entry(
        "azure-test",
        Family::Chat,
        base,
        Some("AZURE_TEST_KEY"),
        1,
        Transport::Https,
    );
    azure_entry.auth = AuthStyle::ApiKey;
    azure_entry.settings.api_version = Some("2025-01-01-preview".into());
    azure_entry
        .settings
        .deployments
        .insert("gpt-test".into(), "prod".into());
    let set = ProviderSet::new(
        &config(vec![azure_entry], 0),
        identity(),
        EnvSnapshot::test(&[("AZURE_TEST_KEY", "az-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("azure-test", Family::Chat, "gpt-test"))?;
    let request = request(Family::Chat, "gpt-test", vec![user_text("Say hello.")]);
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
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello"))
    );
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(
        head.starts_with(
            "post /v1/deployments/prod/chat/completions?api-version=2025-01-01-preview "
        ),
        "{head}"
    );
    assert!(head.contains("api-key: az-secret"), "{head}");
    let body = String::from_utf8_lossy(request_body(&captured[0]));
    assert!(body.starts_with(r#"{"model":"prod","#), "{body}");
    assert!(!body.contains(r#""model":"gpt-test""#), "{body}");
    Ok(())
}

/// The built-in `openai` row shaped as Azure (`api-key` header, `api_version`),
/// the way a host opts a row into the Azure adapter; `extra` is more TOML for
/// the same table.
fn azure_row(extra: &str) -> String {
    format!(
        "[providers.openai]\nauth = 'api-key'\nkey_env = 'AZURE_TEST_KEY'\napi_version = '2025-03-01-preview'\n{extra}"
    )
}

const AZURE_KEY: (&str, &str) = ("AZURE_TEST_KEY", "az-secret");

fn model_list_reply() -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: br#"{"data":[{"id":"gpt-azure"}]}"#.to_vec(),
    }
}

/// Asserts that provider `id` carries `expected` where each request path reads
/// its base URL: the slot that serves catalog and inference, and the
/// configuration the set keeps. One prepared endpoint, no second copy.
fn assert_prepared_base(set: &ProviderSet, id: &str, expected: &str) {
    let slot = &set
        .inner
        .providers
        .get(id)
        .unwrap_or_else(|| panic!("{id} is configured"))
        .entry
        .base_url;
    let kept = &set
        .inner
        .config
        .providers
        .iter()
        .find(|entry| &*entry.id == id)
        .unwrap_or_else(|| panic!("{id} is configured"))
        .base_url;
    assert_eq!(&**slot, expected, "{id}: the serving slot's base URL");
    assert_eq!(&**kept, expected, "{id}: the kept configuration's base URL");
}

/// Opens one text turn on the `openai` Responses route and checks its text.
async fn azure_hello(set: &ProviderSet) -> TestResult {
    let provider = set.provider(resolved("openai", Family::Responses, "gpt-azure"))?;
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &request(
                    Family::Responses,
                    "gpt-azure",
                    vec![user_text("Say hello.")],
                ),
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
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello")),
        "the Azure turn streamed no text"
    );
    Ok(())
}

#[tokio::test]
async fn azure_environment_base_url_serves_catalog_and_turn_when_base_url_is_omitted() -> TestResult
{
    let dir = TestDir::new()?;
    let (base, mut asked) = server(
        "/azure",
        vec![model_list_reply(), stream_reply(responses_reply())],
    )
    .await?;
    // The base URL variable outranks the resource variable, so a resource
    // name nothing needs is never validated; padding is trimmed.
    let padded = format!("  {base}  ");
    let set = preset_set(
        &dir,
        &azure_row(""),
        &[
            AZURE_KEY,
            ("AZURE_OPENAI_BASE_URL", padded.as_str()),
            ("AZURE_OPENAI_RESOURCE_NAME", "evil.example/x"),
        ],
    )?;
    // Checked before any request: a set that kept the row default would send
    // the key to the default host.
    assert_prepared_base(&set, "openai", &base);

    let listed = set.load_models("openai").await?;
    assert_eq!(
        listed.source,
        CatalogSource::Live,
        "{:?}",
        listed.live_error
    );
    azure_hello(&set).await?;

    let requests = asked.join_next().await.expect("server completes")??;
    assert_eq!(requests.len(), 2);
    let list = String::from_utf8_lossy(&requests[0]).to_ascii_lowercase();
    assert!(list.starts_with("get /azure/models "), "{list}");
    assert!(list.contains("api-key: az-secret"), "{list}");
    assert!(!list.contains("authorization:"), "{list}");
    let turn = String::from_utf8_lossy(&requests[1]).to_ascii_lowercase();
    assert!(
        turn.starts_with("post /azure/responses?api-version=2025-03-01-preview "),
        "{turn}"
    );
    assert!(turn.contains("api-key: az-secret"), "{turn}");
    assert!(!turn.contains("authorization:"), "{turn}");
    Ok(())
}

#[tokio::test]
async fn azure_resource_name_derives_the_endpoint_when_base_url_is_omitted() -> TestResult {
    let dir = TestDir::new()?;
    let derived = "https://my-res.openai.azure.com/openai/v1";
    // A blank base URL variable is unset; the padded resource name is trimmed.
    let set = preset_set(
        &dir,
        &azure_row(""),
        &[
            AZURE_KEY,
            ("AZURE_OPENAI_BASE_URL", "   "),
            ("AZURE_OPENAI_RESOURCE_NAME", " my-res "),
        ],
    )?;
    assert_prepared_base(&set, "openai", derived);
    let provider = set.provider(resolved("openai", Family::Responses, "gpt-azure"))?;
    let Provider::Responses(http) = &provider else {
        return Err(io::Error::other("the openai row binds the Responses variant").into());
    };
    assert_eq!(&*http.entry.base_url, derived);
    Ok(())
}

#[tokio::test]
async fn azure_invalid_resource_name_refuses_an_entry_with_an_omitted_base_url() -> TestResult {
    let dir = TestDir::new()?;
    for bad in ["evil.example/x", "a.b", "-x", "x-", "x@evil.com"] {
        let built = preset_set(
            &dir,
            &azure_row(""),
            &[AZURE_KEY, ("AZURE_OPENAI_RESOURCE_NAME", bad)],
        );
        let Err(error) = built else {
            return Err(io::Error::other(format!("resource {bad:?} was accepted")).into());
        };
        let Some(ProviderError::InvalidRequest { message }) = error.downcast_ref::<ProviderError>()
        else {
            return Err(io::Error::other(format!("{bad:?} failed untyped: {error}")).into());
        };
        assert!(
            message.contains("openai")
                && message.contains("AZURE_OPENAI_RESOURCE_NAME")
                && message.contains(bad),
            "{bad:?}: {message}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn azure_explicit_base_url_beats_the_environment_and_its_invalid_resource() -> TestResult {
    let dir = TestDir::new()?;
    let (explicit, mut asked) = server(
        "/explicit",
        vec![model_list_reply(), stream_reply(responses_reply())],
    )
    .await?;
    let (decoy, mut decoy_asked) = server("/decoy", vec![stream_reply(responses_reply())]).await?;
    let set = preset_set(
        &dir,
        &azure_row(&format!("base_url = '{explicit}'\n")),
        &[
            AZURE_KEY,
            ("AZURE_OPENAI_BASE_URL", decoy.as_str()),
            ("AZURE_OPENAI_RESOURCE_NAME", "evil.example/x"),
        ],
    )?;
    assert_prepared_base(&set, "openai", &explicit);

    let listed = set.load_models("openai").await?;
    assert_eq!(
        listed.source,
        CatalogSource::Live,
        "{:?}",
        listed.live_error
    );
    azure_hello(&set).await?;

    let requests = asked.join_next().await.expect("server completes")??;
    assert_eq!(requests.len(), 2);
    let list = String::from_utf8_lossy(&requests[0]).to_ascii_lowercase();
    assert!(list.starts_with("get /explicit/models "), "{list}");
    assert!(list.contains("api-key: az-secret"), "{list}");
    let turn = String::from_utf8_lossy(&requests[1]).to_ascii_lowercase();
    assert!(turn.starts_with("post /explicit/responses?"), "{turn}");
    assert!(turn.contains("api-key: az-secret"), "{turn}");
    // The environment endpoint never saw a connection, so it never saw the key.
    assert!(
        tokio::time::timeout(Duration::from_millis(25), decoy_asked.join_next())
            .await
            .is_err()
    );
    decoy_asked.abort_all();
    Ok(())
}

#[tokio::test]
async fn azure_environment_never_replaces_an_explicit_default_or_a_non_azure_row() -> TestResult {
    let dir = TestDir::new()?;
    let default = crate::find("openai")
        .expect("the openai row exists")
        .base_url;
    let env = [
        AZURE_KEY,
        ("AZURE_OPENAI_BASE_URL", "http://127.0.0.1:9/decoy"),
        ("AZURE_OPENAI_RESOURCE_NAME", "my-res"),
    ];

    // Naming the row's own default is still naming a URL: it wins, because
    // omission is what the table said, not what the value equals.
    let configured = preset_set(&dir, &azure_row(&format!("base_url = '{default}'\n")), &env)?;
    assert_prepared_base(&configured, "openai", default);

    // A named Azure-shaped provider names its own base URL and keeps it.
    let named = preset_set(
        &dir,
        concat!(
            "[providers.azure-named]\napi = 'openai_responses'\n",
            "base_url = 'https://named.example.com/openai/v1'\n",
            "key_env = 'AZURE_TEST_KEY'\nauth = 'api-key'\n",
        ),
        &env,
    )?;
    assert_prepared_base(&named, "azure-named", "https://named.example.com/openai/v1");

    // A row without the Azure shape never takes an Azure variable, and an
    // invalid resource name no Azure entry needs refuses nothing.
    let plain = preset_set(
        &dir,
        "",
        &[
            AZURE_KEY,
            ("AZURE_OPENAI_BASE_URL", "http://127.0.0.1:9/decoy"),
            ("AZURE_OPENAI_RESOURCE_NAME", "evil.example/x"),
        ],
    )?;
    assert_prepared_base(&plain, "openai", default);

    // Blank variables name nothing: the Azure-shaped row keeps its default.
    let blank = preset_set(
        &dir,
        &azure_row(""),
        &[
            AZURE_KEY,
            ("AZURE_OPENAI_BASE_URL", "  "),
            ("AZURE_OPENAI_RESOURCE_NAME", "\t"),
        ],
    )?;
    assert_prepared_base(&blank, "openai", default);
    Ok(())
}

#[tokio::test]
async fn keyless_devin_refuses_before_any_request() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("", vec![proto_reply(devin_jwt_reply("jwt-1"))]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "devin-test",
                Family::Devin,
                base,
                None,
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let result = set.provider(resolved("devin-test", Family::Devin, "adaptive"));
    assert!(
        matches!(&result, Err(ProviderError::NoCredentials { provider }) if provider == "devin-test"),
        "{result:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
            .await
            .is_err()
    );
    server_task.abort_all();
    Ok(())
}

#[tokio::test]
async fn droid_api_key_never_reaches_the_proxy() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("", vec![stream_reply(chat_reply())]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "droid-test",
                Family::Droid,
                base,
                Some("DROID_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("DROID_TEST_KEY", "control-plane-key")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("droid-test", Family::Droid, "kimi-k3"))?;
    let stream = provider
        .open(
            SessionId::new_v7(),
            &request(Family::Droid, "kimi-k3", vec![user_text("Read the file.")]),
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    let events = collect(stream).await;
    assert!(
        matches!(&events, Err(ProviderError::InvalidRequest { message }) if message.contains("Factory API keys")),
        "{events:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
            .await
            .is_err()
    );
    server_task.abort_all();
    Ok(())
}

#[test]
fn azure_and_copilot_row_selectors_follow_the_documented_shape() {
    let shaped = |id: &str, family: Family, auth: AuthStyle| ProviderEntry {
        auth,
        ..entry(
            id,
            family,
            String::from("https://example.com/v1"),
            None,
            4,
            Transport::Https,
        )
    };
    let azure = shaped("azure-test", Family::Chat, AuthStyle::ApiKey);
    assert!(super::request::is_azure_entry(&azure));
    assert!(!super::request::is_copilot_entry(&azure));
    let keyed = shaped("plain-test", Family::Chat, AuthStyle::Bearer);
    assert!(!super::request::is_azure_entry(&keyed));
    assert!(!super::request::is_copilot_entry(&keyed));
    // The auth gate is family-scoped: another family keeps its own path
    // whatever its auth style.
    let other = shaped("other-test", Family::Mistral, AuthStyle::ApiKey);
    assert!(!super::request::is_azure_entry(&other));

    let copilot = shaped("github-copilot", Family::Chat, AuthStyle::Bearer);
    assert!(super::request::is_copilot_entry(&copilot));
    assert!(!super::request::is_azure_entry(&copilot));
    let mut pinned = shaped("custom-copilot", Family::Responses, AuthStyle::Bearer);
    pinned.settings.integration = Some("copilot-chat".into());
    assert!(super::request::is_copilot_entry(&pinned));
    let mut hosted = shaped("custom-copilot", Family::Anthropic, AuthStyle::Bearer);
    hosted.base_url = String::from("https://api.githubcopilot.com").into_boxed_str();
    assert!(super::request::is_copilot_entry(&hosted));
}

fn eventstream_crc(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// One binary event-stream frame — prelude, string headers, payload, and
/// both CRCs — encoded independently of the family decoder.
fn eventstream_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut block = Vec::new();
    for (name, value) in headers {
        block.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
        block.extend_from_slice(name.as_bytes());
        block.push(7);
        block.extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_be_bytes());
        block.extend_from_slice(value.as_bytes());
    }
    let total = u32::try_from(12 + block.len() + payload.len() + 4).unwrap_or(u32::MAX);
    let headers_len = u32::try_from(block.len()).unwrap_or(u32::MAX);
    let mut wire = Vec::new();
    wire.extend_from_slice(&total.to_be_bytes());
    wire.extend_from_slice(&headers_len.to_be_bytes());
    wire.extend_from_slice(&eventstream_crc(&wire).to_be_bytes());
    wire.extend_from_slice(&block);
    wire.extend_from_slice(payload);
    let message_crc = eventstream_crc(&wire);
    wire.extend_from_slice(&message_crc.to_be_bytes());
    wire
}

fn eventstream_event(kind: &str, payload: &str) -> Vec<u8> {
    eventstream_frame(
        &[
            (":message-type", "event"),
            (":event-type", kind),
            (":content-type", "application/json"),
        ],
        payload.as_bytes(),
    )
}

/// One text-only Converse turn: two text deltas, a stop, and usage.
fn bedrock_text_turn() -> Vec<u8> {
    [
        eventstream_event("messageStart", r#"{"role":"assistant"}"#),
        eventstream_event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"text":"Hel"}}"#,
        ),
        eventstream_event(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"text":"lo"}}"#,
        ),
        eventstream_event("contentBlockStop", r#"{"contentBlockIndex":0}"#),
        eventstream_event("messageStop", r#"{"stopReason":"end_turn"}"#),
        eventstream_event(
            "metadata",
            r#"{"usage":{"inputTokens":10,"outputTokens":5,"totalTokens":15}}"#,
        ),
    ]
    .concat()
}

struct BytesReply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

/// A loopback server speaking binary bodies, modeled on [`server`]: it
/// serves each reply once, in order, and captures every request head.
async fn bytes_server(
    replies: Vec<BytesReply>,
) -> io::Result<(String, tokio::task::JoinSet<io::Result<Vec<Vec<u8>>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let url = format!("http://{address}");
    let mut task = tokio::task::JoinSet::new();
    task.spawn(async move {
        let mut requests = Vec::with_capacity(replies.len());
        for reply in replies {
            let (mut socket, _) = listener.accept().await?;
            requests.push(read_request(&mut socket).await?);
            let reason = match reply.status {
                200 => "OK",
                401 => "Unauthorized",
                _ => "Test",
            };
            let headers = format!(
                "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                reply.status,
                reply.content_type,
                reply.body.len()
            );
            socket.write_all(headers.as_bytes()).await?;
            socket.write_all(&reply.body).await?;
            socket.flush().await?;
        }
        Ok(requests)
    });
    Ok((url, task))
}

/// A Bedrock route with Anthropic thinking support: the catalog seam owns
/// this shape for real rows, and the adapter proves against it directly.
fn bedrock_resolved(provider: &str, model: &str) -> ResolvedModel {
    let mut resolved = resolved(provider, Family::Bedrock, model);
    resolved.entry.thinking = ThinkingSupport::Budget { can_disable: true };
    resolved
}

fn bedrock_env(vars: &[(&str, &str)]) -> EnvSnapshot {
    let mut full = vec![("AWS_EC2_METADATA_DISABLED", "true")];
    full.extend(vars.iter().copied());
    EnvSnapshot::test(&full)
}

#[tokio::test]
async fn bedrock_sigv4_turn_streams_eventstream_through_loopback() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server) = bytes_server(vec![BytesReply {
        status: 200,
        content_type: "application/vnd.amazon.eventstream",
        body: bedrock_text_turn(),
    }])
    .await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "bedrock-test",
                Family::Bedrock,
                base,
                None,
                2,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        bedrock_env(&[
            ("AWS_ACCESS_KEY_ID", "test-key"),
            ("AWS_SECRET_ACCESS_KEY", "test-secret"),
        ]),
        dir.path(),
        dir.path(),
    )?;
    // No keyed credential exists, so the route binds credential-less and the
    // AWS chain owns auth.
    let provider = set.provider(bedrock_resolved("bedrock-test", "anthropic.claude-test"))?;
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &request(
                    Family::Bedrock,
                    "anthropic.claude-test",
                    vec![user_text("Say hello.")],
                ),
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?,
    )
    .await?;
    let texts: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, "Hello");
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 1);
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(head.starts_with("post /model/anthropic.claude-test/converse-stream "));
    assert!(head.contains("authorization: aws4-hmac-sha256"));
    assert!(head.contains("x-amz-date:"));
    assert!(head.contains("x-amz-content-sha256:"));
    assert!(head.contains("accept: application/vnd.amazon.eventstream"));
    assert!(!head.contains("authorization: bearer "));
    Ok(())
}

#[tokio::test]
async fn bedrock_bearer_token_rides_authorization_header_unsigned() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server) = bytes_server(vec![BytesReply {
        status: 200,
        content_type: "application/vnd.amazon.eventstream",
        body: bedrock_text_turn(),
    }])
    .await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "bedrock-bearer",
                Family::Bedrock,
                base,
                None,
                2,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        bedrock_env(&[("AWS_BEARER_TOKEN_BEDROCK", "bearer-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(bedrock_resolved("bedrock-bearer", "anthropic.claude-test"))?;
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &request(
                    Family::Bedrock,
                    "anthropic.claude-test",
                    vec![user_text("Say hello.")],
                ),
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?,
    )
    .await?;
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    let captured = server.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 1);
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(head.contains("authorization: bearer bearer-secret"));
    assert!(!head.contains("x-amz-date:"));
    Ok(())
}

#[tokio::test]
async fn bedrock_unauthorized_invalidates_and_retries_once() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut server) = bytes_server(vec![
        BytesReply {
            status: 401,
            content_type: "application/json",
            body: br#"{"message":"expired"}"#.to_vec(),
        },
        BytesReply {
            status: 200,
            content_type: "application/vnd.amazon.eventstream",
            body: bedrock_text_turn(),
        },
    ])
    .await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "bedrock-retry",
                Family::Bedrock,
                base,
                None,
                2,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        bedrock_env(&[
            ("AWS_ACCESS_KEY_ID", "test-key"),
            ("AWS_SECRET_ACCESS_KEY", "test-secret"),
        ]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(bedrock_resolved("bedrock-retry", "anthropic.claude-test"))?;
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &request(
                    Family::Bedrock,
                    "anthropic.claude-test",
                    vec![user_text("Say hello.")],
                ),
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?,
    )
    .await?;
    let texts: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, "Hello");
    let captured = server.join_next().await.expect("server completes")??;
    assert_eq!(captured.len(), 2, "one rejection plus one retry");
    Ok(())
}

/// The secret access key of the echo tests' credential. It never travels, so
/// only a redaction list that names every secret field of the credential
/// scrubs a server reply that quotes it.
const ECHO_SECRET_ACCESS_KEY: &str = "echo-test-secret-access-key";

/// The credential material of one signed Bedrock request, read back from the
/// request head the way a server that echoes headers would see it.
struct EchoedCredential {
    key: String,
    token: String,
    signature: String,
    authorization: String,
}

impl EchoedCredential {
    fn parse(head: &str) -> Self {
        let header = |name: &str| {
            head.lines()
                .find_map(|line| {
                    let (field, value) = line.split_once(':')?;
                    field
                        .eq_ignore_ascii_case(name)
                        .then(|| value.trim().to_owned())
                })
                .unwrap_or_default()
        };
        let authorization = header("authorization");
        let key = authorization
            .split_once("Credential=")
            .and_then(|(_, rest)| rest.split_once('/'))
            .map(|(key, _)| key.to_owned())
            .unwrap_or_default();
        let signature = authorization
            .rsplit_once("Signature=")
            .map(|(_, signature)| signature.to_owned())
            .unwrap_or_default();
        Self {
            key,
            token: header("x-amz-security-token"),
            signature,
            authorization,
        }
    }

    /// One sentence that quotes every value the request carried, then the
    /// secret access key, the way a signature mismatch quotes signed headers.
    fn quote(&self) -> String {
        format!(
            "key {} token {} signature {} presented {} secret {ECHO_SECRET_ACCESS_KEY}",
            self.key, self.token, self.signature, self.authorization
        )
    }

    fn values(&self) -> [&str; 5] {
        [
            &self.key,
            &self.token,
            &self.signature,
            &self.authorization,
            ECHO_SECRET_ACCESS_KEY,
        ]
    }
}

/// A loopback server that answers `replies` requests with whatever `respond`
/// builds from the credential material each request carried, and returns the
/// material it saw.
async fn signature_echo_server(
    replies: usize,
    respond: fn(&EchoedCredential) -> Reply,
) -> io::Result<(
    String,
    tokio::task::JoinSet<io::Result<Vec<EchoedCredential>>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let url = format!("http://{address}");
    let mut task = tokio::task::JoinSet::new();
    task.spawn(async move {
        let mut seen = Vec::with_capacity(replies);
        for _ in 0..replies {
            let (mut socket, _) = listener.accept().await?;
            let captured = read_request(&mut socket).await?;
            let echoed = EchoedCredential::parse(&String::from_utf8_lossy(&captured));
            write_reply(&mut socket, respond(&echoed)).await?;
            seen.push(echoed);
        }
        Ok(seen)
    });
    Ok((url, task))
}

/// One Bedrock turn signed with a temporary credential (access key id, secret
/// access key, session token) against `base`.
async fn sigv4_session_turn(
    base: String,
) -> Result<Result<Vec<StreamEvent>, ProviderError>, Box<dyn Error>> {
    let dir = TestDir::new()?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "bedrock-echo",
                Family::Bedrock,
                base,
                None,
                2,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        bedrock_env(&[
            ("AWS_ACCESS_KEY_ID", "AKIDECHOTESTKEY"),
            ("AWS_SECRET_ACCESS_KEY", ECHO_SECRET_ACCESS_KEY),
            ("AWS_SESSION_TOKEN", "echo-test-session-token"),
        ]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(bedrock_resolved("bedrock-echo", "anthropic.claude-test"))?;
    let opened = provider
        .open(
            SessionId::new_v7(),
            &request(
                Family::Bedrock,
                "anthropic.claude-test",
                vec![user_text("Say hello.")],
            ),
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await;
    Ok(match opened {
        Ok(stream) => collect(stream).await,
        Err(error) => Err(error),
    })
}

/// Asserts that a surfaced failure carries none of the credential material
/// the server saw, and that the echo was scrubbed rather than dropped.
fn assert_scrubbed(failure: &ProviderError, seen: &[EchoedCredential]) {
    let text = format!("{failure:?} {failure}");
    assert!(
        text.contains("<redacted>"),
        "the echo was dropped, not scrubbed: {text}"
    );
    assert!(!seen.is_empty());
    for echoed in seen {
        for value in echoed.values() {
            assert!(!value.is_empty(), "the request carried no such value");
            assert!(!text.contains(value), "{value} survived in {text}");
        }
    }
}

#[tokio::test]
async fn a_rejected_sigv4_request_never_surfaces_the_credential_it_carried() -> TestResult {
    // A 403 signature mismatch quotes the signed headers: the session token,
    // the access key id and the signature. The single unauthorized retry
    // sends the request again, so both attempts are echoed.
    let (base, mut server) = signature_echo_server(2, |echoed| Reply {
        status: 403,
        content_type: "application/json",
        body: format!(r#"{{"message":"{}"}}"#, echoed.quote()).into_bytes(),
    })
    .await?;
    let outcome = sigv4_session_turn(base).await?;
    let seen = server.join_next().await.expect("server completes")??;
    assert_eq!(seen.len(), 2, "one rejection plus one retry");
    let Err(failure) = outcome else {
        return Err(io::Error::other("a 403 must fail the turn").into());
    };
    assert_scrubbed(&failure, &seen);
    Ok(())
}

#[tokio::test]
async fn an_in_stream_exception_never_surfaces_the_sigv4_credential() -> TestResult {
    // An exception frame rides inside an HTTP 200 stream, so it reaches the
    // caller through the decoder's redaction, not the status path.
    let (base, mut server) = signature_echo_server(1, |echoed| Reply {
        status: 200,
        content_type: "application/vnd.amazon.eventstream",
        body: eventstream_frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "validationException"),
                (":content-type", "application/json"),
            ],
            format!(r#"{{"message":"{}"}}"#, echoed.quote()).as_bytes(),
        ),
    })
    .await?;
    let outcome = sigv4_session_turn(base).await?;
    let seen = server.join_next().await.expect("server completes")??;
    let Err(failure) = outcome else {
        return Err(io::Error::other("an exception frame must fail the turn").into());
    };
    assert_scrubbed(&failure, &seen);
    Ok(())
}

fn gitlab_grant_reply(token: &str) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: format!(r#"{{"token":"{token}","headers":{{"X-GitLab-Test":"1"}}}}"#).into_bytes(),
    }
}

#[tokio::test]
async fn gitlab_grant_cache_is_singleton_and_refetchable_over_loopback() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut grant_server) = server(
        "",
        vec![
            gitlab_grant_reply("grant-one"),
            gitlab_grant_reply("grant-two"),
        ],
    )
    .await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "gitlab-test",
                Family::GitLab,
                base.clone(),
                Some("GITLAB_TEST_TOKEN"),
                2,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("GITLAB_TEST_TOKEN", "pat-token")]),
        dir.path(),
        dir.path(),
    )?;
    let client = set
        .inner
        .providers
        .get("gitlab-test")
        .expect("configured provider")
        .client
        .get()
        .clone();
    let user_agent = identity().user_agent();
    let grants = set.gitlab_grants();
    // The same instance serves every caller: the second ensure is a cache
    // hit and never reaches the loopback grant endpoint.
    grants
        .ensure(&client, &base, "pat-token", &user_agent)
        .await?;
    grants
        .ensure(&client, &base, "pat-token", &user_agent)
        .await?;
    // Invalidation drops every grant of the token, so the next ensure
    // refetches instead of serving the rotated grant.
    grants.remove("pat-token");
    grants
        .ensure(&client, &base, "pat-token", &user_agent)
        .await?;
    let captured = grant_server
        .join_next()
        .await
        .expect("server completes")??;
    assert_eq!(
        captured.len(),
        2,
        "one initial fetch and one refetch after invalidation"
    );
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(head.starts_with("post /api/v4/ai/third_party_agents/direct_access "));
    assert!(head.contains("authorization: bearer pat-token"));
    let second_head = String::from_utf8_lossy(&captured[1]).to_ascii_lowercase();
    assert!(second_head.starts_with("post /api/v4/ai/third_party_agents/direct_access "));
    assert!(second_head.contains("authorization: bearer pat-token"));
    Ok(())
}

#[tokio::test]
async fn gitlab_proxy_builds_pinned_wire_from_cached_grant() -> TestResult {
    let dir = TestDir::new()?;
    let (base, mut grant_server) = server("", vec![gitlab_grant_reply("grant-wire")]).await?;
    let set = ProviderSet::new(
        &config(
            vec![entry(
                "gitlab-wire",
                Family::GitLab,
                base.clone(),
                Some("GITLAB_TEST_TOKEN"),
                2,
                Transport::Https,
            )],
            0,
        ),
        identity(),
        EnvSnapshot::test(&[("GITLAB_TEST_TOKEN", "pat-token")]),
        dir.path(),
        dir.path(),
    )?;
    let client = set
        .inner
        .providers
        .get("gitlab-wire")
        .expect("configured provider")
        .client
        .get()
        .clone();
    let user_agent = identity().user_agent();
    let grant = set
        .gitlab_grants()
        .ensure(&client, &base, "pat-token", &user_agent)
        .await?;
    let _ = grant_server
        .join_next()
        .await
        .expect("server completes")??;
    let mut resolved = resolved("gitlab-wire", Family::GitLab, "duo-chat-sonnet-4-6");
    resolved.entry.thinking = ThinkingSupport::Budget { can_disable: true };
    let wire = crate::family::gitlab::build_proxy(&crate::family::gitlab::DuoProxyRequest {
        request: &request(
            Family::GitLab,
            "duo-chat-sonnet-4-6",
            vec![user_text("Summarize.")],
        ),
        thinking: &crate::thinking::plan(
            ThinkingLevel::Off,
            &crate::thinking::BeforeRequestPatch::default(),
            &resolved.entry.thinking,
            crate::thinking::RequestLimits {
                max_tokens: 4096,
                max_output: 4096,
                temperature_allowed: true,
            },
        ),
        grant: &grant,
        max_output: resolved.entry.max_output,
        reasoning_summary: resolved.entry.supports_reasoning_summaries,
        display_supported: resolved.entry.display_supported,
        session: SessionId::new_v7(),
    })?;
    assert!(
        matches!(wire.route, crate::family::gitlab::ProxyRoute::Anthropic),
        "the claude alias rides the Anthropic proxy"
    );
    assert!(
        wire.url
            .as_str()
            .starts_with(crate::family::gitlab::ANTHROPIC_PROXY_BASE),
        "the proxy URL is the pinned base, never the configured one"
    );
    let (name, value) = wire.headers.first().expect("grant bearer leads");
    assert_eq!(*name, "authorization");
    assert!(value.starts_with("Bearer "), "the grant rides bearer");
    Ok(())
}

#[tokio::test]
async fn gitlab_workflow_path_skips_grant_fetch_on_namespace_entries() -> TestResult {
    let dir = TestDir::new()?;
    // A grant endpoint that must never be hit: the workflow surface trades
    // its own per-run token, never a `direct_access` grant.
    let (grant_base, mut grant_server) =
        server("", vec![gitlab_grant_reply("grant-never")]).await?;
    let closed_listener = TcpListener::bind("127.0.0.1:0").await?;
    let closed_base = format!("http://{}", closed_listener.local_addr()?);
    drop(closed_listener);
    let _ = grant_base;
    let mut wf_entry = entry(
        "gitlab-wf",
        Family::GitLab,
        closed_base,
        Some("GITLAB_TEST_TOKEN"),
        2,
        Transport::Https,
    );
    wf_entry.settings.namespace = Some("123".into());
    let set = ProviderSet::new(
        &config(vec![wf_entry], 0),
        identity(),
        EnvSnapshot::test(&[("GITLAB_TEST_TOKEN", "pat-token")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("gitlab-wf", Family::GitLab, "duo-agent"))?;
    let stream = provider
        .open(
            SessionId::new_v7(),
            &request(Family::GitLab, "duo-agent", vec![user_text("Run.")]),
            &[],
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    let outcome = collect(stream).await;
    // The workflow REST setup has no listener, so the turn fails closed at
    // transport without ever fetching a proxy grant.
    assert!(
        matches!(outcome, Err(ProviderError::Transport { .. })),
        "a namespace entry runs the workflow surface, not the proxy"
    );
    assert!(
        grant_server.try_join_next().is_none(),
        "the workflow path never touches the grant endpoint"
    );
    Ok(())
}

#[tokio::test]
async fn gitlab_workflow_namespace_path_is_refused_typed() -> TestResult {
    let dir = TestDir::new()?;
    for namespace in [
        "my-group/my-project",
        "gid://gitlab/Group/",
        "gid://gitlab/Group/abc",
        "",
    ] {
        let mut path_entry = entry(
            "gitlab-path",
            Family::GitLab,
            String::from("https://example.com"),
            Some("GITLAB_TEST_TOKEN"),
            2,
            Transport::Https,
        );
        path_entry.settings.namespace = Some(namespace.into());
        let set = ProviderSet::new(
            &config(vec![path_entry], 0),
            identity(),
            EnvSnapshot::test(&[("GITLAB_TEST_TOKEN", "pat-token")]),
            dir.path(),
            dir.path(),
        )?;
        let provider = set.provider(resolved("gitlab-path", Family::GitLab, "duo-agent"))?;
        let stream = provider
            .open(
                SessionId::new_v7(),
                &request(Family::GitLab, "duo-agent", vec![user_text("Run.")]),
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?;
        let outcome = collect(stream).await;
        assert!(
            matches!(outcome, Err(ProviderError::InvalidRequest { .. })),
            "namespace {namespace:?} is refused before any workflow call"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn gemini_cli_spawns_fixture_through_provider_set() -> TestResult {
    let dir = TestDir::new()?;
    let fixture = format!(
        "{}/src/family/google/gemini_cli/fixtures/fake-gemini.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut gem_entry = entry(
        "gemini-test",
        Family::GoogleGeminiCli,
        String::from("https://example.com"),
        None,
        2,
        Transport::Https,
    );
    gem_entry.auth = AuthStyle::None;
    gem_entry.settings.binary = Some(fixture.into());
    let env = EnvSnapshot::test(&[("GEMINI_TEST_MARKER", "1")]).with_cwd(dir.path().to_path_buf());
    let set = ProviderSet::new(
        &config(vec![gem_entry], 0),
        identity(),
        env,
        dir.path(),
        dir.path(),
    )?;
    // The deny-all policy is published at build through the dal-store
    // private-file boundary, in the exact CLI grammar.
    let policy = set
        .inner
        .gemini_policy
        .clone()
        .expect("policy written at build");
    assert_eq!(
        std::fs::read_to_string(&policy)?,
        super::set::GEMINI_CLI_DENY_ALL_POLICY
    );
    let provider = set.provider(resolved(
        "gemini-test",
        Family::GoogleGeminiCli,
        "gemini-3-pro",
    ))?;
    let events = collect(
        provider
            .open(
                SessionId::new_v7(),
                &request(
                    Family::GoogleGeminiCli,
                    "gemini-3-pro",
                    vec![user_text("Say hello.")],
                ),
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?,
    )
    .await?;
    let texts: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, "Hello world");
    assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
    Ok(())
}

#[tokio::test]
async fn gemini_cli_models_lists_configured_row_without_catalog() -> TestResult {
    let dir = TestDir::new()?;
    let mut gem_entry = entry(
        "gemini-models",
        Family::GoogleGeminiCli,
        String::from("https://example.com"),
        None,
        2,
        Transport::Https,
    );
    gem_entry.auth = AuthStyle::None;
    let env = EnvSnapshot::test(&[]).with_cwd(dir.path().to_path_buf());
    let set = ProviderSet::new(
        &config(vec![gem_entry], 0),
        identity(),
        env,
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved(
        "gemini-models",
        Family::GoogleGeminiCli,
        "gemini-3-pro",
    ))?;
    let models = provider.models().await?;
    assert_eq!(models.len(), 1, "one configured row, no catalog fetch");
    assert!(matches!(
        models[0].route,
        ModelRoute::Api {
            family: Family::GoogleGeminiCli,
            ..
        }
    ));
    Ok(())
}

#[tokio::test]
async fn gemini_cli_compact_is_unsupported() -> TestResult {
    let dir = TestDir::new()?;
    let mut gem_entry = entry(
        "gemini-compact",
        Family::GoogleGeminiCli,
        String::from("https://example.com"),
        None,
        2,
        Transport::Https,
    );
    gem_entry.auth = AuthStyle::None;
    let env = EnvSnapshot::test(&[]).with_cwd(dir.path().to_path_buf());
    let set = ProviderSet::new(
        &config(vec![gem_entry], 0),
        identity(),
        env,
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved(
        "gemini-compact",
        Family::GoogleGeminiCli,
        "gemini-3-pro",
    ))?;
    let mut compact_request = request(
        Family::GoogleGeminiCli,
        "gemini-3-pro",
        vec![user_text("Summarize.")],
    );
    compact_request.purpose = Purpose::Compact;
    let outcome = provider
        .compact(
            SessionId::new_v7(),
            &compact_request,
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    assert_eq!(outcome, Some(CompactOutcome::Unsupported));
    Ok(())
}

fn compact_request(model: &str) -> ModelRequest {
    let mut compact = request(Family::Responses, model, vec![user_text("Summarize.")]);
    compact.purpose = Purpose::Compact;
    compact
}

#[tokio::test]
async fn azure_and_copilot_routed_entries_compact_locally_without_any_request() -> TestResult {
    // The `openai` row's remote-compaction capability describes api.openai.com.
    // Pointed at an Azure or Copilot surface it routes turns through that
    // adapter, and neither offers the plain `/responses/compact` wire.
    type EntryShape = fn(&mut ProviderEntry);
    let shapes: [(&str, EntryShape); 2] = [
        ("openai", |entry| entry.auth = AuthStyle::ApiKey),
        ("github-copilot-responses", |_| {}),
    ];
    for (id, shape) in shapes {
        let dir = TestDir::new()?;
        let (base, mut server_task) = server("/v1", vec![stream_reply(String::new())]).await?;
        let mut routed = entry(
            id,
            Family::Responses,
            base,
            Some("ROUTED_KEY"),
            1,
            Transport::Https,
        );
        shape(&mut routed);
        let set = ProviderSet::new(
            &config(vec![routed], 0),
            identity(),
            EnvSnapshot::test(&[("ROUTED_KEY", "routed-secret")]),
            dir.path(),
            dir.path(),
        )?;
        let provider = set.provider(resolved(id, Family::Responses, "gpt-5.4"))?;
        let outcome = provider
            .compact(
                SessionId::new_v7(),
                &compact_request("gpt-5.4"),
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?;
        assert_eq!(outcome, Some(CompactOutcome::Unsupported), "{id}");
        assert!(
            tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
                .await
                .is_err(),
            "{id} compaction reached the wire"
        );
        server_task.abort_all();
    }
    Ok(())
}

#[tokio::test]
async fn a_plain_responses_entry_still_compacts_remotely() -> TestResult {
    // The control for the gate above: the same row with the bearer style
    // posts to the plain compaction endpoint, so the local-only outcome
    // comes from the Azure and Copilot shapes, not from the capability gate.
    let dir = TestDir::new()?;
    let body = br#"{"output":[{"type":"compaction","encrypted_content":"opaque"}]}"#;
    let (base, mut server_task) = server(
        "/v1",
        vec![Reply {
            status: 200,
            content_type: "application/json",
            body: body.to_vec(),
        }],
    )
    .await?;
    let plain = entry(
        "openai",
        Family::Responses,
        base,
        Some("PLAIN_KEY"),
        1,
        Transport::Https,
    );
    let set = ProviderSet::new(
        &config(vec![plain], 0),
        identity(),
        EnvSnapshot::test(&[("PLAIN_KEY", "plain-secret")]),
        dir.path(),
        dir.path(),
    )?;
    let provider = set.provider(resolved("openai", Family::Responses, "gpt-5.4"))?;
    let outcome = provider
        .compact(
            SessionId::new_v7(),
            &compact_request("gpt-5.4"),
            notice_sink(),
            &CancellationToken::new(),
        )
        .await?;
    let Some(CompactOutcome::Compacted(history)) = outcome else {
        return Err(io::Error::other(format!("not compacted: {outcome:?}")).into());
    };
    assert_eq!(history.family, Family::Responses);
    assert_eq!(history.items.len(), 1);
    let captured = server_task.join_next().await.expect("server completes")??;
    let head = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
    assert!(head.starts_with("post /v1/responses/compact "), "{head}");
    assert!(
        head.contains("authorization: bearer plain-secret"),
        "{head}"
    );
    Ok(())
}

#[tokio::test]
async fn google_vertex_binds_without_a_key_and_reads_the_captured_bearer_token() -> TestResult {
    // Without a key or stored sign-in the Vertex row binds credential-less:
    // each attempt reads the pre-minted bearer token from the captured
    // environment. The adapter always overrides the base with its location
    // host, so a loopback cannot serve the success path; a route refusal
    // proves the token was accepted, and the typed credential refusal
    // proves a missing token still fails closed.
    let dir = TestDir::new()?;
    let (base, mut server_task) = server("", vec![stream_reply(google_reply())]).await?;
    let cases = [
        (
            EnvSnapshot::test(&[("GOOGLE_CLOUD_ACCESS_TOKEN", "ya29-env")]),
            true,
        ),
        (EnvSnapshot::test(&[]), false),
    ];
    for (env, holds_token) in cases {
        let vertex_entry = entry(
            "google-vertex",
            Family::GoogleVertex,
            base.clone(),
            None,
            1,
            Transport::Https,
        );
        let set = ProviderSet::new(
            &config(vec![vertex_entry], 0),
            identity(),
            env,
            dir.path(),
            dir.path(),
        )?;
        let provider = set.provider(resolved("google-vertex", Family::GoogleVertex, "v-test"))?;
        let stream = provider
            .open(
                SessionId::new_v7(),
                &request(
                    Family::GoogleVertex,
                    "v-test",
                    vec![user_text("Say hello.")],
                ),
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?;
        let events = collect(stream).await;
        if holds_token {
            assert!(
                matches!(&events, Err(ProviderError::InvalidRequest { .. })),
                "{events:?}"
            );
        } else {
            assert!(
                matches!(&events, Err(ProviderError::NoCredentials { .. })),
                "{events:?}"
            );
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server_task.join_next())
            .await
            .is_err()
    );
    server_task.abort_all();
    Ok(())
}

#[tokio::test]
async fn oauth_only_families_are_ready_only_with_a_stored_oauth_sign_in() -> TestResult {
    // These adapters refuse an API key, so a key from the store or the
    // captured environment must not read as a usable credential source.
    // `auth.json` only holds members for built-in rows: the OAuth login sits
    // under an OAuth row, the keys under a key row.
    let cases = [
        (
            "factory-droid",
            Family::GoogleAntigravity,
            Some(stored_workos_login("ya29-test")),
            true,
        ),
        (
            "factory-droid",
            Family::Droid,
            Some(stored_workos_login("workos-test")),
            true,
        ),
        ("factory-droid", Family::Droid, None, false),
        (
            "groq",
            Family::GoogleAntigravity,
            Some(api_key_credential("stored-key")),
            false,
        ),
        ("groq", Family::GoogleAntigravity, None, false),
    ];
    let cancel = CancellationToken::new();
    for (id, family, stored, ready) in cases {
        let dir = TestDir::new()?;
        if let Some(credential) = stored {
            let mut store = crate::auth::credential::AuthStore::empty(dir.path().join("auth.json"));
            store.set(id, credential)?;
            store.store()?;
        }
        let oauth_only = entry(
            id,
            family,
            String::from("https://api.example.com"),
            Some("AG_KEY"),
            1,
            Transport::Https,
        );
        let set = ProviderSet::new(
            &config(vec![oauth_only], 0),
            identity(),
            EnvSnapshot::test(&[("AG_KEY", "env-key")]),
            dir.path(),
            dir.path(),
        )?;
        assert_eq!(
            set.credential_source_available(id, &cancel).await?,
            ready,
            "{id} as {family:?}"
        );
    }
    Ok(())
}

fn end_session_tool_call() -> ContextItem {
    use dal_core::{AssistantPart, CallId, ReplaySource};

    ContextItem::Assistant {
        source: ReplaySource {
            family: Family::GitLab,
            model: "duo-agent".into(),
        },
        parts: vec![AssistantPart::ToolCall {
            call: CallId::new("r-1"),
            name: "read".into(),
            args: RawJson::parse(r#"{"q":"x"}"#).unwrap(),
            replay: None,
        }],
        replay: None,
    }
}

fn end_session_turn_request(context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GitLab,
            model: "duo-agent".into(),
        },
        system: "Be terse.".into(),
        tools: vec![ModelToolSpec {
            name: "read".into(),
            description: "reads".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).unwrap(),
            grammar: None,
            defer_loading: false,
        }]
        .into(),
        context: context.into(),
        params: RequestParams {
            thinking: ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    }
}

async fn serve_end_session_workflows(ws: TcpListener) {
    use crate::family::gitlab::workflow::support::action_frame;
    use futures::{SinkExt, StreamExt};

    // First socket: the parked turn. Read START, deliver the tool action,
    // then hold until the client closes on end_session.
    let (tcp, _) = ws.accept().await.unwrap();
    let mut parked = tokio_tungstenite::accept_async(tcp).await.unwrap();
    let start = parked.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(start.contains("\"startRequest\""));
    parked
        .send(tokio_tungstenite::tungstenite::Message::Text(
            action_frame("r-1").into(),
        ))
        .await
        .unwrap();
    while parked.next().await.is_some() {}
    // Second socket: the fresh workflow after end_session.
    let (tcp, _) = ws.accept().await.unwrap();
    let mut fresh = tokio_tungstenite::accept_async(tcp).await.unwrap();
    let start = fresh.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(start.contains("\"workflowID\":\"w-2\""), "{start}");
    fresh
        .send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"status":"FINISHED"}"#.into(),
        ))
        .await
        .unwrap();
    while fresh.next().await.is_some() {}
}

#[tokio::test]
async fn provider_end_session_drops_its_parked_gitlab_workflows() -> TestResult {
    use crate::family::gitlab::workflow::support::{RestCounts, rest_router, test_timeouts};
    use crate::family::gitlab::workflow::{WorkflowScope, WorkflowTurn, run_turn};
    use std::sync::Mutex;

    // Parks a workflow row in the set's own table through a real tool-action
    // turn, then ends the session through the public ProviderSet call. The
    // next turn must create a fresh workflow (creates == 2): without the
    // forwarding line it would resume the parked row and creates would stay
    // 1. The family proof covers the table seam directly; this covers the
    // public call reaching it. Server rig is the family's shared support.
    let dir = TestDir::new()?;
    let rest = TcpListener::bind("127.0.0.1:0").await?;
    let rest_base = format!("http://{}", rest.local_addr()?);
    let ws = TcpListener::bind("127.0.0.1:0").await?;
    let ws_base = format!("http://{}", ws.local_addr()?);
    let counts = Arc::new(Mutex::new(RestCounts::default()));
    counts.lock().unwrap().ws_base = ws_base;
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(rest_router(rest, Arc::clone(&counts)));
    servers.spawn(serve_end_session_workflows(ws));
    let set = ProviderSet::new(
        &config(vec![], 0),
        identity(),
        EnvSnapshot::test(&[]),
        dir.path(),
        dir.path(),
    )?;
    let client = crate::http::build_client();
    let cancel = CancellationToken::new();
    let session = SessionId::new_v7();
    let scope = WorkflowScope {
        root_namespace_id: "42".into(),
        namespace_path: Some("g".into()),
        project_id: Some("11".into()),
        project_path: None,
        workflow_definition: None,
    };
    // First turn parks on the tool action: the ToolUse stop proves the row
    // was put back, so the creates == 1 check below is not vacuous.
    let parked_request = end_session_turn_request(vec![user_text("go"), end_session_tool_call()]);
    let turn = WorkflowTurn {
        request: &parked_request,
        model: "duo-agent",
        base_url: &rest_base,
        token: "gl-pat",
        provider: "gitlab-duo-agent",
        session,
        scope: &scope,
        tool_choice_none: false,
        timeouts: test_timeouts(),
        cancel: &cancel,
        user_agent: "dalgon/test",
    };
    let stream = run_turn(&client, &set.inner.gitlab_workflows, &turn)
        .await
        .unwrap()
        .unwrap();
    let (events, failure) = drain_stream(stream).await;
    assert!(failure.is_none(), "{failure:?}");
    assert!(
        matches!(
            events.last(),
            Some(StreamEvent::Stop {
                reason: crate::stream::StopReason::ToolUse
            })
        ),
        "{events:?}"
    );
    assert_eq!(counts.lock().unwrap().creates, 1);
    // Ending the session through the public call must drop the parked row:
    // the next turn starts a fresh workflow instead of resuming it. The
    // timeout turns a missing forward into a failure instead of a hang.
    set.end_session(&session);
    let fresh_request = end_session_turn_request(vec![user_text("go again")]);
    let turn = WorkflowTurn {
        request: &fresh_request,
        model: "duo-agent",
        base_url: &rest_base,
        token: "gl-pat",
        provider: "gitlab-duo-agent",
        session,
        scope: &scope,
        tool_choice_none: false,
        timeouts: test_timeouts(),
        cancel: &cancel,
        user_agent: "dalgon/test",
    };
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        run_turn(&client, &set.inner.gitlab_workflows, &turn),
    )
    .await
    .map_err(|_| io::Error::other("the fresh turn never completed"))?
    .unwrap()
    .unwrap();
    let (events, failure) = drain_stream(outcome).await;
    assert!(failure.is_none(), "{failure:?}");
    assert!(
        matches!(
            events.last(),
            Some(StreamEvent::Stop {
                reason: crate::stream::StopReason::EndTurn
            })
        ),
        "{events:?}"
    );
    assert_eq!(counts.lock().unwrap().creates, 2);
    servers.abort_all();
    Ok(())
}

/// A Compatible-preset `groq` row overridden to the Bedrock family: the
/// stored `auth.json` key counts as a bearer source without any ambient
/// AWS material, so the row is ready while the ambient probe is empty.
fn bedrock_override_set(
    dir: &TestDir,
    providers: Vec<ProviderEntry>,
    env: EnvSnapshot,
) -> Result<ProviderSet, Box<dyn Error>> {
    Ok(ProviderSet::new(
        &config(providers, 0),
        identity(),
        env,
        dir.path(),
        dir.path(),
    )?)
}

fn bedrock_override_entry() -> ProviderEntry {
    entry(
        "groq",
        Family::Bedrock,
        String::from("https://api.groq.com/openai/v1"),
        None,
        2,
        Transport::Https,
    )
}

fn store_groq_key(dir: &TestDir) -> Result<(), Box<dyn Error>> {
    let mut store = crate::auth::credential::AuthStore::empty(dir.path().join("auth.json"));
    store.set("groq", api_key_credential("groq-stored"))?;
    store.store()?;
    Ok(())
}

#[tokio::test]
async fn bedrock_stored_key_counts_without_ambient_sources() -> TestResult {
    let dir = TestDir::new()?;
    store_groq_key(&dir)?;
    // Inserted out of order on purpose: the batch must still report stable
    // id order. `openai` holds no credential and reads as not ready.
    let set = bedrock_override_set(
        &dir,
        vec![
            entry(
                "openai",
                Family::Responses,
                String::from("https://api.openai.com/v1"),
                None,
                2,
                Transport::Https,
            ),
            bedrock_override_entry(),
        ],
        bedrock_env(&[]),
    )?;
    let cancel = CancellationToken::new();
    assert!(
        set.credential_source_available("groq", &cancel).await?,
        "stored bearer must win with no ambient source"
    );
    assert!(
        !set.credential_source_available("openai", &cancel).await?,
        "a keyless row is a clean miss, not an error"
    );
    let rows = set.provider_readiness(&cancel).await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(&*rows[0].provider, "groq");
    assert_eq!(rows[0].family, Family::Bedrock);
    assert!(rows[0].ready);
    assert_eq!(&*rows[1].provider, "openai");
    assert!(!rows[1].ready);
    Ok(())
}

#[tokio::test]
async fn bedrock_without_any_source_is_a_clean_miss() -> TestResult {
    let dir = TestDir::new()?;
    let set = bedrock_override_set(&dir, vec![bedrock_override_entry()], bedrock_env(&[]))?;
    let cancel = CancellationToken::new();
    assert!(
        !set.credential_source_available("groq", &cancel).await?,
        "no bearer and no ambient source reads as false, not an error"
    );
    let rows = set.provider_readiness(&cancel).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].family, Family::Bedrock);
    assert!(!rows[0].ready);
    Ok(())
}

#[tokio::test]
async fn precancelled_readiness_is_cancelled_never_false() -> TestResult {
    let dir = TestDir::new()?;
    let set = bedrock_override_set(&dir, vec![bedrock_override_entry()], bedrock_env(&[]))?;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = set
        .credential_source_available("groq", &cancel)
        .await
        .expect_err("pre-cancelled check must fail loud");
    assert!(
        matches!(error, ProviderError::Cancelled),
        "pre-cancelled check must not read as false: {error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_empty_batch_is_cancelled_never_empty() -> TestResult {
    let dir = TestDir::new()?;
    let set = bedrock_override_set(&dir, Vec::new(), bedrock_env(&[]))?;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = set
        .provider_readiness(&cancel)
        .await
        .expect_err("cancelled empty batch must fail loud");
    assert!(
        matches!(error, ProviderError::Cancelled),
        "cancelled empty batch must not read as ready-less success: {error:?}"
    );
    let rows = set.provider_readiness(&CancellationToken::new()).await?;
    assert_eq!(rows, [] as [crate::provider::set::ProviderReadiness; 0]);
    Ok(())
}

/// One `openrouter` entry bound to a loopback base, through the same entry
/// construction as the configured set: the row's own def supplies the
/// dual-dispatch identity.
fn openrouter_set(
    dir: &TestDir,
    base: String,
    env: &[(&str, &str)],
) -> Result<ProviderSet, Box<dyn Error>> {
    let openrouter = entry(
        "openrouter",
        Family::Responses,
        base,
        Some("OPENROUTER_API_KEY"),
        2,
        Transport::Https,
    );
    ProviderSet::new(
        &config(vec![openrouter], 0),
        identity(),
        EnvSnapshot::test(env),
        dir.path(),
        dir.path(),
    )
    .map_err(Into::into)
}

/// The openrouter row streams its Responses surface by default and its Chat
/// Completions surface only when the captured environment names `"0"`
/// exactly: any other captured value keeps Responses.
#[tokio::test]
async fn openrouter_streams_responses_by_default_and_chat_only_when_the_env_names_zero()
-> TestResult {
    let dir = TestDir::new()?;
    let key = ("OPENROUTER_API_KEY", "or-secret");
    for (label, extra, expected_path, reply, marker, absent) in [
        (
            "no captured value",
            Vec::new(),
            "/v1/responses",
            responses_reply(),
            "\"instructions\"",
            "\"messages\"",
        ),
        (
            "`0`",
            vec![("DAL_OPENROUTER_RESPONSES", "0")],
            "/v1/chat/completions",
            chat_reply(),
            "\"messages\"",
            "\"instructions\"",
        ),
        (
            "another value",
            vec![("DAL_OPENROUTER_RESPONSES", "1")],
            "/v1/responses",
            responses_reply(),
            "\"instructions\"",
            "\"messages\"",
        ),
    ] {
        let (base, mut captured) = server("/v1", vec![stream_reply(reply)]).await?;
        let mut env = vec![key];
        env.extend(extra);
        let set = openrouter_set(&dir, base, &env)?;
        let provider = set.provider(resolved("openrouter", Family::Responses, "openai/gpt-5.5"))?;
        let request = request(
            Family::Responses,
            "openai/gpt-5.5",
            vec![user_text("Say hello.")],
        );
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
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello")),
            "{label} streamed no text"
        );
        let request = captured.join_next().await.expect("server completes")??;
        let head = String::from_utf8_lossy(&request[0]).to_ascii_lowercase();
        assert!(
            head.starts_with(&format!("post {expected_path} ")),
            "{label}: {head}"
        );
        assert!(
            head.contains("authorization: bearer or-secret"),
            "{label} sent no bearer key: {head}"
        );
        let body = String::from_utf8_lossy(request_body(&request[0])).into_owned();
        assert!(
            body.contains(marker),
            "{label} body misses {marker}: {body}"
        );
        assert!(
            !body.contains(absent),
            "{label} body carries {absent}: {body}"
        );
    }
    Ok(())
}
#[tokio::test]
async fn dispatch_verify_oauth_only_none_refuses_before_network() -> TestResult {
    // Adapted from the supplied Droid bind/turn smoke: admission now refuses
    // both OAuth-only families. Accept observes the first network effect,
    // rather than waiting for a server task to finish a whole HTTP exchange.
    for family in [Family::Droid, Family::GoogleAntigravity] {
        for auth in [AuthStyle::None, AuthStyle::Bearer] {
            let dir = TestDir::new()?;
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let mut row = entry(
                "factory-droid",
                family,
                format!("http://{}", listener.local_addr()?),
                None,
                1,
                Transport::Https,
            );
            row.auth = auth;
            let set = ProviderSet::new(
                &config(vec![row.clone()], 0),
                identity(),
                EnvSnapshot::test(&[]),
                dir.path(),
                dir.path(),
            )?;
            let cancel = CancellationToken::new();
            assert!(
                !set.credential_source_available("factory-droid", &cancel)
                    .await?
            );
            assert!(!set.provider_readiness(&cancel).await?[0].ready);
            assert!(matches!(
                set.provider(resolved("factory-droid", family, "model")),
                Err(ProviderError::NoCredentials { provider }) if provider == "factory-droid"
            ));
            // Also exercise direct construction, which otherwise bypasses
            // the set's credential resolver for a caller-supplied None.
            assert!(matches!(
                Provider::new(
                    resolved("factory-droid", family, "model"),
                    row,
                    Credential::None,
                    &reqwest::Client::new(),
                    &set,
                ),
                Err(ProviderError::NoCredentials { provider }) if provider == "factory-droid"
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(25), listener.accept())
                    .await
                    .is_err(),
                "refused {family:?} admission must never connect"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn dispatch_verify_oauth_signin_and_optional_auth_admission() -> TestResult {
    for family in [Family::Droid, Family::GoogleAntigravity] {
        let dir = TestDir::new()?;
        let mut store = crate::auth::credential::AuthStore::empty(dir.path().join("auth.json"));
        store.set(
            "factory-droid",
            stored_workos_login("admission-only-access"),
        )?;
        store.store()?;
        let mut row = entry(
            "factory-droid",
            family,
            String::from("http://127.0.0.1:1"),
            None,
            1,
            Transport::Https,
        );
        row.auth = AuthStyle::Bearer;
        let set = ProviderSet::new(
            &config(vec![row.clone()], 0),
            identity(),
            EnvSnapshot::test(&[]),
            dir.path(),
            dir.path(),
        )?;
        assert!(
            set.credential_source_available("factory-droid", &CancellationToken::new())
                .await?
        );
        assert!(
            set.provider(resolved("factory-droid", family, "model"))
                .is_ok()
        );
        // Explicit none still disables the stored login; do not silently
        // recover it as a new credential source to admit an unusable row.
        row.auth = AuthStyle::None;
        let disabled = ProviderSet::new(
            &config(vec![row], 0),
            identity(),
            EnvSnapshot::test(&[]),
            dir.path(),
            dir.path(),
        )?;
        assert!(matches!(
            disabled.credential("factory-droid")?,
            Credential::None
        ));
        assert!(matches!(
            disabled.provider(resolved("factory-droid", family, "model")),
            Err(ProviderError::NoCredentials { .. })
        ));
    }
    for family in [Family::Chat, Family::Ollama, Family::GoogleGeminiCli] {
        let dir = TestDir::new()?;
        let mut row = entry(
            "local-admission",
            family,
            String::from("http://127.0.0.1:1"),
            None,
            1,
            Transport::Https,
        );
        row.auth = AuthStyle::None;
        let set = ProviderSet::new(
            &config(vec![row], 0),
            identity(),
            EnvSnapshot::test(&[]),
            dir.path(),
            dir.path(),
        )?;
        assert!(
            set.credential_source_available("local-admission", &CancellationToken::new())
                .await?
        );
        assert!(
            set.provider(resolved("local-admission", family, "model"))
                .is_ok()
        );
    }
    Ok(())
}
