//! Workflow surface tests: builders, frame decoding, and the socket driver
//! against loopback servers. Frames below replay captured shapes.

use std::sync::{Arc, Mutex};

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RawJson,
    ReplaySource, RequestParams, SessionId, ThinkingLevel,
};
use futures::{SinkExt, StreamExt};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::*;
use crate::{
    error::ProviderError,
    http,
    stream::{EventStream, StopReason, StreamEvent},
};
use tokio_util::sync::CancellationToken;

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn text_part(text: &str) -> Part {
    Part::Text { text: text.into() }
}

fn user(text: &str) -> ContextItem {
    ContextItem::User {
        parts: vec![text_part(text)],
    }
}

fn request(context: Vec<ContextItem>) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::GitLab,
            model: "duo-agent".into(),
        },
        system: "Be terse.".into(),
        tools: vec![dal_core::ModelToolSpec {
            name: "read".into(),
            description: "reads".into(),
            parameters: raw(r#"{"type":"object"}"#),
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

fn scope() -> WorkflowScope {
    WorkflowScope {
        root_namespace_id: "42".into(),
        namespace_path: Some("g".into()),
        project_id: Some("11".into()),
        project_path: None,
        workflow_definition: None,
    }
}

fn turn<'a>(
    request: &'a ModelRequest,
    scope: &'a WorkflowScope,
    cancel: &'a CancellationToken,
) -> WorkflowTurn<'a> {
    WorkflowTurn {
        request,
        model: "duo-agent",
        base_url: "https://gitlab.com",
        token: "gl-pat",
        provider: "gitlab-duo-agent",
        session: SessionId::new_v7(),
        scope,
        tool_choice_none: false,
        timeouts: Timeouts::default(),
        cancel,
        user_agent: "dalgon/test",
    }
}

#[test]
fn namespace_forms_convert_both_ways() {
    assert_eq!(graphql_namespace_id("42"), "gid://gitlab/Group/42");
    assert_eq!(
        graphql_namespace_id("gid://gitlab/Group/42"),
        "gid://gitlab/Group/42"
    );
    assert_eq!(rest_namespace_id("gid://gitlab/Group/42"), "42");
    assert_eq!(rest_namespace_id("gid://gitlab/Namespace/7"), "7");
    assert_eq!(rest_namespace_id("g"), "g");
}

#[test]
fn socket_url_routes_service_and_instance() {
    let cancel = CancellationToken::new();
    let request = request(vec![user("hi")]);
    let scope = scope();
    let turn = turn(&request, &scope, &cancel);
    let direct = Connection {
        token: String::new(),
        base_url: String::new(),
        headers: Vec::new(),
        service_endpoint: false,
    };
    let url = socket_url(&turn, &direct, Some("11"), "claude-opus-4-6", "ambient").unwrap();
    assert_eq!(
        url,
        "wss://gitlab.com/api/v4/ai/duo_workflows/ws?project_id=11&namespace_id=42&root_namespace_id=42&user_selected_model_identifier=claude-opus-4-6&workflow_definition=ambient"
    );
    let service = Connection {
        token: String::new(),
        base_url: "https://dws.example.com/runway".into(),
        headers: Vec::new(),
        service_endpoint: true,
    };
    let url = socket_url(&turn, &service, None, "m", "ambient").unwrap();
    assert!(url.starts_with("wss://dws.example.com/runway/?"));
    assert!(socket_url(&turn, &direct, None, "m", "ambient").is_ok());
}

#[test]
fn socket_headers_carry_the_pinned_identity() {
    let cancel = CancellationToken::new();
    let request = request(vec![user("hi")]);
    let scope = scope();
    let turn = turn(&request, &scope, &cancel);
    let connection = Connection {
        token: "wtok".into(),
        base_url: String::new(),
        headers: vec![(String::from("x-extra"), String::from("1"))],
        service_endpoint: false,
    };
    let headers = socket_headers(&turn, &connection, Some("11"));
    let get = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap()
    };
    assert_eq!(get("authorization"), "Bearer wtok");
    assert_eq!(get("x-gitlab-client-type"), "node-websocket");
    assert_eq!(get("x-gitlab-language-server-version"), "8.104.0");
    assert!(get("user-agent").contains("gitlab-language-server/8.104.0"));
    assert_eq!(get("origin"), "https://gitlab.com");
    assert_eq!(get("x-gitlab-project-id"), "11");
    assert_eq!(get("x-gitlab-namespace-id"), "42");
    assert_eq!(get("x-gitlab-root-namespace-id"), "42");
    assert_eq!(get("x-extra"), "1");
}

