//! Plan-named extension-turn wire tests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};
use dal_agent::{Agent, Delivery, HostUpdate, SessionRef};
use dal_core::{
    ClientId, ExtState, ExtStatus, Gen, PageReq, Seq, ServiceSet, SessionId, TurnId, TurnState,
    Update, UpdateKind,
};
use sonic_rs::{JsonValueTrait, Value};

use super::support::{
    Rig, Rpc, gate_step, host_header, http, initialize, result, rig_with_extensions,
    router_options, sse_data, text_step, with_serve,
};
use crate::serve::a2a::events::{apply, step};
use crate::serve::a2a::table::{TaskKey, TaskRec};
use crate::transport::MemoryTransport;
use crate::{serve_acp, serve_rpc};

struct Gauge {
    state: Mutex<Option<Box<str>>>,
}

impl Gauge {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(None),
        })
    }

    fn busy(&self, text: &str) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(text.into());
    }

    fn quiet(&self) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

impl StatusPoll for Gauge {
    fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        StatusSnapshot {
            quiet: state.is_none(),
            text: state.clone(),
        }
    }
}

fn status_extension(gauge: &Arc<Gauge>) -> dal_agent::ext::Extension {
    ExtensionBuilder::new("focus", "0.1.0", ServiceSet::EMPTY)
        .expect("extension name")
        .status_kind("focus", Arc::clone(gauge) as Arc<dyn StatusPoll>)
        .build()
        .expect("status extension builds")
}

