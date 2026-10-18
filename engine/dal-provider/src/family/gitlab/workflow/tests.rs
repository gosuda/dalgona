//! Workflow surface tests: builders, frame decoding, and the socket driver
//! against loopback servers. Frames below replay captured shapes.

use std::sync::{Arc, Mutex};

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, Part, Purpose, RawJson,
    ReplaySource, RequestParams, SessionId, ThinkingLevel,
};
use futures::{SinkExt, StreamExt};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};
use tokio::net::TcpListener;

use super::*;
use super::support::{RestCounts, action_frame, rest_router, test_timeouts};
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
    // A second bare action with no fresh checkpoint since the last boundary
    // has nothing comparable: the snapshot reading was consumed once by
    // that boundary, so this must not stall on the stale length.
    assert!(matches!(
        decoder.feed(&action("r-6"), &mut out),
        Ok(FrameOutcome::Action(_))
    ));
    // A boundary whose own frame carries a checkpoint compares that length:
    // a repeat of the previous boundary stalls, an advanced checkpoint does
    // not, and a later bare action still does not stall.
    let checkpointed_action = |id: &str, text: &str| {
        let content = sonic_rs::to_string(text).unwrap_or_default();
        let inner = ["{\"channel_values\":{\"ui_chat_log\":[{\"message_type\":\"agent\",\"message_id\":\"m1\",\"content\":", content.as_str(), "}]}}"].concat();
        let checkpoint = sonic_rs::to_string(inner.as_str()).unwrap_or_default();
        [
            "{\"newCheckpoint\":{\"checkpoint\":",
            checkpoint.as_str(),
            "},\"action\":{\"name\":\"runMCPTool\",\"requestID\":\"",
            id,
            "\",\"args\":{\"name\":\"read\"}}}",
        ]
        .concat()
    };
    assert!(matches!(
        decoder.feed(&checkpointed_action("r-7", "hello"), &mut out),
        Ok(FrameOutcome::Action(act)) if act.name.as_str() == "read"
    ));
    assert!(matches!(
        decoder.feed(&checkpointed_action("r-8", "hello"), &mut out),
        Ok(FrameOutcome::Stalled)
    ));
    assert!(matches!(
        decoder.feed(&checkpointed_action("r-9", "hello world"), &mut out),
        Ok(FrameOutcome::Action(_))
    ));
    assert!(matches!(
        decoder.feed(&action("r-10"), &mut out),
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

// The loopback rig (router, counts, timeouts, action frames) lives in
// super::support so the provider-set boundary test can share it.

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

#[tokio::test]
async fn loopback_fresh_connect_failure_stops_the_created_workflow() {
    let rest = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rest_base = format!("http://{}", rest.local_addr().unwrap());
    // A socket base nobody listens on: the REST setup still creates `w-1`,
    // then the workflow-socket handshake fails before the first frame.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_base = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let counts = Arc::new(Mutex::new(RestCounts::default()));
    lock(&counts).ws_base = dead_base;
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(rest_router(rest, Arc::clone(&counts)));
    let cancel = CancellationToken::new();
    let sessions = WorkflowSessions::default();
    let client = http::build_client();
    let scope = WorkflowScope {
        root_namespace_id: "42".into(),
        namespace_path: Some("g".into()),
        project_id: Some("11".into()),
        project_path: None,
        workflow_definition: None,
    };
    let model_request = request(vec![user("hi")]);
    let turn = test_turn(
        &model_request,
        &scope,
        &cancel,
        &rest_base,
        SessionId::new_v7(),
    );
    // The fresh run fails before its first frame; without the cleanup the
    // created workflow would strand server-side with no terminal owning it.
    let result = run_turn(&client, &sessions, &turn).await;
    assert!(matches!(
        result,
        Err(crate::lifecycle::AttemptFailure::Provider(
            ProviderError::Transport { .. }
        ))
    ));
    let counts = lock(&counts);
    assert_eq!(counts.creates, 1);
    assert_eq!(counts.stops, vec![String::from("w-1")]);
    drop(counts);
    servers.abort_all();
}

#[tokio::test]
async fn loopback_restart_open_failure_stops_the_created_workflow() {
    let rest = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rest_base = format!("http://{}", rest.local_addr().unwrap());
    let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_base = format!("http://{}", ws.local_addr().unwrap());
    let counts = Arc::new(Mutex::new(RestCounts::default()));
    lock(&counts).ws_base = ws_base;
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(rest_router(rest, Arc::clone(&counts)));
    servers.spawn(async move {
        // First socket: accept, then stay silent past the 100 ms test idle
        // deadline so the client restarts on a fresh workflow — then drop
        // the listener so the fresh open fails instead of completing.
        let (tcp, _) = ws.accept().await.unwrap();
        let mut first = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let start = first.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(start.contains("\"workflowID\":\"w-1\""));
        drop(ws);
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        drop(first);
    });
    let model_request = request(vec![user("hi")]);
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
    let turn = test_turn(
        &model_request,
        &scope,
        &cancel,
        &rest_base,
        SessionId::new_v7(),
    );
    let stream = run_turn(&client, &sessions, &turn).await.unwrap().unwrap();
    let (events, error) = drain(stream).await;
    // The restart's fresh open fails: exactly one terminal error, and both
    // the dead workflow and the created workflow are stopped server-side.
    let Some(ProviderError::Transport { .. }) = error else {
        panic!("restart open failure must surface its transport error, got {error:?}");
    };
    assert!(events.is_empty());
    let counts = lock(&counts);
    assert_eq!(counts.creates, 2);
    assert_eq!(
        counts.stops,
        vec![String::from("w-1"), String::from("w-2")]
    );
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

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "two-session loopback fixture covers park, end, resume, and restart"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "the loopback server detaches one handler task per accepted connection"
)]
async fn loopback_end_session_drops_only_its_parked_workflows() {
    let rest = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rest_base = format!("http://{}", rest.local_addr().unwrap());
    let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_base = format!("http://{}", ws.local_addr().unwrap());
    let counts = Arc::new(Mutex::new(RestCounts::default()));
    lock(&counts).ws_base = ws_base;
    let (closed_tx, mut closed_rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(rest_router(rest, Arc::clone(&counts)));
    // Three connections serve two parked turns and one fresh resume; each
    // handler routes on the START frame it reads, so accept order is free.
    servers.spawn(async move {
        loop {
            let Ok((tcp, _)) = ws.accept().await else {
                return;
            };
            let closed_tx = closed_tx.clone();
            tokio::spawn(async move {
                let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let start = socket.next().await.unwrap().unwrap().into_text().unwrap();
                assert!(start.contains("\"startRequest\""));
                let text =
                    |body: &str| tokio_tungstenite::tungstenite::Message::Text(body.into());
                if start.contains("\"workflowID\":\"w-1\"") {
                    // Session A parks on an action; ending its session must
                    // terminate this socket together with its row.
                    socket.send(text(&action_frame("r-a"))).await.unwrap();
                    while socket.next().await.is_some() {}
                    let _ = closed_tx.send("a");
                } else if start.contains("\"workflowID\":\"w-2\"") {
                    // Session B parks, then resumes on this same socket.
                    socket.send(text(&action_frame("r-b"))).await.unwrap();
                    let answer = socket.next().await.unwrap().unwrap().into_text().unwrap();
                    assert!(answer.contains("\"requestID\":\"r-b\""));
                    assert!(answer.contains("file text B"));
                    socket
                        .send(text(
                            r#"{"newCheckpoint":{"checkpoint":"{\"channel_values\":{\"ui_chat_log\":[{\"message_type\":\"agent\",\"message_id\":\"b2\",\"content\":\"b used it\"}]}}"}}"#,
                        ))
                        .await
                        .unwrap();
                    socket.send(text(r#"{"status":"FINISHED"}"#)).await.unwrap();
                    while socket.next().await.is_some() {}
                } else {
                    // Session A's fresh resume after its rows were dropped.
                    socket
                        .send(text(
                            r#"{"newCheckpoint":{"checkpoint":"{\"channel_values\":{\"ui_chat_log\":[{\"message_type\":\"agent\",\"message_id\":\"a3\",\"content\":\"a fresh\"}]}}"}}"#,
                        ))
                        .await
                        .unwrap();
                    socket.send(text(r#"{"status":"FINISHED"}"#)).await.unwrap();
                    while socket.next().await.is_some() {}
                }
            });
        }
    });
    let cancel = CancellationToken::new();
    let session_a = SessionId::new_v7();
    let session_b = SessionId::new_v7();
    let sessions = WorkflowSessions::default();
    let client = http::build_client();
    let scope = WorkflowScope {
        root_namespace_id: "42".into(),
        namespace_path: Some("g".into()),
        project_id: Some("11".into()),
        project_path: None,
        workflow_definition: None,
    };
    let tool_call = |id: &str| ContextItem::Assistant {
        source: ReplaySource {
            family: Family::GitLab,
            model: "duo-agent".into(),
        },
        parts: vec![AssistantPart::ToolCall {
            call: CallId::new(id),
            name: "read".into(),
            args: raw(r#"{"q":"x"}"#),
            replay: None,
        }],
        replay: None,
    };
    // Two sessions park one workflow each.
    for (session, id, go) in [
        (session_a, "r-a", "go A"),
        (session_b, "r-b", "go B"),
    ] {
        let model_request = request(vec![user(go), tool_call(id)]);
        let turn = test_turn(&model_request, &scope, &cancel, &rest_base, session);
        let stream = run_turn(&client, &sessions, &turn).await.unwrap().unwrap();
        let (events, error) = drain(stream).await;
        assert!(error.is_none());
        assert!(
            matches!(
                events.last(),
                Some(StreamEvent::Stop {
                    reason: StopReason::ToolUse
                })
            )
        );
    }
    assert_eq!(lock(&sessions.inner).len(), 2);
    // Ending session A removes only its rows and terminates its socket.
    sessions.end_session(&session_a);
    assert_eq!(lock(&sessions.inner).len(), 1);
    assert!(
        lock(&sessions.inner)
            .keys()
            .all(|(owner, _)| owner != &session_a)
    );
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), closed_rx.recv())
            .await
            .expect("session A's socket stays open after end_session"),
        Some("a")
    );
    // Session B still resumes on its live socket with no fresh workflow.
    let result_b = ContextItem::ToolResult {
        call: CallId::new("r-b"),
        name: "read".into(),
        is_error: false,
        parts: vec![text_part("file text B")],
    };
    let resumed_b = request(vec![user("go B"), tool_call("r-b"), result_b]);
    let turn = test_turn(&resumed_b, &scope, &cancel, &rest_base, session_b);
    let stream = run_turn(&client, &sessions, &turn).await.unwrap().unwrap();
    let (events, error) = drain(stream).await;
    assert!(error.is_none());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "b used it"))
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
    // Session A, its rows gone, starts a fresh workflow for its tool result.
    let result_a = ContextItem::ToolResult {
        call: CallId::new("r-a"),
        name: "read".into(),
        is_error: false,
        parts: vec![text_part("file text A")],
    };
    let resumed_a = request(vec![user("go A"), tool_call("r-a"), result_a]);
    let turn = test_turn(&resumed_a, &scope, &cancel, &rest_base, session_a);
    let stream = run_turn(&client, &sessions, &turn).await.unwrap().unwrap();
    let (events, error) = drain(stream).await;
    assert!(error.is_none());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "a fresh"))
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Stop {
            reason: StopReason::EndTurn
        })
    ));
    let counts = lock(&counts);
    assert_eq!(counts.creates, 3);
    assert_eq!(counts.stops, [] as [String; 0]);
    drop(counts);
    assert_eq!(lock(&sessions.inner).len(), 0);
    // A second end is idempotent and never touches other sessions.
    sessions.end_session(&session_a);
    sessions.end_session(&session_b);
    servers.abort_all();
}
