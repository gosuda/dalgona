use std::path::Path;
use std::task::Poll;
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
use crate::remote::{RemoteAgent, RemoteDelivery, RemoteEndpoint, RemoteHost, backoff};

const SESSION: &str = "018f0f62-3b00-7000-8000-000000000001";
const WAIT_SESSION: &str = "018f0f62-3b00-7000-8000-000000000002";

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

    /// Opens one session and names its id in the reply.
    async fn open_session(&mut self, session: &str) {
        let (id, _) = self.expect("session/open").await;
        self.reply(
            id,
            sonic_rs::json!({"sessionId": session, "gen": 1, "view": {}}),
        )
        .await;
    }

    /// Confirms one `session/subscribe` for `session` and names its head.
    async fn subscribed_session(
        &mut self,
        session: &str,
        after: Option<(u64, u64)>,
        head: (u64, u64),
    ) {
        let (id, params) = self.expect("session/subscribe").await;
        assert_eq!(
            params.get("sessionId").and_then(|s| s.as_str()),
            Some(session)
        );
        let sent = params
            .get("gen")
            .and_then(JsonValueTrait::as_u64)
            .zip(params.get("after").and_then(JsonValueTrait::as_u64));
        assert_eq!(sent, after);
        self.reply(id, sonic_rs::json!({"gen": head.0, "seq": head.1}))
            .await;
    }

    /// Sends one update for `session`.
    async fn update_session(&mut self, session: &str, generation: u64, seq: u64) {
        self.send(&sonic_rs::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session,
                "gen": generation,
                "seq": seq,
                "update": {"type": "remote_test_marker"},
            },
        }))
        .await;
    }

    /// Reads to EOF on the closed transport, bounded: a healthy keep-open
    /// connection fails the bound instead of hanging.
    async fn expect_eof(&mut self) {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(10), self.reader.read_line(&mut line))
            .await
            .expect("closed transport reaches EOF in time")
            .expect("read after close");
        assert_eq!(read, 0, "expected EOF, got frame: {line}");
    }

    /// Replies to one request with a JSON-RPC error, as a non-reference
    /// server would for a well-formed call it refuses.
    async fn error(&mut self, id: Value, code: i64, message: &str) {
        self.send(&sonic_rs::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }))
        .await;
    }

    /// Accepts the replacement connection after a refusal forced a
    /// reconnection, completing its handshake. The refused session left
    /// `subs`, so only the waiter resubscribes, at its cursor.
    async fn successor(
        listener: &UnixListener,
        after: Option<(u64, u64)>,
        head: (u64, u64),
    ) -> Self {
        let mut peer = Self::initialized(listener).await;
        peer.subscribed_session(WAIT_SESSION, after, head).await;
        peer
    }
}

fn session_ref() -> SessionRef {
    SessionRef::Continue {
        workspace: Workspace::new(std::path::PathBuf::from("/workspace")).expect("workspace"),
    }
}

/// The second session's resume reference, on the same shared connection.
fn wait_session() -> SessionRef {
    SessionRef::Continue {
        workspace: Workspace::new(std::path::PathBuf::from("/wait")).expect("workspace"),
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
async fn stale_drop_leaves_current_delivering() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, endpoint) = endpoint(dir.path());
    let server = async {
        let mut peer = Peer::initialized(&listener).await;
        peer.open().await;
        peer.subscribed(None, (1, 5)).await;
        peer.subscribed(None, (1, 5)).await;
        let quiet = tokio::time::timeout(Duration::from_millis(200), peer.recv()).await;
        assert!(quiet.is_err(), "stale drop must not release the server sub");
        peer.update(1, 6).await;
        peer
    };
    let client = async {
        let host = RemoteHost::connect(endpoint).await.expect("connect");
        let agent = host.open(session_ref()).await.expect("open");
        let first = agent.subscribe(None).await.expect("first");
        let mut second = agent.subscribe(None).await.expect("second");
        drop(first);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let delivery = tokio::time::timeout(Duration::from_secs(5), second.next())
            .await
            .expect("delivery in time")
            .expect("delivery");
        assert_eq!(pair(&delivery), (1, 6));
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        Box::pin(async { tokio::join!(server, client) }),
    )
    .await
    .expect("stale drop finishes");
}

