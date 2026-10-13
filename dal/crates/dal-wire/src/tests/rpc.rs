use std::time::Duration;

use dal_core::{Command, Expect, Part, RequestId};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::support::{
    Rig, Rpc, assert_error, assert_invalid_params, initialize, result, rig, text_step,
};
use crate::serve_rpc;
use crate::transport::MemoryTransport;

mod auth;
mod backpressure;
mod capabilities;
mod concurrency;
mod drain;
mod foreign;
mod order;
mod schema;
mod turns;

/// Runs `client` against `serve_rpc` on a memory transport.
async fn with_rpc<F>(rig: &Rig, client: impl FnOnce(Rpc) -> F)
where
    F: Future<Output = ()>,
{
    let (transport, peer) = MemoryTransport::pair(64);
    let server = serve_rpc(rig.host.clone(), transport);
    let (outcome, ()) = tokio::join!(server, client(Rpc::new(peer)));
    outcome.expect("rpc serve ends cleanly");
}

fn prompt(text: &str, expect: Expect) -> Value {
    let command = Command::Prompt {
        expect,
        content: vec![Part::Text { text: text.into() }],
    };
    sonic_rs::to_value(&command).expect("command json")
}

/// Opens a session over RPC and returns its id text.
async fn open(rpc: &mut Rpc, id: i64, reference: Value) -> String {
    let reply = rpc
        .call(id, "session/open", sonic_rs::json!({"ref": reference}))
        .await;
    result(&reply)["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned()
}

/// Reads `session/update` notifications until the named update type.
async fn until_update(rpc: &mut Rpc, kind: &str) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        let frame = rpc.next().await;
        if frame["method"].as_str() != Some("session/update") {
            continue;
        }
        let done = frame["params"]["update"]["type"].as_str() == Some(kind);
        seen.push(frame);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn list_before_initialize_rejected() {
    let rig = rig(&[]).await;
    with_rpc(&rig, async |mut rpc| {
        let reply = rpc.call(1, "session/list", sonic_rs::json!({})).await;
        assert_error(&reply, -32006, "initialize must be the first request");
    })
    .await;
}

#[tokio::test]
async fn initialize_capability_trim() {
    let rig = rig(&[]).await;
    with_rpc(&rig, async |mut rpc| {
        let reply = rpc
            .call(
                1,
                "initialize",
                sonic_rs::json!({
                    "protocolVersion": 3,
                    "clientInfo": {"name": "trim", "version": "1"},
                    "capabilities": ["sessions", "zzz"],
                }),
            )
            .await;
        let body = result(&reply);
        assert_eq!(body["protocolVersion"].as_i64(), Some(1));
        let caps: Vec<&str> = body["capabilities"]
            .as_array()
            .expect("capability list")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(caps, ["sessions"]);
        let client = body["clientId"].as_str().expect("client id");
        let counter = client
            .strip_prefix("trim#")
            .expect("client id names the client");
        assert!(counter.parse::<u64>().is_ok_and(|n| n >= 1), "{client}");
    })
    .await;
}

#[tokio::test]
async fn initialize_refusals() {
    let rig = rig(&[]).await;
    with_rpc(&rig, async |mut rpc| {
        let old = rpc
            .call(1, "initialize", sonic_rs::json!({"protocolVersion": 0}))
            .await;
        assert_error(
            &old,
            -32600,
            "protocol version 0 is not supported: dalgon speaks 1",
        );
        initialize(&mut rpc).await;
        let again = rpc
            .call(2, "initialize", sonic_rs::json!({"protocolVersion": 1}))
            .await;
        assert_error(&again, -32600, "initialize was already called");
    })
    .await;
}

#[tokio::test]
async fn capability_gate_errors() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        let reply = rpc
            .call(
                1,
                "initialize",
                sonic_rs::json!({"protocolVersion": 1, "capabilities": ["sessions"]}),
            )
            .await;
        result(&reply);
        let blob = rpc
            .call(
                2,
                "blob/read",
                sonic_rs::json!({"sessionId": "x", "blobId": "y"}),
            )
            .await;
        assert_error(
            &blob,
            -32007,
            r#"capability "blobs" is not enabled on this connection"#,
        );
        let host = rpc.call(3, "host/subscribe", sonic_rs::json!({})).await;
        assert_error(
            &host,
            -32007,
            r#"capability "host.updates" is not enabled on this connection"#,
        );
        open(
            &mut rpc,
            4,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        let list = rpc.call(5, "session/list", sonic_rs::json!({})).await;
        result(&list);
        assert!(
            rpc.stashed()
                .iter()
                .all(|frame| frame["method"].as_str() != Some("host/update")),
            "host/update sent without the capability: {:?}",
            rpc.stashed()
        );
    })
    .await;
}

