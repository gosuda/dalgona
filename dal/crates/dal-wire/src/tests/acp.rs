use std::time::Duration;

use std::sync::Arc;

use dal_agent::ext::command::{CommandCx, CommandHandler};
use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_agent::ext::{BoxFuture, ExtensionBuilder};
use dal_agent::{HostUpdate, ServiceError, SessionRef};
use dal_core::{
    ClientId, CommandName, CommandSpec, ModelInfo, Name, RawJson, Reply, ServiceSet, ToolClass,
    ToolSpec, Visibility, Workspace,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::support::{
    Rig, Rpc, assert_error, gate_step, result, rig, rig_with_extensions, text_step, tool_step,
};
use crate::serve_acp;
use crate::transport::MemoryTransport;

/// Runs `client` against `serve_acp` on a memory transport.
async fn with_acp<F>(rig: &Rig, client: impl FnOnce(Rpc) -> F)
where
    F: Future<Output = ()>,
{
    let (transport, peer) = MemoryTransport::pair(64);
    let server = serve_acp(rig.host.clone(), transport);
    let (outcome, ()) = tokio::join!(server, client(Rpc::new(peer)));
    outcome.expect("acp serve ends cleanly");
}

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn v1_init_frame() -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":1,"agentCapabilities":{{"loadSession":true,"promptCapabilities":{{"image":true,"audio":false,"embeddedContext":true}},"mcpCapabilities":{{"http":false,"sse":false}},"sessionCapabilities":{{"list":{{}},"resume":{{}},"close":{{}}}}}},"authMethods":[],"agentInfo":{{"name":"dal","title":"dal","version":"{}"}}}}}}"#,
        version()
    )
}

fn v2_init_frame() -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":2,"capabilities":{{"session":{{"prompt":{{"image":{{}},"embeddedContext":{{}}}}}}}},"info":{{"name":"dal","title":"dal","version":"{}"}},"authMethods":[]}}}}"#,
        version()
    )
}

/// Sends `initialize` with `version` and returns the raw reply frame.
async fn init(acp: &mut Rpc, version: i64) -> String {
    acp.send(
        1,
        "initialize",
        sonic_rs::json!({"protocolVersion": version, "clientInfo": {"name": "zed"}}),
    )
    .await;
    acp.raw().await
}

/// Opens one session and returns its id.
async fn new_session(acp: &mut Rpc, id: i64, cwd: &str) -> String {
    let reply = acp
        .call(
            id,
            "session/new",
            sonic_rs::json!({"cwd": cwd, "mcpServers": []}),
        )
        .await;
    result(&reply)["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned()
}

fn prompt_params(session: &str, text: &str) -> Value {
    sonic_rs::json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]})
}

fn update_kind(frame: &Value) -> Option<&str> {
    frame["params"]["update"]["sessionUpdate"].as_str()
}

#[tokio::test]
async fn acp_v1_prompt_flow() {
    let rig = rig(&[text_step(&["Hel", "lo"], 10, 5)]).await;
    let ws = rig.ws();
    with_acp(&rig, async |mut acp| {
        assert_eq!(init(&mut acp, 1).await, v1_init_frame());
        let session = new_session(&mut acp, 2, &ws).await;
        let commands = acp.next().await;
        assert_eq!(
            commands["method"].as_str(),
            Some("session/update"),
            "{commands}"
        );
        let listed = commands["params"]["update"]["available_commands_update"]["availableCommands"]
            .as_array()
            .expect("commands update follows session/new");
        assert!(
            listed
                .iter()
                .any(|row| row["name"].as_str() == Some("mode")),
            "{commands}"
        );
        acp.send(3, "session/prompt", prompt_params(&session, "hi"))
            .await;
        let mut chunks = String::new();
        let reply = loop {
            let frame = acp.next().await;
            if frame["id"].as_i64() == Some(3) {
                break frame;
            }
            if update_kind(&frame) == Some("agent_message_chunk") {
                chunks.push_str(
                    frame["params"]["update"]["content"]["text"]
                        .as_str()
                        .expect("chunk text"),
                );
            }
        };
        assert_eq!(chunks, "Hello");
        assert_eq!(
            result(&reply)["stopReason"].as_str(),
            Some("end_turn"),
            "{reply}"
        );
        acp.assert_quiet(Duration::from_millis(200)).await;
    })
    .await;
}