#[tokio::test]
async fn idle_drop_releases_server_subscription() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, endpoint) = endpoint(dir.path());
    let server = async {
        let mut peer = Peer::initialized(&listener).await;
        peer.open().await;
        peer.subscribed(None, (1, 5)).await;
        let (id, params) = peer.expect("session/unsubscribe").await;
        assert_eq!(
            params.get("sessionId").and_then(|s| s.as_str()),
            Some(SESSION)
        );
        peer.reply(id, sonic_rs::json!({})).await;
    };
    let client = async {
        let host = RemoteHost::connect(endpoint).await.expect("connect");
        let agent = host.open(session_ref()).await.expect("open");
        let subscription = agent.subscribe(None).await.expect("subscribe");
        drop(subscription);
        // The release is async: keep the connection alive until the server
        // observes it.
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        Box::pin(async { tokio::join!(server, client) }),
    )
    .await
    .expect("idle drop releases");
}

#[tokio::test]
async fn drop_outside_runtime_thread_still_releases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, endpoint) = endpoint(dir.path());
    let server = async {
        let mut peer = Peer::initialized(&listener).await;
        peer.open().await;
        peer.subscribed(None, (1, 5)).await;
        let (id, params) = peer.expect("session/unsubscribe").await;
        assert_eq!(
            params.get("sessionId").and_then(|s| s.as_str()),
            Some(SESSION)
        );
        peer.reply(id, sonic_rs::json!({})).await;
    };
    let client = async {
        let host = RemoteHost::connect(endpoint).await.expect("connect");
        let agent = host.open(session_ref()).await.expect("open");
        let subscription = agent.subscribe(None).await.expect("subscribe");
        // A plain std thread carries no entered runtime handle; scheduling
        // must use the handle retained at connect, not thread-local context.
        std::thread::spawn(move || drop(subscription))
            .join()
            .expect("drop thread joins");
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        Box::pin(async { tokio::join!(server, client) }),
    )
    .await
    .expect("off-thread drop releases");
}

#[tokio::test]
async fn raced_drop_and_resubscribe_never_unsubs_successor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, endpoint) = endpoint(dir.path());
    let server = async {
        let mut peer = Peer::initialized(&listener).await;
        peer.open().await;
        peer.subscribed(None, (1, 5)).await;
        // Either order is correct: a stale send before the resubscribe, or
        // no send at all. What must never happen is an unsubscribe landing
        // after the successor subscribed.
        let frame = peer.recv().await;
        if frame.get("method").and_then(|m| m.as_str()) == Some("session/unsubscribe") {
            let id = frame.get("id").cloned().expect("unsubscribe has an id");
            peer.reply(id, sonic_rs::json!({})).await;
            peer.subscribed(None, (1, 5)).await;
        } else {
            assert_eq!(
                frame.get("method").and_then(|m| m.as_str()),
                Some("session/subscribe"),
                "unexpected frame: {frame}"
            );
            let id = frame.get("id").cloned().expect("subscribe has an id");
            peer.reply(id, sonic_rs::json!({"gen": 1, "seq": 5})).await;
        }
        let quiet = tokio::time::timeout(Duration::from_millis(300), peer.recv()).await;
        assert!(quiet.is_err(), "no unsubscribe may follow the successor");
        peer.update(1, 6).await;
        peer
    };
    let client = async {
        let host = RemoteHost::connect(endpoint).await.expect("connect");
        let agent = host.open(session_ref()).await.expect("open");
        let first = agent.subscribe(None).await.expect("first");
        drop(first);
        let mut second = agent.subscribe(None).await.expect("second");
        let delivery = tokio::time::timeout(Duration::from_secs(5), second.next())
            .await
            .expect("delivery in time")
            .expect("delivery");
        assert_eq!(pair(&delivery), (1, 6));
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        Box::pin(async { tokio::join!(server, client) }),
    )
    .await
    .expect("raced resubscribe finishes");
}

