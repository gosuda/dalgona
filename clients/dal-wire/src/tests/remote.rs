use std::path::Path;
use std::time::Duration;

use dal_agent::SessionRef;
use dal_core::{
    Family, Gen, ModelRequest, ModelRoute, Purpose, RequestParams, Seq, SessionId, View, Workspace,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use crate::error::WireError;
use crate::remote::{RemoteDelivery, RemoteEndpoint, RemoteHost, backoff};

const SESSION: &str = "018f0f62-3b00-7000-8000-000000000001";

struct Peer {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Peer {
    async fn accept(listener: &UnixListener) -> Self {
        let (stream, _) = listener.accept().await.expect("accept client");
        Self::new(stream)
    }

    fn new(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let read = self.reader.read_line(&mut line).await.expect("read frame");
        assert!(read > 0, "client closed the connection");
        sonic_rs::from_str(&line).expect("client frame is JSON")
    }

    async fn send(&mut self, frame: &Value) {
        let mut text = sonic_rs::to_string(frame).expect("encode frame");
        text.push('\n');
        self.writer
            .write_all(text.as_bytes())
            .await
            .expect("write frame");
    }

    async fn expect(&mut self, method: &str) -> (Value, Value) {
        let frame = self.recv().await;
        assert_eq!(frame.get("method").and_then(|m| m.as_str()), Some(method));
        let id = frame.get("id").cloned().expect("request has an id");
        let params = frame.get("params").cloned().unwrap_or_default();
        (id, params)
    }

    async fn reply(&mut self, id: Value, result: Value) {
        self.send(&sonic_rs::json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    async fn initialized(listener: &UnixListener) -> Self {
        let mut peer = Self::accept(listener).await;
        let (id, params) = peer.expect("initialize").await;
        let info = params.get("clientInfo").expect("clientInfo");
        assert_eq!(
            info.get("name").and_then(|n| n.as_str()),
            Some("dal-remotehost")
        );
        assert_eq!(
            info.get("version").and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let capabilities: Vec<&str> = params
            .get("capabilities")
            .and_then(|c| c.as_array())
            .expect("capabilities")
            .iter()
            .filter_map(|c| c.as_str())
            .collect();
        assert_eq!(
            capabilities,
            [
                crate::protocol::CAPABILITIES.as_slice(),
                &["approval", "ask"]
            ]
            .concat()
        );
        let granted: Vec<Value> = crate::protocol::CAPABILITIES
            .iter()
            .map(|name| Value::from(*name))
            .collect();
        peer.reply(
            id,
            sonic_rs::json!({
                "protocolVersion": 1,
                "serverInfo": {"name": "dal", "version": "0"},
                "capabilities": granted,
                "clientId": "dal-remotehost#1",
            }),
        )
        .await;
        peer
    }

    async fn open(&mut self) {
        let (id, _) = self.expect("session/open").await;
        self.reply(
            id,
            sonic_rs::json!({"sessionId": SESSION, "gen": 1, "view": {}}),
        )
        .await;
    }

    async fn subscribed(&mut self, after: Option<(u64, u64)>, head: (u64, u64)) {
        let (id, params) = self.expect("session/subscribe").await;
        assert_eq!(
            params.get("sessionId").and_then(|s| s.as_str()),
            Some(SESSION)
        );
        let sent = params
            .get("gen")
            .and_then(JsonValueTrait::as_u64)
            .zip(params.get("after").and_then(JsonValueTrait::as_u64));
        assert_eq!(sent, after);
        self.reply(id, sonic_rs::json!({"gen": head.0, "seq": head.1}))
            .await;
    }

    async fn update(&mut self, generation: u64, seq: u64) {
        self.send(&sonic_rs::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": SESSION,
                "gen": generation,
                "seq": seq,
                "update": {"type": "remote_test_marker"},
            },
        }))
        .await;
    }

    async fn resync(&mut self, generation: u64, seq: u64) {
        self.send(&sonic_rs::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": SESSION,
                "gen": generation,
                "seq": seq,
                "update": {"type": "resync", "gen": generation, "seq": seq},
            },
        }))
        .await;
    }
}

fn session_ref() -> SessionRef {
    SessionRef::Continue {
        workspace: Workspace::new(std::path::PathBuf::from("/workspace")).expect("workspace"),
    }
}

fn endpoint(dir: &Path) -> (UnixListener, RemoteEndpoint) {
    let path = dir.join("rpc.sock");
    let listener = UnixListener::bind(&path).expect("bind socket");
    (listener, RemoteEndpoint::LocalSocket(path))
}