#[tokio::test]
async fn acp_v2_prompt_order() {
    let rig = rig(&[text_step(&["Hel", "lo"], 10, 5)]).await;
    let ws = rig.ws();
    with_acp(&rig, async |mut acp| {
        assert_eq!(init(&mut acp, 2).await, v2_init_frame());
        let session = new_session(&mut acp, 2, &ws).await;
        acp.send(3, "session/prompt", prompt_params(&session, "hi"))
            .await;
        let mut frames = Vec::new();
        loop {
            let frame = acp.next().await;
            let idle = frame["params"]["update"]["state"].as_str() == Some("idle");
            frames.push(frame);
            if idle {
                break;
            }
        }
        let accepted = frames
            .iter()
            .position(|frame| frame["id"].as_i64() == Some(3))
            .expect("prompt reply");
        let message_id = result(&frames[accepted])["messageId"]
            .as_str()
            .expect("message id")
            .to_owned();
        let running = frames
            .iter()
            .position(|frame| frame["params"]["update"]["state"].as_str() == Some("running"))
            .expect("running state");
        assert!(accepted < running, "reply after running: {frames:?}");
        let echoed = frames
            .iter()
            .find(|frame| update_kind(frame) == Some("user_message"))
            .expect("user_message");
        assert_eq!(
            echoed["params"]["update"]["messageId"].as_str(),
            Some(message_id.as_str())
        );
        let idle = frames.last().expect("idle state");
        assert_eq!(
            idle["params"]["update"]["stopReason"].as_str(),
            Some("end_turn"),
            "{idle}"
        );
    })
    .await;
    with_acp(&rig, async |mut acp| {
        let frame = init(&mut acp, 7).await;
        let reply: Value = sonic_rs::from_str(&frame).expect("init json");
        assert_eq!(result(&reply)["protocolVersion"].as_i64(), Some(2));
    })
    .await;
}

#[tokio::test]
async fn acp_cwd_mcp_refusals() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    with_acp(&rig, async |mut acp| {
        init(&mut acp, 1).await;
        let relative = acp
            .call(
                2,
                "session/new",
                sonic_rs::json!({"cwd": "rel/w", "mcpServers": []}),
            )
            .await;
        assert_error(
            &relative,
            -32602,
            "invalid params for session/new: cwd must be an absolute path",
        );
        let servers = sonic_rs::json!([
            {"name": "a", "command": "a", "args": [], "env": []},
            {"name": "b", "command": "b", "args": [], "env": []},
        ]);
        let reply = acp
            .call(
                3,
                "session/new",
                sonic_rs::json!({"cwd": ws, "mcpServers": servers}),
            )
            .await;
        result(&reply);
        let notice = loop {
            let frame = acp.next().await;
            if let Some(notice) = frame["params"]["update"].get("_dal/notice") {
                break notice.clone();
            }
        };
        assert_eq!(
            notice["text"].as_str(),
            Some("dalgon does not load MCP servers: 2 ignored")
        );
    })
    .await;
}

#[tokio::test]
async fn batch_prompt_rejected() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    with_acp(&rig, async |mut acp| {
        init(&mut acp, 2).await;
        let session = new_session(&mut acp, 2, &ws).await;
        let batch = sonic_rs::json!([
            {"jsonrpc": "2.0", "id": 5, "method": "session/prompt", "params": prompt_params(&session, "hi")},
            {"jsonrpc": "2.0", "id": 6, "method": "session/list", "params": {}},
        ]);
        acp.send_raw(&sonic_rs::to_string(&batch).expect("batch json"))
            .await;
        let replies: Vec<Value> = loop {
            let frame = acp.raw().await;
            if frame.starts_with('[') {
                break sonic_rs::from_str(&frame).expect("batch reply json");
            }
        };
        assert_eq!(replies.len(), 2, "{replies:?}");
        let prompt = replies
            .iter()
            .find(|reply| reply["id"].as_i64() == Some(5))
            .expect("prompt entry");
        assert_error(prompt, -32600, "session/prompt may not appear in a batch");
        let list = replies
            .iter()
            .find(|reply| reply["id"].as_i64() == Some(6))
            .expect("list entry");
        result(list);
    })
    .await;
    with_acp(&rig, async |mut acp| {
        init(&mut acp, 1).await;
        acp.send_raw(r#"[{"jsonrpc":"2.0","id":2,"method":"session/list","params":{}}]"#)
            .await;
        let reply = acp.next().await;
        assert!(reply["id"].is_null(), "{reply}");
        assert_error(&reply, -32600, "batches are not supported");
    })
    .await;
}