/// A dropped subscription whose `session/unsubscribe` is refused over an
/// otherwise healthy connection closes that transport while another
/// session waits, and the waiting session resumes on a fresh one.
///
/// The server rejects the well-formed call with `method not found`
/// (-32601) and keeps its end open, so only the client can close it. The
/// release runs through the `Drop` cleanup task. The poll order that pins
/// the waiter's reader across the teardown is forced by the private guard
/// in `remote/conn.rs`; this test covers the public path end to end.
#[tokio::test]
async fn refused_drop_release_tears_down_and_session_resumes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (listener, endpoint) = endpoint(dir.path());
    let server = async {
        let mut peer = Peer::initialized(&listener).await;
        // The client opens and subscribes the dropped session, then the
        // surviving session; the server mirrors that order.
        peer.open_session(SESSION).await;
        peer.subscribed_session(SESSION, None, (1, 5)).await;
        peer.open_session(WAIT_SESSION).await;
        peer.subscribed_session(WAIT_SESSION, None, (1, 5)).await;
        // The refusal lands while the waiter's poll owns the reader step on
        // this quiet transport; the socket stays open until the repair.
        let (id, params) = peer.expect("session/unsubscribe").await;
        assert_eq!(
            params.get("sessionId").and_then(|s| s.as_str()),
            Some(SESSION)
        );
        peer.error(id, -32601, "method not found").await;
        // The client still waits on the other session, so this EOF is the
        // teardown, not a final-owner drop.
        peer.expect_eof().await;
        let mut fresh = Peer::successor(&listener, Some((1, 5)), (1, 5)).await;
        fresh.update_session(WAIT_SESSION, 1, 6).await;
        fresh
    };
    let client = async {
        let host = RemoteHost::connect(endpoint).await.expect("connect");
        let agent = host.open(session_ref()).await.expect("open");
        let subscription = agent.subscribe(None).await.expect("subscribe");
        let waiter_agent = host.open(wait_session()).await.expect("open");
        let mut waiter = waiter_agent.subscribe(None).await.expect("subscribe");
        // Causal park: one poll of the waiter's `next()` must be Pending on
        // the quiet transport before the drop, so the reader step is owned by
        // this waiter when the refused cleanup runs.
        let mut parked = Box::pin(waiter.next());
        let pending = std::future::poll_fn(|cx| match parked.as_mut().poll(cx) {
            Poll::Ready(_) => Poll::Ready(false),
            Poll::Pending => Poll::Ready(true),
        })
        .await;
        assert!(pending, "waiter next() must park before the drop");
        drop(subscription);
        let delivery = tokio::time::timeout(Duration::from_secs(10), parked)
            .await
            .expect("the waiting session resumes after the refusal")
            .expect("delivery");
        assert_eq!(pair(&delivery), (1, 6));
    };
    tokio::time::timeout(
        Duration::from_secs(30),
        Box::pin(async { tokio::join!(server, client) }),
    )
    .await
    .expect("refused unsubscribe repair finishes");
}

struct LocalEndpoint {
    _tmp: tempfile::TempDir,
    socket: std::path::PathBuf,
    server: tokio::task::JoinSet<()>,
}