#[test]
fn goal_renders_bare_for_a_lone_turn() {
    let request = request(vec![user("do it")]);
    let (goal, multiturn) = render_goal(&request, &[]).unwrap();
    assert_eq!(goal, "do it");
    assert!(!multiturn);
}

#[test]
fn goal_renders_chatml_with_ran_records() {
    let call = ContextItem::Assistant {
        source: ReplaySource {
            family: Family::GitLab,
            model: "duo-agent".into(),
        },
        parts: vec![
            AssistantPart::Text {
                text: "on it".into(),
                replay: None,
            },
            AssistantPart::ToolCall {
                call: CallId::new("c1"),
                name: "read".into(),
                args: raw(r#"{"q":"x"}"#),
                replay: None,
            },
        ],
        replay: None,
    };
    let result = ContextItem::ToolResult {
        call: CallId::new("c1"),
        name: "read".into(),
        is_error: true,
        parts: vec![text_part("nope")],
    };
    let request = request(vec![user("go"), call, result, user("again")]);
    let (goal, multiturn) = render_goal(&request, &[]).unwrap();
    assert!(multiturn);
    assert!(goal.contains("<|im_start|>user\ngo\n<|im_end|>"));
    assert!(goal.contains(r#"<ran read>{"q":"x"}</ran>"#));
    assert!(goal.contains("<ran:result status=error>\nnope"));
    assert!(goal.ends_with("<|im_start|>user\nagain\n<|im_end|>"));
}

#[test]
fn start_frame_carries_the_inline_flow() {
    let parts = StartParts {
        workflow_id: "w-1".into(),
        definition: "ambient".into(),
        goal: "do it".into(),
        metadata: r#"{"environment":"ide"}"#.into(),
        system: "sys".into(),
        tools_json: "[]".into(),
        preapproved_json: "[]".into(),
    };
    let frame = render_start(&parts).unwrap();
    assert!(frame.ends_with('\n'));
    let value: sonic_rs::Value = sonic_rs::from_str(&frame).unwrap();
    // The server only accepts the payload nested under `startRequest`.
    let start = &value["startRequest"];
    assert_eq!(start["workflowID"].as_str().unwrap(), "w-1");
    assert_eq!(start["clientVersion"].as_str().unwrap(), "1.0");
    assert_eq!(start["goal"].as_str().unwrap(), "do it");
    assert!(start.get("approval").is_none());
    // Metadata rides as a JSON string the server parses itself.
    let metadata =
        sonic_rs::from_str::<sonic_rs::Value>(start["workflowMetadata"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["environment"].as_str().unwrap(), "ide");
    assert_eq!(
        start["flowConfig"]["prompts"][0]["prompt_template"]["user"]
            .as_str()
            .unwrap(),
        "{{goal}}"
    );
    assert_eq!(
        start["flowConfig"]["components"][0]["ui_log_events"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(start["clientCapabilities"].as_array().unwrap().len(), 5);
    let approval = render_approval_start(&parts).unwrap();
    let value: sonic_rs::Value = sonic_rs::from_str(&approval).unwrap();
    let start = &value["startRequest"];
    assert_eq!(start["goal"].as_str().unwrap(), "");
    assert!(start.get("approval").is_some());
    assert!(start.get("flowConfig").is_none());
    // Tool schemas ride as JSON strings, matching `mcpTools[].inputSchema`.
    let tooled = request(vec![user("hi")]);
    let (tools_json, _) = render_tools(&tooled, false).unwrap();
    let parts = StartParts {
        workflow_id: "w-1".into(),
        definition: "ambient".into(),
        goal: "do it".into(),
        metadata: r#"{"environment":"ide"}"#.into(),
        system: "sys".into(),
        tools_json,
        preapproved_json: "[]".into(),
    };
    let frame = render_start(&parts).unwrap();
    let value: sonic_rs::Value = sonic_rs::from_str(&frame).unwrap();
    let schema = value["startRequest"]["mcpTools"][0]["inputSchema"]
        .as_str()
        .unwrap();
    let schema: sonic_rs::Value = sonic_rs::from_str(schema).unwrap();
    assert_eq!(schema["type"].as_str().unwrap(), "object");
}

#[tokio::test]
async fn overflow_message_names_the_byte_budget() {
    // An oversized goal fails before spending any request, as a typed
    // overflow the shared classifier recognizes by its code; the message
    // names the goal bytes against the soft and hard budgets.
    let goal = "x".repeat(GOAL_HARD_BYTES);
    let request = request(vec![user(&goal)]);
    let scope = scope();
    let cancel = CancellationToken::new();
    let turn = turn(&request, &scope, &cancel);
    let client = http::build_client();
    let sessions = WorkflowSessions::default();
    let Err(crate::lifecycle::AttemptFailure::Provider(error)) =
        run_turn(&client, &sessions, &turn).await
    else {
        panic!("oversized goal must fail before spending a request");
    };
    match &error {
        ProviderError::ContextOverflow { code, message, .. } => {
            assert_eq!(code, "context_length_exceeded");
            let budget = GOAL_HARD_BYTES.to_string();
            let soft = GOAL_SOFT_BYTES.to_string();
            assert!(message.contains(budget.as_str()));
            assert!(message.contains(soft.as_str()));
            assert!(
                ProviderError::context_overflow(Family::GitLab, code, message).is_some(),
                "{error:?}"
            );
        }
        _ => panic!("oversized goal must be a context overflow: {error:?}"),
    }
}

#[test]
fn token_extraction_follows_the_pinned_precedence() {
    let rails = br#"{"gitlab_rails":{"token":"rails"},"token":"other"}"#;
    assert_eq!(parse_connection(rails).unwrap().token, "rails");
    let service =
        br#"{"duo_workflow_service":{"token":"svc","base_url":"https://dws.example.com/x"}}"#;
    let connection = parse_connection(service).unwrap();
    assert_eq!(connection.token, "svc");
    assert!(connection.service_endpoint);
    assert!(parse_connection(br"{}").is_err());
}

#[test]
fn checkpoint_snapshots_emit_only_deltas() {
    let mut decoder = Decoder::new(CheckpointState::default(), None);
    let mut out = Vec::new();
    // The wire carries the checkpoint as a JSON-encoded string, per the
    // pinned shape; quoting passes through `sonic_rs::to_string`.
    let snapshot = |text: &str| {
        let content = sonic_rs::to_string(text).unwrap_or_default();
        let inner = ["{\"channel_values\":{\"ui_chat_log\":[{\"message_type\":\"agent\",\"message_id\":\"m1\",\"content\":", content.as_str(), "}]}}"].concat();
        let checkpoint = sonic_rs::to_string(inner.as_str()).unwrap_or_default();
        [
            "{\"newCheckpoint\":{\"checkpoint\":",
            checkpoint.as_str(),
            "}}",
        ]
        .concat()
    };
    let frame = snapshot("hello");
    assert!(matches!(
        decoder.feed(&frame, &mut out),
        Ok(FrameOutcome::Continue)
    ));
    assert_eq!(out.len(), 1);
    assert!(matches!(&out[0], StreamEvent::TextDelta { text } if text == "hello"));
    out.clear();
    assert!(matches!(
        decoder.feed(&frame, &mut out),
        Ok(FrameOutcome::Continue)
    ));
    assert_eq!(out, []);
    let grown = snapshot("hello world");
    assert!(matches!(
        decoder.feed(&grown, &mut out),
        Ok(FrameOutcome::Continue)
    ));
    assert!(matches!(&out[0], StreamEvent::TextDelta { text } if text == " world"));
}

#[test]
fn reasoning_subtype_emits_reasoning_deltas() {
    let mut decoder = Decoder::new(CheckpointState::default(), None);
    let mut out = Vec::new();
    let inner = "{\"channel_values\":{\"ui_chat_log\":[{\"message_type\":\"agent\",\"message_id\":\"r1\",\"message_sub_type\":\"reasoning\",\"content\":\"why\"}]}}";
    let checkpoint = sonic_rs::to_string(inner).unwrap_or_default();
    let frame = [
        "{\"newCheckpoint\":{\"checkpoint\":",
        checkpoint.as_str(),
        "}}",
    ]
    .concat();
    assert!(matches!(
        decoder.feed(&frame, &mut out),
        Ok(FrameOutcome::Continue)
    ));
    assert!(matches!(&out[0], StreamEvent::ReasoningDelta { text } if text == "why"));
}

#[test]
fn statuses_map_to_terminal_approval_and_restart() {
    let mut decoder = Decoder::new(CheckpointState::default(), None);
    let mut out = Vec::new();
    assert!(matches!(
        decoder.feed(r#"{"status":"FINISHED"}"#, &mut out),
        Ok(FrameOutcome::Terminal)
    ));
    assert!(matches!(
        decoder.feed(r#"{"status":"TOOL_CALL_APPROVAL_REQUIRED"}"#, &mut out),
        Ok(FrameOutcome::Approval)
    ));
    assert!(matches!(
        decoder.feed(r#"{"status":"FAILED","message":"The workflow reached its maximum step limit and could not complete"}"#, &mut out),
        Ok(FrameOutcome::StepLimit)
    ));
    assert!(matches!(
        decoder.feed(r#"{"status":"FAILED","message":"There was an error processing your request in the Duo Agent Platform, try again"}"#, &mut out),
        Ok(FrameOutcome::Retryable(_))
    ));
    assert!(matches!(
        decoder.feed(r#"{"status":"STOPPED","message":"nope"}"#, &mut out),
        Ok(FrameOutcome::Failed(_))
    ));
    assert!(matches!(
        decoder.feed("not json", &mut out),
        Ok(FrameOutcome::Continue)
    ));
}

#[test]
fn actions_require_request_id_and_detect_stalls() {
    let mut decoder = Decoder::new(CheckpointState::default(), None);
    let mut out = Vec::new();
    // A `runMCPTool` wrapper names the envelope; the dal tool rides in its
    // args, and the first boundary has no prior reading to stall against.
    let action = |id: &str| {
        format!(
            r#"{{"action":{{"name":"runMCPTool","requestID":"{id}","args":{{"name":"read"}}}}}}"#
        )
    };
    assert!(matches!(
        decoder.feed(&action("r-1"), &mut out),
        Ok(FrameOutcome::Action(act)) if act.name.as_str() == "read"
    ));
    // A bare action carries no checkpoint, so a second bare boundary still
    // has no comparable reading and is not a stall.
    assert!(matches!(
        decoder.feed(&action("r-2"), &mut out),
        Ok(FrameOutcome::Action(_))
    ));
    let missing = r#"{"action":{"name":"runMCPTool","args":{}}}"#;
    assert!(matches!(
        decoder.feed(missing, &mut out),
        Err(ProviderError::Protocol { .. })
    ));
    // A fresh boundary whose checkpoint bytes repeat the previous boundary
    // is a stall; a boundary that advances the bytes is not.
    let snapshot = |text: &str| {
        let content = sonic_rs::to_string(text).unwrap_or_default();
        let inner = ["{\"channel_values\":{\"ui_chat_log\":[{\"message_type\":\"agent\",\"message_id\":\"m1\",\"content\":", content.as_str(), "}]}}"].concat();
        let checkpoint = sonic_rs::to_string(inner.as_str()).unwrap_or_default();
        [
            "{\"newCheckpoint\":{\"checkpoint\":",
            checkpoint.as_str(),
            "}}",
        ]
        .concat()
    };
    assert!(matches!(
        decoder.feed(&snapshot("hello"), &mut out),
        Ok(FrameOutcome::Continue)
    ));
    assert!(matches!(
        decoder.feed(&action("r-3"), &mut out),
        Ok(FrameOutcome::Action(_))
    ));
    assert!(matches!(
        decoder.feed(&snapshot("hello"), &mut out),
        Ok(FrameOutcome::Continue)
    ));
    assert!(matches!(
        decoder.feed(&action("r-4"), &mut out),
        Ok(FrameOutcome::Stalled)
    ));
    assert!(matches!(
        decoder.feed(&snapshot("hello world"), &mut out),
        Ok(FrameOutcome::Continue)
    ));
    assert!(matches!(
        decoder.feed(&action("r-5"), &mut out),
        Ok(FrameOutcome::Action(_))
    ));
}

#[test]
fn usage_reports_the_chat_agent_first() {
    let mut decoder = Decoder::new(CheckpointState::default(), None);
    let mut out = Vec::new();
    // Usage rides with a checkpoint holder on the wire; the chat agent wins
    // even when another agent's entry comes first.
    let usage = "{\"Other\":{\"total_tokens\":1,\"max_tokens\":2},\"Chat Agent\":{\"total_tokens\":40,\"max_tokens\":100}}";
    let inner = "{\"channel_values\":{\"ui_chat_log\":[]}}";
    let checkpoint = sonic_rs::to_string(inner).unwrap_or_default();
    let frame = [
        "{\"newCheckpoint\":{\"agent_context_usage\":",
        usage,
        ",\"checkpoint\":",
        checkpoint.as_str(),
        "}}",
    ]
    .concat();
    assert!(matches!(
        decoder.feed(&frame, &mut out),
        Ok(FrameOutcome::Continue)
    ));
    assert_eq!(decoder.context_used, Some(40));
}

// --- Loopback driver tests ------------------------------------------------

#[derive(Debug, Default)]
struct RestCounts {
    creates: usize,
    stops: Vec<String>,
    direct_access: usize,
    ws_base: String,
}

async fn read_http(tcp: &mut tokio::net::TcpStream) -> (String, String, Vec<u8>) {
    let mut head = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        AsyncReadExt::read_exact(tcp, &mut byte).await.unwrap();
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head_text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head_text.lines();
    let start = lines.next().unwrap_or_default().to_owned();
    let len = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("content-length") {
                return None;
            }
            value.trim().parse::<usize>().ok()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    AsyncReadExt::read_exact(tcp, &mut body).await.unwrap();
    (start, head_text, body)
}

async fn reply(tcp: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    AsyncWriteExt::write_all(tcp, head.as_bytes())
        .await
        .unwrap();
    AsyncWriteExt::write_all(tcp, body.as_bytes())
        .await
        .unwrap();
}

async fn rest_router(listener: TcpListener, counts: Arc<Mutex<RestCounts>>) {
    loop {
        let Ok((mut tcp, _)) = listener.accept().await else {
            return;
        };
        let counts = Arc::clone(&counts);
        let (start, _, _) = read_http(&mut tcp).await;
        let mut parts = start.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        let ws_base = lock(&counts).ws_base.clone();
        let body = if method == "PUT" {
            r"{}".to_owned()
        } else if path.starts_with("/api/v4/groups/") && path.contains("/projects") {
            r#"[{"id":11,"path_with_namespace":"g/p"}]"#.to_owned()
        } else if path.starts_with("/api/v4/ai/duo_workflows/direct_access") {
            lock(&counts).direct_access += 1;
            format!(r#"{{"duo_workflow_service":{{"token":"wtok","base_url":"{ws_base}"}}}}"#)
        } else if path.starts_with("/api/v4/ai/duo_workflows/workflows/") && method == "PATCH" {
            let id = path.rsplit('/').next().unwrap_or_default().to_owned();
            lock(&counts).stops.push(id);
            r"{}".to_owned()
        } else if path.starts_with("/api/v4/ai/duo_workflows/workflows") {
            let mut counts = lock(&counts);
            counts.creates += 1;
            format!(r#"{{"id":"w-{}"}}"#, counts.creates)
        } else if path.starts_with("/api/graphql") {
            r#"{"data":{"aiChatAvailableModels":{"pinnedModel":null}}}"#.to_owned()
        } else {
            r"{}".to_owned()
        };
        reply(&mut tcp, "200 OK", &body).await;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn test_timeouts() -> Timeouts {
    Timeouts {
        rest: std::time::Duration::from_secs(5),
        idle: std::time::Duration::from_millis(100),
    }
}

fn test_turn<'a>(
    request: &'a ModelRequest,
    scope: &'a WorkflowScope,
    cancel: &'a CancellationToken,
    base: &'a str,
    session: SessionId,
) -> WorkflowTurn<'a> {
    WorkflowTurn {
        request,
        model: "duo-agent",
        base_url: base,
        token: "gl-pat",
        provider: "gitlab-duo-agent",
        session,
        scope,
        tool_choice_none: false,
        timeouts: test_timeouts(),
        cancel,
        user_agent: "dalgon/test",
    }
}

async fn drain(mut stream: EventStream) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(error) => return (events, Some(error)),
        }
    }
    (events, None)
}

#[tokio::test]
async fn loopback_turn_streams_text_to_a_terminal_stop() {
    let rest = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rest_base = format!("http://{}", rest.local_addr().unwrap());
    let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_base = format!("http://{}", ws.local_addr().unwrap());
    let counts = Arc::new(Mutex::new(RestCounts::default()));
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(rest_router(rest, Arc::clone(&counts)));
    servers.spawn(async move {
        let (tcp, _) = ws.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let start = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(start.contains("\"startRequest\""));
        assert!(start.contains("\"workflowID\":\"w-1\""));
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"newCheckpoint":{"checkpoint":"{\"channel_values\":{\"ui_chat_log\":[{\"message_type\":\"agent\",\"message_id\":\"a1\",\"content\":\"hello\"}]}}"}}"#.into(),
            ))
            .await
            .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"status":"FINISHED"}"#.into(),
            ))
            .await
            .unwrap();
        // Keep the socket until the client finishes.
        while socket.next().await.is_some() {}
    });
    // Point the socket at the WS listener through the service endpoint.
    lock(&counts).ws_base = ws_base;
    let request = request(vec![user("hi")]);
    let scope = WorkflowScope {
        root_namespace_id: "42".into(),
        namespace_path: Some("g".into()),
        project_id: Some("11".into()),
        project_path: None,
        workflow_definition: None,
    };
    let cancel = CancellationToken::new();
    let session = SessionId::new_v7();
    let sessions = WorkflowSessions::default();
    let client = http::build_client();
    let turn = test_turn(&request, &scope, &cancel, &rest_base, session);
    let stream = run_turn(&client, &sessions, &turn).await.unwrap().unwrap();
    let (events, error) = drain(stream).await;
    assert!(error.is_none());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "hello"))
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
    let counts = lock(&counts);
    assert_eq!(counts.direct_access, 1);
    assert_eq!(counts.creates, 1);
    assert_eq!(counts.stops, [] as [String; 0]);
    drop(counts);
    servers.abort_all();
}

fn action_frame(id: &str) -> String {
    format!(
        r#"{{"action":{{"name":"runMCPTool","requestID":"{id}","args":{{"name":"read","args":{{"q":"x"}}}}}}}}"#
    )
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "loopback action fixture tests full flow"
)]
async fn loopback_action_resumes_with_the_tool_result() {
    let rest = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rest_base = format!("http://{}", rest.local_addr().unwrap());
    let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_base = format!("http://{}", ws.local_addr().unwrap());
    let counts = Arc::new(Mutex::new(RestCounts::default()));
    lock(&counts).ws_base = ws_base;
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(rest_router(rest, Arc::clone(&counts)));
    servers.spawn(async move {
        let (tcp, _) = ws.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let start = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(start.contains("\"startRequest\""));
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(action_frame("r-1").into()))
            .await
            .unwrap();
        // The resume answers on this same socket with the echoed requestID.
        let answer = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(answer.contains("\"requestID\":\"r-1\""));
        assert!(answer.contains("file text"));
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"newCheckpoint":{"checkpoint":"{\"channel_values\":{\"ui_chat_log\":[{\"message_type\":\"agent\",\"message_id\":\"a2\",\"content\":\"used it\"}]}}"}}"#.into(),
            ))
            .await
            .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"status":"FINISHED"}"#.into(),
            ))
            .await
            .unwrap();
        while socket.next().await.is_some() {}
    });
    let cancel = CancellationToken::new();
    let session = SessionId::new_v7();
    let sessions = WorkflowSessions::default();
    let client = http::build_client();
    let scope = WorkflowScope {
        root_namespace_id: "42".into(),
        namespace_path: Some("g".into()),
        project_id: Some("11".into()),
        project_path: None,
        workflow_definition: None,
    };
    let tool_call = || ContextItem::Assistant {
        source: ReplaySource {
            family: Family::GitLab,
            model: "duo-agent".into(),
        },
        parts: vec![AssistantPart::ToolCall {
            call: CallId::new("r-1"),
            name: "read".into(),
            args: raw(r#"{"q":"x"}"#),
            replay: None,
        }],
        replay: None,
    };
    let model_request = request(vec![user("go"), tool_call()]);
    let turn = test_turn(&model_request, &scope, &cancel, &rest_base, session);
    let stream = run_turn(&client, &sessions, &turn).await.unwrap().unwrap();
    let (events, error) = drain(stream).await;
    assert!(error.is_none());
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::ToolUse
        })
    ));
    let started = events.iter().any(|event| {
        matches!(
            event,
            StreamEvent::ToolCallStarted { id, name } if id == "r-1" && name == "read"
        )
    });
    assert!(started);
    // Second turn: the tool result resumes the same socket.
    let result = ContextItem::ToolResult {
        call: CallId::new("r-1"),
        name: "read".into(),
        is_error: false,
        parts: vec![text_part("file text")],
    };
    let resumed_request = request(vec![user("go"), tool_call(), result]);
    let turn = test_turn(&resumed_request, &scope, &cancel, &rest_base, session);
    let stream = run_turn(&client, &sessions, &turn).await.unwrap().unwrap();
    let (events, error) = drain(stream).await;
    assert!(error.is_none());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "used it"))
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
    let counts = lock(&counts);
    assert_eq!(counts.creates, 1);
    assert_eq!(counts.stops, [] as [String; 0]);
    drop(counts);
    servers.abort_all();
}