#[tokio::test]
async fn unknown_types_rejected() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        let before = rpc
            .call(2, "session/view", sonic_rs::json!({"sessionId": session}))
            .await;
        let seq = result(&before)["seq"].as_i64();
        let command = rpc
            .call(
                3,
                "session/submit",
                sonic_rs::json!({"sessionId": session, "command": {"type": "frobnicate"}}),
            )
            .await;
        assert_invalid_params(&command, "session/submit", "frobnicate");
        let answer = rpc
            .call(
                4,
                "session/answer",
                sonic_rs::json!({
                    "sessionId": session,
                    "requestId": RequestId::new_v7().to_string(),
                    "answer": {"type": "maybe"},
                }),
            )
            .await;
        assert_invalid_params(&answer, "session/answer", "maybe");
        let after = rpc
            .call(5, "session/view", sonic_rs::json!({"sessionId": session}))
            .await;
        assert_eq!(
            result(&after)["seq"].as_i64(),
            seq,
            "a rejected type changed the session"
        );
    })
    .await;
}

#[tokio::test]
async fn malformed_optional_members_are_invalid_params() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        let cases = [
            ("session/list", sonic_rs::json!({"limit": "10"}), "limit"),
            ("session/list", sonic_rs::json!({"limit": 1.5}), "limit"),
            ("session/list", sonic_rs::json!({"cursor": 7}), "cursor"),
            ("session/list", sonic_rs::json!({"search": ["x"]}), "search"),
            (
                "session/view",
                sonic_rs::json!({"sessionId": session, "before": 3}),
                "before",
            ),
            (
                "session/subscribe",
                sonic_rs::json!({"sessionId": session, "gen": "1"}),
                "gen",
            ),
            (
                "session/subscribe",
                sonic_rs::json!({"sessionId": session, "after": "1"}),
                "after",
            ),
            ("docs/read", sonic_rs::json!({"uri": 5}), "uri"),
        ];
        for (id, (method, params, member)) in (2..).zip(cases) {
            let reply = rpc.call(id, method, params).await;
            assert_invalid_params(&reply, method, member);
        }
        let reply = rpc.call(50, "session/list", sonic_rs::json!([])).await;
        assert_invalid_params(&reply, "session/list", "object");
    })
    .await;
}

#[tokio::test]
async fn every_command_tag_reaches_the_decoder() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    let tags = [
        "prompt",
        "steer",
        "follow_up",
        "cancel_queued",
        "cancel",
        "set_model",
        "set_thinking",
        "set_approval",
        "set_mode",
        "compact",
        "move_leaf",
        "fork",
        "rename",
        "set_scoped_models",
        "export",
        "reload_plugins",
        "run",
    ];
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        for (id, tag) in (2..).zip(tags) {
            let reply = rpc
                .call(
                    id,
                    "session/submit",
                    sonic_rs::json!({"sessionId": session, "command": {"type": tag}}),
                )
                .await;
            let message = reply["error"]["message"].as_str().unwrap_or_default();
            assert!(
                !message.contains(tag),
                "tag {tag} was refused before decoding: {reply}"
            );
        }
        let clone = rpc
            .call(
                50,
                "session/submit",
                sonic_rs::json!({"sessionId": session, "command": {"type": "clone"}}),
            )
            .await;
        assert_invalid_params(&clone, "session/submit", "no entries to clone");
        let unknown = rpc
            .call(
                100,
                "session/submit",
                sonic_rs::json!({"sessionId": session, "command": {"type": "frobnicate"}}),
            )
            .await;
        assert_invalid_params(&unknown, "session/submit", "frobnicate");
    })
    .await;
}

#[tokio::test]
async fn commands_beyond_the_old_allowlist_run_over_rpc() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        let done = sonic_rs::json!({"type": "done", "output": {"type": "nothing"}});
        let mode = rpc
            .call(
                2,
                "session/submit",
                sonic_rs::json!({
                    "sessionId": session,
                    "command": {"type": "set_mode", "mode": "normal", "save": "session_only"},
                }),
            )
            .await;
        assert_eq!(result(&mode), &done);
        let scoped = rpc
            .call(
                3,
                "session/submit",
                sonic_rs::json!({
                    "sessionId": session,
                    "command": {"type": "set_scoped_models", "scopedModels": []},
                }),
            )
            .await;
        assert_eq!(result(&scoped), &done);
        let queued = rpc
            .call(
                4,
                "session/submit",
                sonic_rs::json!({
                    "sessionId": session,
                    "command": {"type": "cancel_queued", "turn": 1},
                }),
            )
            .await;
        assert_invalid_params(&queued, "session/submit", "turn 1");
        let reload = rpc
            .call(
                5,
                "session/submit",
                sonic_rs::json!({"sessionId": session, "command": {"type": "reload_plugins"}}),
            )
            .await;
        assert_invalid_params(&reload, "session/submit", "plugins cannot reload");
    })
    .await;
}