impl LocalEndpoint {
    async fn start(host: dal_agent::Host) -> Self {
        use crate::transport::serve_local;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("rpc");
        std::fs::create_dir(&dir).expect("socket dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .expect("private socket dir");
        }
        let socket = dir.join("dal.sock");
        let mut server = tokio::task::JoinSet::new();
        {
            let host = host.clone();
            let socket = socket.clone();
            server.spawn(async move {
                serve_local(&socket, None, None, move |transport| {
                    let host = host.clone();
                    Box::pin(async move { crate::rpc::serve_rpc(host, transport).await })
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
        Self {
            _tmp: tmp,
            socket,
            server,
        }
    }
}

async fn ask_rig() -> super::support::Rig {
    use super::support::{ask_probe_extension, rig_with_extensions, text_step, tool_step};
    let steps = [tool_step("c1", "ask"), text_step(&["done"], 1, 1)];
    rig_with_extensions(&steps, "", vec![ask_probe_extension()]).await
}

async fn probe_session(rpc: &mut super::support::Rpc, workspace: String) -> String {
    use super::support::result;
    let reply = rpc
        .call(
            1,
            "session/open",
            sonic_rs::json!({"ref": {"type": "new", "workspace": workspace}}),
        )
        .await;
    result(&reply)["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned()
}

async fn remote_resumed(
    socket: std::path::PathBuf,
    rig: &super::support::Rig,
    session: &str,
) -> RemoteAgent {
    let remote = RemoteHost::connect(RemoteEndpoint::LocalSocket(socket))
        .await
        .expect("connect");
    remote
        .open(SessionRef::Resume {
            key: session.to_owned().into_boxed_str(),
            workspace: rig.core_workspace(),
        })
        .await
        .expect("resume")
}

async fn set_ask_approval(rpc: &mut super::support::Rpc, session: &str) {
    use super::support::result;
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
}

async fn subscribe_probe(rpc: &mut super::support::Rpc, session: &str) {
    use super::support::result;
    rpc.send(
        3,
        "session/subscribe",
        sonic_rs::json!({"sessionId": session}),
    )
    .await;
    let reply = rpc.next().await;
    assert_eq!(reply["id"].as_i64(), Some(3), "{reply}");
    result(&reply);
}

#[tokio::test]
async fn dropped_remote_answerer_does_not_strand_a_real_approval() {
    use super::support::{Rpc, result, result_text};
    use crate::rpc::serve_rpc;
    use crate::transport::MemoryTransport;

    let rig = ask_rig().await;
    let mut endpoint = LocalEndpoint::start(rig.host.clone()).await;
    let (transport, peer) = MemoryTransport::pair(64);
    let server = serve_rpc(rig.host.clone(), transport);
    let work = async {
        let mut rpc = Rpc::new(peer);
        let capabilities: Vec<Value> = ["sessions"].iter().map(|n| Value::from(*n)).collect();
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
        let session = probe_session(&mut rpc, rig.ws()).await;
        let agent = remote_resumed(endpoint.socket.clone(), &rig, &session).await;
        let subscription = agent.subscribe(None).await.expect("subscribe");
        drop(subscription);
        set_ask_approval(&mut rpc, &session).await;
        subscribe_probe(&mut rpc, &session).await;
        submit_edit_prompt(&mut rpc, &session).await;
        drain_to_end(&mut rpc).await;
        let denial = result_text(&rig, &session, "ask", true).await;
        assert_eq!(
            dal_core::parse_headless_denial(&denial).map(|(tool, _)| tool),
            Some("ask"),
            "{denial}"
        );
    };
    tokio::time::timeout(
        Duration::from_secs(30),
        Box::pin(async {
            let (outcome, ()) = tokio::join!(server, work);
            outcome.expect("rpc serve ends cleanly");
        }),
    )
    .await
    .expect("a dropped answerer denies a real approval at once");
    endpoint.server.abort_all();
}

async fn submit_edit_prompt(rpc: &mut super::support::Rpc, session: &str) {
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
}

async fn drain_to_end(rpc: &mut super::support::Rpc) {
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
}

fn gen_of(value: u64) -> Gen {
    Gen::new(std::num::NonZeroU64::new(value).expect("nonzero"))
}

fn seq_of(value: u64) -> Seq {
    Seq::new(std::num::NonZeroU64::new(value).expect("nonzero"))
}
