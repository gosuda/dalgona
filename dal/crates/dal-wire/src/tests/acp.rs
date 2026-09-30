use std::time::Duration;

use dal_agent::{HostUpdate, SessionRef};
use dal_core::ClientId;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::support::{Rig, Rpc, assert_error, gate_step, result, rig, text_step};
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