#[tokio::test]
async fn session_submit_rejects_export_paths_outside_the_workspace() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    let outside_absolute = rig.dir.path().join("rpc-absolute.md");
    let outside_traversal = rig.dir.path().join("rpc-traversal.md");
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        let absolute = rpc
            .call(
                2,
                "session/submit",
                sonic_rs::json!({
                    "sessionId": session,
                    "command": {
                        "type": "export",
                        "path": outside_absolute,
                        "format": "markdown",
                    },
                }),
            )
            .await;
        assert_eq!(
            absolute["error"]["code"].as_i64(),
            Some(-32602),
            "{absolute}"
        );
        let traversal = rpc
            .call(
                3,
                "session/submit",
                sonic_rs::json!({
                    "sessionId": session,
                    "command": {
                        "type": "export",
                        "path": "../rpc-traversal.md",
                        "format": "markdown",
                    },
                }),
            )
            .await;
        assert_eq!(
            traversal["error"]["code"].as_i64(),
            Some(-32602),
            "{traversal}"
        );
        let relative = rpc
            .call(
                4,
                "session/submit",
                sonic_rs::json!({
                    "sessionId": session,
                    "command": {
                        "type": "export",
                        "path": "rpc-relative.md",
                        "format": "markdown",
                    },
                }),
            )
            .await;
        result(&relative);
        assert!(!outside_absolute.exists());
        assert!(!outside_traversal.exists());
        assert!(rig.workspace.join("rpc-relative.md").exists());
    })
    .await;
}

#[tokio::test]
async fn ephemeral_prompt_happy_path() {
    let rig = rig(&[text_step(&["Hel", "lo"], 10, 5)]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let session = open(
            &mut rpc,
            1,
            sonic_rs::json!({"type": "ephemeral", "workspace": ws}),
        )
        .await;
        rpc.send(
            2,
            "session/subscribe",
            sonic_rs::json!({"sessionId": session}),
        )
        .await;
        let subscribed = rpc.next().await;
        assert_eq!(subscribed["id"].as_i64(), Some(2), "{subscribed}");
        result(&subscribed);
        rpc.send(
            3,
            "session/submit",
            sonic_rs::json!({"sessionId": session, "command": prompt("hi", Expect::Idle)}),
        )
        .await;
        let mut frames = Vec::new();
        let mut accepted_at = None;
        loop {
            let frame = rpc.next().await;
            if frame["id"].as_i64() == Some(3) {
                assert_eq!(result(&frame)["type"].as_str(), Some("accepted"), "{frame}");
                accepted_at = Some(frames.len());
                continue;
            }
            let ended = frame["params"]["update"]["type"].as_str() == Some("turn_ended");
            frames.push(frame);
            if ended {
                break;
            }
        }
        let accepted_at = accepted_at.expect("accepted reply precedes turn_ended");
        assert!(accepted_at < frames.len());
        let updates: Vec<&Value> = frames
            .iter()
            .map(|frame| &frame["params"]["update"])
            .collect();
        let started = updates
            .iter()
            .find(|update| update["type"].as_str() == Some("turn_started"))
            .expect("turn_started");
        assert_eq!(started["cause"].as_str(), Some("user"));
        let text: String = updates
            .iter()
            .filter(|update| update["type"].as_str() == Some("delta"))
            .filter_map(|update| update["text"].as_str())
            .collect();
        assert_eq!(text, "Hello");
        let ended = updates.last().expect("turn_ended");
        assert_eq!(ended["stop"].as_str(), Some("end_turn"), "{ended}");
    })
    .await;
}

#[tokio::test]
async fn relative_workspace_refused() {
    let rig = rig(&[]).await;
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let reply = rpc
            .call(
                1,
                "session/open",
                sonic_rs::json!({"ref": {"type": "new", "workspace": "rel/w"}}),
            )
            .await;
        assert_error(
            &reply,
            -32602,
            "invalid params for session/open: workspace must be an absolute path",
        );
    })
    .await;
}

#[tokio::test]
async fn cancelled_request_one_response() {
    let rig = rig(&[]).await;
    let ws = rig.ws();
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let unknown = rpc.call(1, "x/experimental", sonic_rs::json!({})).await;
        assert_error(&unknown, -32601, r#"unknown method "x/experimental""#);
        let session = open(
            &mut rpc,
            2,
            sonic_rs::json!({"type": "new", "workspace": ws}),
        )
        .await;
        rpc.send(3, "session/view", sonic_rs::json!({"sessionId": session}))
            .await;
        rpc.notify("$/cancel_request", sonic_rs::json!({"requestId": 3}))
            .await;
        let cancelled = rpc.next().await;
        assert_eq!(cancelled["id"].as_i64(), Some(3), "{cancelled}");
        assert_error(&cancelled, -32800, "request 3 was cancelled");
        let next = rpc.call(4, "commands/list", sonic_rs::json!({})).await;
        result(&next);
        assert!(
            rpc.stashed().is_empty(),
            "second response: {:?}",
            rpc.stashed()
        );
        rpc.assert_quiet(Duration::from_millis(200)).await;
    })
    .await;
}