async fn probe(rig: &Rig, session: &str) -> Agent {
    rig.host
        .open(
            SessionRef::Resume {
                key: session.into(),
                workspace: rig.core_workspace(),
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open the live session")
}

async fn sweeps(agent: &Agent, count: usize) {
    for _ in 0..count {
        agent.poll_status().await.expect("status sweep");
    }
}

async fn drain(rpc: &mut Rpc) -> Vec<Value> {
    let mut frames = Vec::new();
    while let Ok(frame) = tokio::time::timeout(Duration::from_millis(300), rpc.next()).await {
        frames.push(frame);
    }
    frames
}

fn status_updates(frames: &[Value]) -> Vec<Value> {
    frames
        .iter()
        .filter(|frame| frame["params"]["update"]["type"].as_str() == Some("ext_status"))
        .map(|frame| frame["params"]["update"].clone())
        .collect()
}

fn status_notices(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| {
            let notice = &frame["params"]["update"]["_dal/notice"];
            (notice["kind"].as_str() == Some("status"))
                .then(|| notice["text"].as_str().unwrap_or_default().to_owned())
        })
        .collect()
}

fn rpc_status_updates_coalesce_before_write() {
    let mut pump = crate::rpc::subs::Pump::new(SessionId::new_v7());
    for number in 1..=50 {
        let seq = Seq::new(std::num::NonZeroU64::new(number).unwrap_or(std::num::NonZeroU64::MIN));
        pump.push(Arc::new(Update {
            r#gen: Gen::new(std::num::NonZeroU64::MIN),
            seq,
            kind: UpdateKind::ExtStatus(ExtStatus {
                ext: "focus".into(),
                state: ExtState::Busy,
                text: Some(format!("busy-{number}").into()),
            }),
        }));
    }
    let queued = pump.queued_updates();
    assert_eq!(queued.len(), 1, "one unsent status per extension");
    let UpdateKind::ExtStatus(status) = &queued[0].kind else {
        panic!("status update expected");
    };
    let expected = format!("busy-{}", 50_u64);
    assert_eq!(status.text.as_deref(), Some(expected.as_str()));
}

async fn rpc_scenario(rig: &Rig, gauge: &Arc<Gauge>) {
    let ws = rig.ws();
    let (transport, peer) = MemoryTransport::pair(256);
    let server = serve_rpc(rig.host.clone(), transport);
    let client = async {
        let mut rpc = Rpc::new(peer);
        initialize(&mut rpc).await;
        let opened = rpc
            .call(
                1,
                "session/open",
                sonic_rs::json!({"ref": { "type": "new", "workspace": ws}}),
            )
            .await;
        let session = result(&opened)["sessionId"]
            .as_str()
            .expect("session id")
            .to_owned();
        rpc.call(
            2,
            "session/subscribe",
            sonic_rs::json!({"sessionId": session}),
        )
        .await;
        let agent = probe(rig, &session).await;

        gauge.busy("indexing");
        sweeps(&agent, 60).await;
        let busy = status_updates(&drain(&mut rpc).await);
        assert_eq!(
            busy,
            vec![sonic_rs::json!({
                "type": "ext_status",
                "ext": "focus",
                "state": "busy",
                "text": "indexing"
            })],
            "sixty identical busy sweeps reach the client as one update"
        );

        gauge.quiet();
        sweeps(&agent, 3).await;
        let quiet = status_updates(&drain(&mut rpc).await);
        assert_eq!(
            quiet,
            vec![sonic_rs::json!({"type": "ext_status", "ext": "focus", "state": "quiet"})],
            "the quiet update follows the busy one and repeats nothing"
        );
        assert!(agent.is_quiet().await.expect("quiet predicate"));
    };
    let (outcome, ()) = tokio::join!(server, client);
    outcome.expect("rpc serve ends cleanly");
}

async fn acp_scenario(rig: &Rig, gauge: &Arc<Gauge>) {
    let ws = rig.ws();
    let (transport, peer) = MemoryTransport::pair(256);
    let server = serve_acp(rig.host.clone(), transport);
    let client = async {
        let mut acp = Rpc::new(peer);
        acp.send(
            1,
            "initialize",
            sonic_rs::json!({"protocolVersion": 1, "clientInfo": {"name": "zed"}}),
        )
        .await;
        acp.raw().await;
        let created = acp
            .call(
                2,
                "session/new",
                sonic_rs::json!({"cwd": ws, "mcpServers": []}),
            )
            .await;
        let session = result(&created)["sessionId"]
            .as_str()
            .expect("session id")
            .to_owned();
        acp.send(
            3,
            "session/prompt",
            sonic_rs::json!({"sessionId": session, "prompt": [{"type": "text", "text": "wait"}]}),
        )
        .await;
        loop {
            let frame = acp.next().await;
            if frame["params"]["update"]["toolCallId"].as_str() == Some("c1") {
                break;
            }
        }
        let agent = probe(rig, &session).await;
        gauge.busy("indexing");
        sweeps(&agent, 60).await;
        gauge.quiet();
        sweeps(&agent, 3).await;
        rig.gate.add_permits(1);
        let mut frames = Vec::new();
        loop {
            let frame = acp.next().await;
            let done = frame["id"].as_i64() == Some(3);
            frames.push(frame);
            if done {
                break;
            }
        }
        assert_eq!(
            status_notices(&frames),
            vec!["focus: indexing".to_owned(), "focus: quiet".to_owned()],
            "ACP maps the two changes to two status notices"
        );
    };
    let (outcome, ()) = tokio::join!(server, client);
    outcome.expect("acp serve ends cleanly");
}

fn acp_status_mappings() {
    let update = Update {
        r#gen: Gen::new(std::num::NonZeroU64::MIN),
        seq: Seq::new(std::num::NonZeroU64::MIN),
        kind: UpdateKind::ExtStatus(ExtStatus {
            ext: "focus".into(),
            state: ExtState::Busy,
            text: Some("indexing".into()),
        }),
    };
    let session = SessionId::new_v7();
    for version in [crate::AcpVersion::V1, crate::AcpVersion::V2] {
        let notices = crate::acp::map::map_update(version, session, &update, None);
        let [notice] = notices.as_slice() else {
            panic!("one status notice expected");
        };
        assert_eq!(notice["_dal/notice"]["kind"].as_str(), Some("status"));
        assert_eq!(
            notice["_dal/notice"]["text"].as_str(),
            Some("focus: indexing")
        );
    }
}

fn a2a_scenario() {
    let task_key = TaskKey {
        session: SessionId::new_v7(),
        turn: TurnId::new(std::num::NonZeroU64::MIN),
    };
    let mut task = TaskRec::started(task_key, "hi".to_owned());
    let update = UpdateKind::ExtStatus(ExtStatus {
        ext: "focus".into(),
        state: ExtState::Busy,
        text: Some("indexing".into()),
    });
    let next = step(task_key.turn, None, &update).expect("a2a keeps ext_status");
    let emitted = apply(&mut task, next);
    let [event] = emitted.as_slice() else {
        panic!("one status update expected: {emitted:?}");
    };
    assert_eq!(
        event["statusUpdate"]["metadata"]["dal.status"].as_str(),
        Some("focus: indexing")
    );
    assert!(
        event["statusUpdate"]["metadata"]["dal.notice"]
            .as_str()
            .is_none()
    );
}

#[expect(
    clippy::large_futures,
    reason = "one scenario drives every router surface in one future"
)]
async fn router_scenario(rig: &Rig, gauge: &Arc<Gauge>) {
    let mut host_updates = rig.host.subscribe();
    let request = async |addr: std::net::SocketAddr| {
        http(
            addr,
            &format!(
                "POST /v1/chat/completions HTTP/1.1\r\n{}\r\ncontent-type: application/json",
                host_header(addr)
            ),
            r#"{"model":"dalgon/normal","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await
    };
    let driver = async {
        let session = loop {
            let update = tokio::time::timeout(super::support::WAIT, host_updates.next())
                .await
                .expect("host update in time")
                .expect("host subscription open");
            if let HostUpdate::SessionAdded { session, .. } = update {
                break session.to_string();
            }
        };
        let agent = probe(rig, &session).await;
        let mut published = agent.subscribe(None).expect("subscribe");
        tokio::time::timeout(super::support::WAIT, async {
            while !matches!(
                agent.view(PageReq::default()).expect("view").turn,
                TurnState::Running { .. }
            ) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("router turn starts");
        gauge.busy("indexing");
        sweeps(&agent, 5).await;
        gauge.quiet();
        sweeps(&agent, 2).await;
        rig.gate.add_permits(1);
        let mut seen = 0;
        while let Ok(Some(delivery)) =
            tokio::time::timeout(Duration::from_millis(300), published.next()).await
        {
            if let Delivery::Update(update) = delivery
                && matches!(update.kind, UpdateKind::ExtStatus(_))
            {
                seen += 1;
            }
        }
        assert_eq!(seen, 2, "the session published busy then quiet");
    };
    with_serve(rig, router_options(rig), async |addr| {
        let (reply, ()) = tokio::join!(request(addr), driver);
        assert_eq!(reply.status, 200, "{}", reply.body);
        let body = sse_data(&reply.body).join("\n");
        assert!(body.contains("done"), "{body}");
        assert!(!body.contains("focus"), "{body}");
        assert!(!body.contains("ext_status"), "{body}");
        assert!(!reply.head.contains("focus"), "{}", reply.head);
    })
    .await;
}

#[tokio::test]
async fn quiet_gating_coalescing() {
    rpc_status_updates_coalesce_before_write();
    acp_status_mappings();
    a2a_scenario();

    let gauge = Gauge::new();
    let rig = rig_with_extensions(
        &[text_step(&["ok"], 1, 1)],
        "",
        vec![status_extension(&gauge)],
    )
    .await;
    Box::pin(rpc_scenario(&rig, &gauge)).await;
    rig.host.shutdown(Duration::from_secs(1)).await;

    let gauge = Gauge::new();
    let rig = rig_with_extensions(
        &[gate_step("c1"), text_step(&["done"], 1, 1)],
        "",
        vec![status_extension(&gauge)],
    )
    .await;
    Box::pin(acp_scenario(&rig, &gauge)).await;
    rig.host.shutdown(Duration::from_secs(1)).await;

    let gauge = Gauge::new();
    let rig = rig_with_extensions(
        &[gate_step("c1"), text_step(&["done"], 1, 1)],
        "",
        vec![status_extension(&gauge)],
    )
    .await;
    Box::pin(router_scenario(&rig, &gauge)).await;
    rig.host.shutdown(Duration::from_secs(1)).await;
}
