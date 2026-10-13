use std::path::Path;
use std::time::Duration;

use dal_agent::SessionRef;
use dal_core::{Gen, Seq, SessionId, View, Workspace};
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
        assert_eq!(capabilities, crate::protocol::CAPABILITIES);
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
    assert!(matches!(
        host.generation(),
        Err(WireError::Unsupported {
            operation: "generation"
        })
    ));
}

fn gen_of(value: u64) -> Gen {
    Gen::new(std::num::NonZeroU64::new(value).expect("nonzero"))
}

fn seq_of(value: u64) -> Seq {
    Seq::new(std::num::NonZeroU64::new(value).expect("nonzero"))
}