fn pair(delivery: &RemoteDelivery) -> (u64, u64) {
    match delivery {
        RemoteDelivery::Update(update) => (update.r#gen.get(), update.seq.get()),
        RemoteDelivery::Resync(view) => panic!("unexpected resync at {}", view.seq),
    }
}

fn view_json(generation: u64, seq: u64) -> Value {
    sonic_rs::json!({
        "gen": generation,
        "seq": seq,
        "session": {
            "id": SESSION,
            "name": null,
            "preview": "",
            "workspace": "/workspace",
            "updatedAt": "1970-01-01T00:00:00Z",
        },
        "turn": {"state": "idle"},
        "entries": {"items": [], "nextBefore": null},
        "tree": {"branches": []},
        "settings": {
            "model": null,
            "thinking": "off",
            "approval": "ask",
            "mode": "normal",
            "name": null,
        },
        "open": [],
        "changes": [],
        "usage": {
            "usage": {
                "input_tokens": 0,
                "cached_input_tokens": 0,
                "output_tokens": 0,
                "reasoning_tokens": null,
                "cache_write_tokens": 0,
                "cost_usd": null,
            },
            "contextTokens": 0,
            "contextWindow": 0,
        },
        "stats": {
            "steersQueued": 0,
            "followUpsQueued": 0,
            "retries": 0,
            "droppedObservations": 0,
            "autoCompaction": "off",
        },
    })
}

#[test]
fn backoff_doubles_from_100ms_to_a_30s_cap() {
    let delays: Vec<u128> = (0..12)
        .map(|attempt| backoff(attempt).as_millis())
        .collect();
    assert_eq!(
        delays,
        [
            100, 200, 400, 800, 1_600, 3_200, 6_400, 12_800, 25_600, 30_000, 30_000, 30_000
        ]
    );
    assert_eq!(backoff(u32::MAX), Duration::from_secs(30));
}

#[tokio::test]
async fn reconnect_resubscribes_from_last_cursor_without_duplicates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, endpoint) = endpoint(dir.path());
    let server = async {
        let mut first = Peer::initialized(&listener).await;
        first.open().await;
        first.subscribed(None, (1, 5)).await;
        first.update(1, 6).await;
        first.update(1, 7).await;
        drop(first);
        let mut second = Peer::initialized(&listener).await;
        second.subscribed(Some((1, 7)), (1, 9)).await;
        second.update(1, 7).await;
        second.update(1, 8).await;
        second.update(1, 9).await;
        second
    };
    let client = async {
        let host = RemoteHost::connect(endpoint).await.expect("connect");
        let agent = host.open(session_ref()).await.expect("open");
        assert_eq!(agent.session(), SessionId::parse(SESSION).expect("id"));
        let mut subscription = agent.subscribe(None).await.expect("subscribe");
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(pair(&subscription.next().await.expect("delivery")));
        }
        seen
    };
    let (_server, seen) = tokio::time::timeout(
        Duration::from_secs(10),
        Box::pin(async { tokio::join!(server, client) }),
    )
    .await
    .expect("reconnect finishes");
    assert_eq!(seen, [(1, 6), (1, 7), (1, 8), (1, 9)]);
}

#[tokio::test]
async fn resync_repaints_from_view_and_resubscribes_at_its_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, endpoint) = endpoint(dir.path());
    let server = async {
        let mut peer = Peer::initialized(&listener).await;
        peer.open().await;
        peer.subscribed(None, (1, 5)).await;
        peer.update(1, 6).await;
        peer.resync(2, 3).await;
        let (id, params) = peer.expect("session/view").await;
        assert_eq!(
            params.get("sessionId").and_then(|s| s.as_str()),
            Some(SESSION)
        );
        peer.reply(id, view_json(2, 4)).await;
        peer.subscribed(Some((2, 4)), (2, 4)).await;
        peer.update(2, 4).await;
        peer.update(2, 5).await;
        peer
    };
    let client = async {
        let host = RemoteHost::connect(endpoint).await.expect("connect");
        let agent = host.open(session_ref()).await.expect("open");
        let mut subscription = agent.subscribe(None).await.expect("subscribe");
        let first = subscription.next().await.expect("update");
        let resync = subscription.next().await.expect("resync");
        let last = subscription.next().await.expect("update after resync");
        (first, resync, last)
    };
    let (_server, (first, resync, last)) = tokio::time::timeout(
        Duration::from_secs(10),
        Box::pin(async { tokio::join!(server, client) }),
    )
    .await
    .expect("resync finishes");
    assert_eq!(pair(&first), (1, 6));
    let RemoteDelivery::Resync(view) = resync else {
        panic!("expected a resync delivery, got {resync:?}");
    };
    let view: View = *view;
    assert_eq!((view.r#gen, view.seq), (gen_of(2), seq_of(4)));
    assert_eq!(pair(&last), (2, 5));
}

#[tokio::test]
async fn host_operations_without_a_version_1_method_are_typed_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, endpoint) = endpoint(dir.path());
    let (_peer, host) = tokio::join!(Peer::initialized(&listener), async {
        RemoteHost::connect(endpoint).await.expect("connect")
    });
    let route = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt".into(),
    };
    let request = ModelRequest {
        purpose: Purpose::Turn,
        model: route.clone(),
        system: String::new().into(),
        tools: Vec::new().into(),
        context: Vec::new().into(),
        params: RequestParams::default(),
        cache_key: None,
    };
    let outcomes: [(&str, Result<(), WireError>); 2] = [
        ("generation", host.generation().map(|_| ())),
        ("relay", host.relay(&route, &request).map(|_| ())),
    ];
    for (operation, result) in outcomes {
        assert!(
            matches!(result, Err(WireError::Unsupported { operation: found }) if found == operation),
            "{operation} is not a typed unsupported error"
        );
    }
}