#[tokio::test]
async fn session_list_takes_null_filters_and_refuses_wrong_types() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    for version in [1, 2] {
        with_acp(&rig, async |mut acp| {
            init(&mut acp, version).await;
            let session = new_session(&mut acp, 2, &ws).await;
            let nulls = acp
                .call(
                    3,
                    "session/list",
                    sonic_rs::json!({"cwd": null, "cursor": null}),
                )
                .await;
            let listed = result(&nulls)["sessions"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["sessionId"].as_str() == Some(session.as_str()))
            });
            assert!(listed, "{nulls}");
            let cursor = acp
                .call(4, "session/list", sonic_rs::json!({"cursor": 7}))
                .await;
            assert_error(
                &cursor,
                -32602,
                "invalid params for session/list: member `cursor` must be a string",
            );
            let cwd = acp
                .call(5, "session/list", sonic_rs::json!({"cwd": ["x"]}))
                .await;
            assert_error(
                &cwd,
                -32602,
                "invalid params for session/list: member `cwd` must be a string",
            );
        })
        .await;
    }
}

#[tokio::test]
async fn transport_end_cancels() {
    let rig = rig(&[gate_step("c1"), text_step(&["late"], 1, 1)]).await;
    let ws = rig.ws();
    let mut host_updates = rig.host.subscribe();
    let mut opened = String::new();
    with_acp(&rig, async |mut acp| {
        init(&mut acp, 2).await;
        opened = new_session(&mut acp, 2, &ws).await;
        acp.send(3, "session/prompt", prompt_params(&opened, "wait"))
            .await;
        loop {
            let frame = acp.next().await;
            if frame["params"]["update"]["toolCallId"].as_str() == Some("c1") {
                break;
            }
        }
    })
    .await;
    let removed = loop {
        let update = tokio::time::timeout(super::support::WAIT, host_updates.next())
            .await
            .expect("host update in time")
            .expect("host subscription open");
        if let HostUpdate::SessionRemoved { session } = update {
            break session.to_string();
        }
    };
    assert_eq!(removed, opened);
    assert_eq!(rig.gate.available_permits(), 0);
    let agent = rig
        .host
        .open(
            SessionRef::Resume {
                key: opened.as_str().into(),
                workspace: rig.core_workspace(),
            },
            ClientId::new("observer"),
        )
        .await
        .expect("closed session resumes");
    let view = agent.view(dal_core::PageReq::default()).expect("view");
    assert!(
        matches!(view.turn, dal_core::TurnState::Idle),
        "turn survived transport end: {:?}",
        view.turn
    );
}

/// A slash command whose handler panics.
struct PanicCommand;

impl CommandHandler for PanicCommand {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        _cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async { panic!("command handler panicked") })
    }
}

/// A tool whose call panics.
struct PanicTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl PanicTool {
    fn new() -> Self {
        let name = Name::parse("explode").expect("tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Panics when called.".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("schema json"),
            grammar: None,
        });
        Self { name, spec }
    }
}

impl Tool for PanicTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { panic!("tool panicked") })
    }
}

fn panic_extension() -> dal_agent::ext::Extension {
    ExtensionBuilder::new("panics", "0.0.0", ServiceSet::EMPTY)
        .expect("extension name")
        .command(
            CommandSpec {
                name: CommandName::parse("boom").expect("command name"),
                summary: "Panics when run.".into(),
                args_hint: None,
            },
            Arc::new(PanicCommand),
        )
        .tool(Arc::new(PanicTool::new()), Visibility::Model)
        .build()
        .expect("panic extension builds")
}

