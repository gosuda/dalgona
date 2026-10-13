//! The `initialize` answer capabilities gate approvals and asks per kind.
//!
//! A connection that declares `approval` subscribes as an approval answerer;
//! one that declares the ask role, spelled `ask` or `question`, subscribes
//! as an ask answerer. Without the matching declaration the subscription is
//! listen-only: an approval is denied at once and an extension question
//! takes its default at once.

use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::super::support::{
    Rpc, ask_probe_extension, denied_result_text, result, result_text, rig_with_extensions,
    text_step, tool_step,
};
use super::{open, prompt, with_rpc};
use dal_core::Expect;
/// Runs `initialize` with exactly `caps` and returns the result body.
async fn initialize_with(rpc: &mut Rpc, caps: &[&str]) -> Value {
    let capabilities: Vec<Value> = caps.iter().map(|name| Value::from(*name)).collect();
    let reply = rpc
        .call(
            0,
            "initialize",
            sonic_rs::json!({
                "protocolVersion": 1,
                "clientInfo": {"name": "probe", "version": "1"},
                "capabilities": capabilities,
            }),
        )
        .await;
    result(&reply).clone()
}

/// Sets session approval mode to ask without persisting the default.
async fn set_approval_ask(rpc: &mut Rpc, id: i64, session: &str) {
    let reply = rpc
        .call(
            id,
            "session/submit",
            sonic_rs::json!({
                "sessionId": session,
                "command": {"type": "set_approval", "mode": "ask", "save": "session_only"},
            }),
        )
        .await;
    result(&reply);
}

/// Subscribes to `session` and consumes the subscribe reply.
async fn subscribe(rpc: &mut Rpc, id: i64, session: &str) {
    rpc.send(
        id,
        "session/subscribe",
        sonic_rs::json!({"sessionId": session}),
    )
    .await;
    let reply = rpc.next().await;
    assert_eq!(reply["id"].as_i64(), Some(id), "{reply}");
    result(&reply);
}

/// Reads updates until the turn ends. A request opening means the connection
/// answered without declaring the matching capability, so fail at once.
async fn updates_until_end(rpc: &mut Rpc) {
    loop {
        let frame = rpc.next().await;
        if frame["method"].as_str() != Some("session/update") {
            continue;
        }
        let kind = frame["params"]["update"]["type"]
            .as_str()
            .unwrap_or("?")
            .to_owned();
        assert_ne!(
            kind, "request_opened",
            "a request opened without the matching capability: {frame}"
        );
        if kind == "turn_ended" {
            return;
        }
    }
}

/// Reads updates until one request opens and returns its id.
async fn next_request_id(rpc: &mut Rpc) -> String {
    loop {
        let frame = rpc.next().await;
        if frame["method"].as_str() != Some("session/update") {
            continue;
        }
        let update = &frame["params"]["update"];
        if update["type"].as_str() == Some("request_opened") {
            return update["id"].as_str().expect("request id").to_owned();
        }
    }
}

/// Answers one open request and consumes the answer reply.
async fn answer(rpc: &mut Rpc, id: i64, session: &str, request: &str, answer: Value) {
    let reply = rpc
        .call(
            id,
            "session/answer",
            sonic_rs::json!({
                "sessionId": session,
                "requestId": request,
                "answer": answer,
            }),
        )
        .await;
    result(&reply);
}

#[tokio::test]
async fn approval_without_capability_denies_at_once() {
    let steps = [tool_step("c1", "ask"), text_step(&["done"], 1, 1)];
    let rig = rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        initialize_with(&mut rpc, &["sessions"]).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        set_approval_ask(&mut rpc, 2, &session).await;
        subscribe(&mut rpc, 3, &session).await;
        rpc.send(
            4,
            "session/submit",
            sonic_rs::json!({"sessionId": session, "command": prompt("edit", Expect::Idle)}),
        )
        .await;
        updates_until_end(&mut rpc).await;
        let denial = denied_result_text(&rig, &session, "ask").await;
        assert_eq!(
            dal_core::parse_headless_denial(&denial).map(|(tool, _)| tool),
            Some("ask"),
            "{denial}"
        );
    })
    .await;
}

#[tokio::test]
async fn ask_without_capability_defaults_at_once() {
    let steps = [tool_step("c1", "probe_ask"), text_step(&["done"], 1, 1)];
    let rig = rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        initialize_with(&mut rpc, &["sessions"]).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        subscribe(&mut rpc, 2, &session).await;
        rpc.send(
            3,
            "session/submit",
            sonic_rs::json!({"sessionId": session, "command": prompt("hi", Expect::Idle)}),
        )
        .await;
        updates_until_end(&mut rpc).await;
        assert_eq!(
            result_text(&rig, &session, "probe_ask", false).await,
            "default"
        );
    })
    .await;
}

#[tokio::test]
async fn approval_with_capabilities_waits_for_the_client_answer() {
    let steps = [tool_step("c1", "ask"), text_step(&["done"], 1, 1)];
    let rig = rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        initialize_with(&mut rpc, &["sessions", "approval", "ask"]).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        set_approval_ask(&mut rpc, 2, &session).await;
        subscribe(&mut rpc, 3, &session).await;
        rpc.send(
            4,
            "session/submit",
            sonic_rs::json!({"sessionId": session, "command": prompt("edit", Expect::Idle)}),
        )
        .await;
        let request = next_request_id(&mut rpc).await;
        answer(
            &mut rpc,
            5,
            &session,
            &request,
            sonic_rs::json!({"type": "approve"}),
        )
        .await;
        updates_until_end(&mut rpc).await;
        assert_eq!(result_text(&rig, &session, "ask", false).await, "asked-ok");
    })
    .await;
}

#[tokio::test]
async fn question_only_client_answers_asks_but_denies_approvals() {
    let steps = [
        tool_step("c1", "ask"),
        text_step(&["done"], 1, 1),
        tool_step("c2", "probe_ask"),
        text_step(&["done"], 1, 1),
    ];
    let rig = rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        let negotiated = initialize_with(&mut rpc, &["sessions", "question"]).await;
        let caps: Vec<&str> = negotiated["capabilities"]
            .as_array()
            .expect("capability list")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(caps, ["sessions"]);
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        set_approval_ask(&mut rpc, 2, &session).await;
        subscribe(&mut rpc, 3, &session).await;
        rpc.send(
            4,
            "session/submit",
            sonic_rs::json!({"sessionId": session, "command": prompt("edit", Expect::Idle)}),
        )
        .await;
        updates_until_end(&mut rpc).await;
        let denial = denied_result_text(&rig, &session, "ask").await;
        assert_eq!(
            dal_core::parse_headless_denial(&denial).map(|(tool, _)| tool),
            Some("ask"),
            "{denial}"
        );
        rpc.send(
            6,
            "session/submit",
            sonic_rs::json!({"sessionId": session, "command": prompt("hi", Expect::Idle)}),
        )
        .await;
        let request = next_request_id(&mut rpc).await;
        answer(
            &mut rpc,
            7,
            &session,
            &request,
            sonic_rs::json!({"type": "value", "value": "yes"}),
        )
        .await;
        updates_until_end(&mut rpc).await;
        assert_eq!(
            result_text(&rig, &session, "probe_ask", false).await,
            "answered"
        );
    })
    .await;
}