#[tokio::test]
async fn loopback_idle_restart_uses_a_fresh_workflow_once() {
    let rest = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rest_base = format!("http://{}", rest.local_addr().unwrap());
    let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_base = format!("http://{}", ws.local_addr().unwrap());
    let counts = Arc::new(Mutex::new(RestCounts::default()));
    lock(&counts).ws_base = ws_base;
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(rest_router(rest, Arc::clone(&counts)));
    servers.spawn(async move {
        // First socket: stay open but silent past the 100 ms test idle
        // deadline, so the client restarts on a fresh workflow.
        let (tcp, _) = ws.accept().await.unwrap();
        let mut first = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let start = first.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(start.contains("\"workflowID\":\"w-1\""));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        // Second socket: the fresh workflow completes.
        let (tcp, _) = ws.accept().await.unwrap();
        let mut second = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let start = second.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(start.contains("\"workflowID\":\"w-2\""));
        second
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"status":"FINISHED"}"#.into(),
            ))
            .await
            .unwrap();
        while second.next().await.is_some() {}
    });
    let request = request(vec![user("hi")]);
    let scope = WorkflowScope {
        root_namespace_id: "42".into(),
        namespace_path: Some("g".into()),
        project_id: Some("11".into()),
        project_path: None,
        workflow_definition: None,
    };
    let cancel = CancellationToken::new();
    let sessions = WorkflowSessions::default();
    let client = http::build_client();
    let turn = test_turn(&request, &scope, &cancel, &rest_base, SessionId::new_v7());
    let stream = run_turn(&client, &sessions, &turn).await.unwrap().unwrap();
    let (events, error) = drain(stream).await;
    assert!(error.is_none());
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
    let counts = lock(&counts);
    assert_eq!(counts.creates, 2);
    assert_eq!(counts.stops, vec![String::from("w-1")]);
    drop(counts);
    servers.abort_all();
}