/// Reads frames until the reply to `id`, skipping notifications.
async fn reply_to(acp: &mut Rpc, id: i64) -> Value {
    loop {
        let frame = acp.next().await;
        if frame["id"].as_i64() == Some(id) {
            return frame;
        }
    }
}

/// Proves the connection still serves a fresh session and prompt.
async fn assert_connection_serves(acp: &mut Rpc, ws: &str, session_id: i64, prompt_id: i64) {
    let session = new_session(acp, session_id, ws).await;
    acp.send(prompt_id, "session/prompt", prompt_params(&session, "hi"))
        .await;
    let reply = reply_to(acp, prompt_id).await;
    assert_eq!(
        result(&reply)["stopReason"].as_str(),
        Some("end_turn"),
        "{reply}"
    );
}

#[tokio::test]
async fn acp_command_panic_answers_error_and_connection_survives() {
    let rig = rig_with_extensions(&[text_step(&["ok"], 1, 1)], "", vec![panic_extension()]).await;
    let ws = rig.ws();
    with_acp(&rig, async |mut acp| {
        init(&mut acp, 1).await;
        let session = new_session(&mut acp, 2, &ws).await;
        acp.send(3, "session/prompt", prompt_params(&session, "/boom"))
            .await;
        let reply = reply_to(&mut acp, 3).await;
        assert!(
            reply["error"]["code"].as_i64().is_some(),
            "a panicking command must answer an error: {reply}"
        );
        let text = reply["error"].to_string();
        assert!(
            text.contains("boom") && text.contains("crashed"),
            "the error names the command and the crash: {text}"
        );
        assert_connection_serves(&mut acp, &ws, 4, 5).await;
    })
    .await;
    rig.host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn acp_tool_panic_reaches_the_client_and_connection_survives() {
    let rig = rig_with_extensions(
        &[
            tool_step("p1", "explode"),
            text_step(&["ok"], 1, 1),
            text_step(&["again"], 1, 1),
        ],
        "",
        vec![panic_extension()],
    )
    .await;
    let ws = rig.ws();
    with_acp(&rig, async |mut acp| {
        init(&mut acp, 1).await;
        let session = new_session(&mut acp, 2, &ws).await;
        acp.send(3, "session/prompt", prompt_params(&session, "go"))
            .await;
        let mut seen = String::new();
        let reply = loop {
            let frame = acp.next().await;
            if frame["id"].as_i64() == Some(3) {
                break frame;
            }
            seen.push_str(&frame.to_string());
        };
        assert_eq!(
            result(&reply)["stopReason"].as_str(),
            Some("end_turn"),
            "{reply}"
        );
        assert!(
            seen.contains("explode tool crashed") && seen.contains("tool panicked"),
            "the client sees the crash text: {seen}"
        );
        assert_connection_serves(&mut acp, &ws, 4, 5).await;
    })
    .await;
    rig.host.shutdown(Duration::from_secs(1)).await;
}

/// Returns the `toolCallId` a permission request names on `version`.
fn requested_tool_call(version: i64, params: &Value) -> Option<String> {
    let call = match version {
        1 => &params["toolCall"],
        _ => &params["subject"]["toolCall"],
    };
    call["toolCallId"].as_str().map(str::to_owned)
}

/// Approves the permission request `frame` by choosing `allow`.
async fn allow_permission(acp: &Rpc, frame: &Value) {
    let answer = sonic_rs::json!({
        "jsonrpc": "2.0",
        "id": frame["id"],
        "result": {"outcome": "selected", "optionId": "allow"},
    });
    acp.send_raw(&sonic_rs::to_string(&answer).expect("answer json"))
        .await;
}

/// Returns whether `frame` ends the prompt turn: v1 replies to the prompt,
/// v2 reports the idle state.
fn prompt_finished(version: i64, frame: &Value) -> bool {
    match version {
        1 => frame["id"].as_i64() == Some(3),
        _ => frame["params"]["update"]["state"].as_str() == Some("idle"),
    }
}

/// Drives one approval-gated tool call and returns the `toolCallId` the
/// tool-call update announced and the one the permission request carries.
async fn announced_and_requested_call_ids(version: i64) -> (String, String) {
    let rig = rig(&[
        tool_step("provider-call-7", "ask"),
        text_step(&["ok"], 1, 1),
    ])
    .await;
    let ws = rig.ws();
    let mut announced = None;
    let mut requested = None;
    with_acp(&rig, async |mut acp| {
        init(&mut acp, version).await;
        let session = new_session(&mut acp, 2, &ws).await;
        acp.send(3, "session/prompt", prompt_params(&session, "go"))
            .await;
        loop {
            let frame = acp.next().await;
            if prompt_finished(version, &frame) {
                break;
            }
            if frame["method"].as_str() == Some("session/request_permission") {
                requested = requested_tool_call(version, &frame["params"]);
                allow_permission(&acp, &frame).await;
            }
            if matches!(update_kind(&frame), Some("tool_call" | "tool_call_update")) {
                announced.get_or_insert_with(|| {
                    let id = frame["params"]["update"]["toolCallId"].as_str();
                    id.expect("tool-call update names its call").to_owned()
                });
            }
        }
    })
    .await;
    rig.host.shutdown(Duration::from_secs(1)).await;
    (
        announced.expect("a tool-call update announced the call"),
        requested.expect("a permission request named a tool call"),
    )
}

#[tokio::test]
async fn acp_v1_permission_request_names_the_announced_tool_call() {
    let (announced, requested) = announced_and_requested_call_ids(1).await;
    assert_eq!(announced, "provider-call-7");
    assert_eq!(
        requested, announced,
        "the permission request must carry the provider call id"
    );
}

#[tokio::test]
async fn acp_v2_permission_request_names_the_announced_tool_call() {
    let (announced, requested) = announced_and_requested_call_ids(2).await;
    assert_eq!(announced, "provider-call-7");
    assert_eq!(
        requested, announced,
        "the permission request must carry the provider call id"
    );
}

/// Loads `session` on a v1 connection and returns the replayed update kinds.
async fn replayed_kinds(acp: &mut Rpc, id: i64, session: &str, cwd: &str) -> Vec<String> {
    acp.send(
        id,
        "session/load",
        sonic_rs::json!({"sessionId": session, "cwd": cwd, "mcpServers": []}),
    )
    .await;
    let mut kinds = Vec::new();
    loop {
        let frame = acp.next().await;
        if frame["id"].as_i64() == Some(id) {
            result(&frame);
            return kinds;
        }
        if let Some(kind) = update_kind(&frame) {
            kinds.push(kind.to_owned());
        }
    }
}

#[tokio::test]
async fn acp_new_session_never_inherits_another_threads_history() {
    let rig = rig(&[text_step(&["first thread"], 1, 1)]).await;
    let ws = rig.ws();
    with_acp(&rig, async |mut acp| {
        init(&mut acp, 1).await;
        let first = new_session(&mut acp, 2, &ws).await;
        acp.send(3, "session/prompt", prompt_params(&first, "remember me"))
            .await;
        let reply = reply_to(&mut acp, 3).await;
        assert_eq!(
            result(&reply)["stopReason"].as_str(),
            Some("end_turn"),
            "{reply}"
        );
        let second = new_session(&mut acp, 4, &ws).await;
        assert_ne!(first, second, "each session/new mints its own thread");
        let second_replay = replayed_kinds(&mut acp, 5, &second, &ws).await;
        assert!(
            !second_replay.iter().any(|kind| kind.contains("message")),
            "the new thread replayed another thread's history: {second_replay:?}"
        );
        let first_replay = replayed_kinds(&mut acp, 6, &first, &ws).await;
        assert!(
            first_replay.iter().any(|kind| kind.contains("message")),
            "the first thread keeps its own history: {first_replay:?}"
        );
    })
    .await;
    rig.host.shutdown(Duration::from_secs(1)).await;
}