#[tokio::test]
async fn dropped_remote_answerer_does_not_strand_a_real_approval() {
    use super::support::{
        Rpc, ask_probe_extension, result, result_text, rig_with_extensions, text_step, tool_step,
    };
    use crate::rpc::serve_rpc;
    use crate::transport::{MemoryTransport, serve_local};

    let steps = [tool_step("c1", "ask"), text_step(&["done"], 1, 1)];
    let rig = rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await;

    let sock_tmp = tempfile::tempdir().expect("tempdir");
    let socket_dir = sock_tmp.path().join("rpc");
    std::fs::create_dir(&socket_dir).expect("socket dir");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700))
            .expect("private socket dir");
    }
    let socket = socket_dir.join("dal.sock");
    let mut server_set = tokio::task::JoinSet::new();
    {
        let host = rig.host.clone();
        let socket = socket.clone();
        server_set.spawn(async move {
            serve_local(&socket, None, None, move |transport| {
                let host = host.clone();
                Box::pin(async move { serve_rpc(host, transport).await })
            })
            .await
            .expect("local server runs");
        });
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(tokio::time::Instant::now() < deadline, "socket appears");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (transport, peer) = MemoryTransport::pair(64);
    let server = serve_rpc(rig.host.clone(), transport);
    let work = async {
        let mut rpc = Rpc::new(peer);
        let capabilities: Vec<Value> = ["sessions"].iter().map(|name| Value::from(*name)).collect();
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
        result(&reply);
        let reply = rpc
            .call(
                1,
                "session/open",
                sonic_rs::json!({"ref": {"type": "new", "workspace": rig.ws()}}),
            )
            .await;
        let session = result(&reply)["sessionId"]
            .as_str()
            .expect("session id")
            .to_owned();

        let remote = RemoteHost::connect(RemoteEndpoint::LocalSocket(socket.clone()))
            .await
            .expect("connect");
        let agent = remote
            .open(SessionRef::Resume {
                key: session.clone().into_boxed_str(),
                workspace: rig.core_workspace(),
            })
            .await
            .expect("resume");
        let subscription = agent.subscribe(None).await.expect("subscribe");
        drop(subscription);

        let reply = rpc
            .call(
                2,
                "session/submit",
                sonic_rs::json!({
                    "sessionId": session,
                    "command": {"type": "set_approval", "mode": "ask", "save": "session_only"},
                }),
            )
            .await;
        result(&reply);
        rpc.send(
            3,
            "session/subscribe",
            sonic_rs::json!({"sessionId": session}),
        )
        .await;
        let reply = rpc.next().await;
        assert_eq!(reply["id"].as_i64(), Some(3), "{reply}");
        result(&reply);
        rpc.send(
            4,
            "session/submit",
            sonic_rs::json!({
                "sessionId": session,
                "command": {
                    "type": "prompt",
                    "expect": "idle",
                    "content": [{"type": "text", "text": "edit"}],
                },
            }),
        )
        .await;
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
                "an approval opened for a dropped answerer: {frame}"
            );
            if kind == "turn_ended" {
                break;
            }
        }
        let denial = result_text(&rig, &session, "ask", true).await;
        assert_eq!(
            dal_core::parse_headless_denial(&denial).map(|(tool, _)| tool),
            Some("ask"),
            "{denial}"
        );
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        let (outcome, ()) = tokio::join!(server, work);
        outcome.expect("rpc serve ends cleanly");
    })
    .await
    .expect("a dropped answerer denies a real approval at once");
    server_set.abort_all();
}

fn gen_of(value: u64) -> Gen {
    Gen::new(std::num::NonZeroU64::new(value).expect("nonzero"))
}

fn seq_of(value: u64) -> Seq {
    Seq::new(std::num::NonZeroU64::new(value).expect("nonzero"))
}