#[test]
fn start_frame_ignores_text_tool_and_message_replay() {
    let assistant = |replay: fn() -> Option<RawJson>| ContextItem::Assistant {
        source: ReplaySource {
            family: Family::GitLab,
            model: "duo-agent".into(),
        },
        replay: replay(),
        parts: vec![
            AssistantPart::Text {
                text: "on it".into(),
                replay: replay(),
            },
            AssistantPart::ToolCall {
                call: CallId::new("c1"),
                name: "read".into(),
                args: raw(r#"{"q":"x"}"#),
                replay: replay(),
            },
        ],
    };
    let payload = || Some(raw(r#"{"foreign":"REPLAY-MARKER"}"#));
    let frame = |replay: fn() -> Option<RawJson>| {
        let built = request(vec![user("go"), assistant(replay), user("again")]);
        let (goal, _) = render_goal(&built, &[]).unwrap();
        let parts = StartParts {
            workflow_id: "w-1".into(),
            definition: "ambient".into(),
            goal,
            metadata: r#"{"environment":"ide"}"#.into(),
            system: "sys".into(),
            tools_json: render_tools(&built, false).unwrap().0,
            preapproved_json: "[]".into(),
        };
        render_start(&parts).unwrap()
    };
    let laden = frame(payload);
    assert_eq!(laden, frame(|| None));
    assert!(!laden.contains("REPLAY-MARKER"), "{laden}");
}
