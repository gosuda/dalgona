use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Answer, ClientId, Config, ConfigProduct, Gen, Notice, Owner, Preview, Question, Request,
    RequestId, Seq, SessionId, Stop, StreamChannel, TurnId, Update, UpdateKind, Workspace,
};
use sonic_rs::{JsonValueTrait, Value};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::acp::map::pump::Deliveries;
use crate::acp::map::{PromptEnd, PumpCtx, prompt_pump};
use crate::acp::{AcpConn, AcpVersion, ServerAnswer};
use crate::transport::{MemoryPeer, MemoryTransport};

/// Deliveries injected by the test through a channel.
struct Injected(mpsc::Receiver<Delivery>);

impl Deliveries for Injected {
    fn next(&mut self) -> impl Future<Output = Option<Delivery>> + Send {
        self.0.recv()
    }

    fn resume(&self, _agent: &Agent, _position: (Gen, Seq), _turn: TurnId) -> Option<Self> {
        None
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    _host: Host,
    agent: Agent,
    session: SessionId,
    state: Arc<Mutex<AcpConn>>,
    transport: crate::transport::Transport,
    peer: MemoryPeer,
    feed: mpsc::Sender<Delivery>,
    source: Option<Injected>,
    seq: u64,
}

const TURN: TurnId = TurnId::new(NonZeroU64::MIN);

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let host = start_host(root).await;
    let workspace = Workspace::try_from(root.to_path_buf()).expect("absolute workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("acp#test"),
        )
        .await
        .expect("session opens");
    let session = agent
        .view(dal_core::PageReq::default())
        .expect("head view")
        .session
        .id;
    let (transport, peer) = MemoryTransport::pair(64);
    let (feed, receiver) = mpsc::channel(16);
    Fixture {
        _dir: dir,
        _host: host,
        agent,
        session,
        state: Arc::new(Mutex::new(AcpConn::new())),
        transport,
        peer,
        feed,
        source: Some(Injected(receiver)),
        seq: 0,
    }
}

async fn start_host(root: &Path) -> Host {
    let product = Product {
        name: "dal",
        data_root: root.join("data"),
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: root.to_path_buf(),
        sandbox_helper: None,
    };
    let config =
        Config::load(ConfigProduct::Dalgon, &root.join("data"), "", None).expect("default config");
    Host::start(product, config, env)
        .await
        .expect("host starts")
}

impl Fixture {
    async fn push(&mut self, kind: UpdateKind) {
        self.seq += 1;
        let update = Update {
            r#gen: Gen::new(NonZeroU64::MIN),
            seq: Seq::new(NonZeroU64::new(self.seq).expect("nonzero seq")),
            kind,
        };
        self.feed
            .send(Delivery::Update(Arc::new(update)))
            .await
            .expect("pump reads updates");
    }

    async fn frame(&mut self) -> Value {
        let frame = tokio::time::timeout(Duration::from_secs(5), self.peer.read_frame())
            .await
            .expect("frame in time")
            .expect("open transport");
        sonic_rs::from_str(&frame).expect("frame is JSON")
    }

    async fn open_count(&self) -> (usize, usize) {
        let locked = self.state.lock().await;
        (locked.pending.len(), locked.outstanding.len())
    }
}

fn approval() -> Request {
    Request {
        id: RequestId::new_v7(),
        turn: Some(TURN),
        owner: Owner::Core,
        question: Question::Approval {
            tool: "exec".into(),
            preview: Preview {
                title: "git status".into(),
                body: String::new().into(),
                digest: None,
            },
            grant: None,
        },
        timeout: Duration::from_secs(60),
        default: Answer::Decline,
    }
}

fn method(frame: &Value) -> &str {
    frame
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

fn cancelled_id(frame: &Value) -> String {
    assert_eq!(method(frame), "$/cancel_request", "frame: {frame}");
    frame
        .get("params")
        .and_then(|params| params.get("requestId"))
        .and_then(|id| id.as_str())
        .expect("requestId")
        .to_owned()
}

fn ended() -> UpdateKind {
    UpdateKind::TurnEnded {
        turn: TURN,
        stop: Stop::EndTurn,
    }
}

#[tokio::test]
async fn pump_keeps_reading_updates_while_a_permission_is_outstanding() {
    let mut fx = fixture().await;
    let writer = fx.transport.writer();
    let source = fx.source.take().expect("source");
    let cancel = CancellationToken::new();
    let (state, agent, session) = (Arc::clone(&fx.state), fx.agent.clone(), fx.session);
    let ctx = PumpCtx {
        state: &state,
        writer: &writer,
        agent: &agent,
        session,
        version: AcpVersion::V1,
        prompt_turn: TURN,
    };
    let asked = approval();
    let driver = async {
        fx.push(UpdateKind::RequestOpened(asked.clone())).await;
        let request = fx.frame().await;
        assert_eq!(method(&request), "session/request_permission");
        let client = request
            .get("id")
            .and_then(|id| id.as_str())
            .expect("string id")
            .to_owned();
        fx.push(UpdateKind::Notice(Notice {
            turn: None,
            kind: "status".into(),
            text: "later notice".into(),
        }))
        .await;
        let notice = fx.frame().await;
        assert_eq!(method(&notice), "session/update");
        assert!(
            sonic_rs::to_string(&notice)
                .expect("encode")
                .contains("later notice")
        );
        fx.push(UpdateKind::RequestResolved {
            id: asked.id,
            answer: Answer::Approve,
            by: ClientId::new("tui#1"),
        })
        .await;
        assert_eq!(cancelled_id(&fx.frame().await), client);
        assert_eq!(fx.open_count().await, (0, 0));
        fx.push(ended()).await;
    };
    let (end, ()) = tokio::join!(prompt_pump(&ctx, source, &cancel), driver);
    assert!(matches!(end, PromptEnd::Stopped(Stop::EndTurn)));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fx.peer.read_frame())
            .await
            .is_err(),
        "no duplicate cancel after the resolved request"
    );
}

#[tokio::test]
async fn cancel_withdraws_an_outstanding_permission_without_waiting_for_it() {
    let mut fx = fixture().await;
    let writer = fx.transport.writer();
    let source = fx.source.take().expect("source");
    let cancel = CancellationToken::new();
    let (state, agent, session) = (Arc::clone(&fx.state), fx.agent.clone(), fx.session);
    let ctx = PumpCtx {
        state: &state,
        writer: &writer,
        agent: &agent,
        session,
        version: AcpVersion::V2,
        prompt_turn: TURN,
    };
    let driver = async {
        fx.push(UpdateKind::RequestOpened(approval())).await;
        let request = fx.frame().await;
        assert_eq!(method(&request), "session/request_permission");
        let client = request
            .get("id")
            .and_then(|id| id.as_str())
            .map(str::to_owned);
        cancel.cancel();
        assert_eq!(Some(cancelled_id(&fx.frame().await)), client);
        assert_eq!(fx.open_count().await, (0, 0));
        fx.push(ended()).await;
    };
    let (end, ()) = tokio::join!(prompt_pump(&ctx, source, &cancel), driver);
    assert!(matches!(end, PromptEnd::Stopped(Stop::EndTurn)));
}

#[tokio::test]
async fn pump_end_cancels_the_client_request_of_an_open_wait() {
    let mut fx = fixture().await;
    let writer = fx.transport.writer();
    let source = fx.source.take().expect("source");
    let cancel = CancellationToken::new();
    let (state, agent, session) = (Arc::clone(&fx.state), fx.agent.clone(), fx.session);
    let ctx = PumpCtx {
        state: &state,
        writer: &writer,
        agent: &agent,
        session,
        version: AcpVersion::V1,
        prompt_turn: TURN,
    };
    let driver = async {
        fx.push(UpdateKind::RequestOpened(approval())).await;
        let request = fx.frame().await;
        let client = request
            .get("id")
            .and_then(|id| id.as_str())
            .map(str::to_owned);
        fx.push(ended()).await;
        client
    };
    let (end, client) = tokio::join!(prompt_pump(&ctx, source, &cancel), driver);
    assert!(matches!(end, PromptEnd::Stopped(Stop::EndTurn)));
    assert_eq!(Some(cancelled_id(&fx.frame().await)), client);
    assert_eq!(fx.open_count().await, (0, 0));
}

fn confirm() -> Request {
    Request {
        question: Question::Confirm {
            text: "deploy?".into(),
        },
        ..approval()
    }
}

#[tokio::test]
async fn client_answer_completes_the_wait_inside_the_pump() {
    let mut fx = fixture().await;
    let writer = fx.transport.writer();
    let source = fx.source.take().expect("source");
    let cancel = CancellationToken::new();
    let (state, agent, session) = (Arc::clone(&fx.state), fx.agent.clone(), fx.session);
    let ctx = PumpCtx {
        state: &state,
        writer: &writer,
        agent: &agent,
        session,
        version: AcpVersion::V2,
        prompt_turn: TURN,
    };
    let driver = async {
        fx.push(UpdateKind::RequestOpened(confirm())).await;
        let request = fx.frame().await;
        assert_eq!(method(&request), "session/request_permission");
        let params = &request["params"];
        assert_eq!(params["title"].as_str(), Some("deploy?"));
        assert_eq!(params["subject"]["type"].as_str(), Some("tool_call"));
        assert_eq!(params["options"][0]["optionId"].as_str(), Some("yes"));
        let client = request["id"].as_str().expect("string id").to_owned();
        let sender = fx
            .state
            .lock()
            .await
            .pending
            .remove(&client)
            .map(|(_, sender)| sender)
            .expect("wait is registered");
        let answer = sonic_rs::json!({"outcome": "selected", "optionId": "yes"});
        assert!(sender.send(ServerAnswer { result: answer }).is_ok());
        tokio::time::timeout(Duration::from_secs(5), async {
            while fx.open_count().await != (0, 0) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the flow finishes while the pump runs");
        fx.push(ended()).await;
    };
    let (end, ()) = tokio::join!(prompt_pump(&ctx, source, &cancel), driver);
    assert!(matches!(end, PromptEnd::Stopped(Stop::EndTurn)));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fx.peer.read_frame())
            .await
            .is_err(),
        "an answered request gets no cancel"
    );
}

#[tokio::test]
async fn text_deltas_become_agent_message_chunks() {
    let mut fx = fixture().await;
    let writer = fx.transport.writer();
    let source = fx.source.take().expect("source");
    let cancel = CancellationToken::new();
    let (state, agent, session) = (Arc::clone(&fx.state), fx.agent.clone(), fx.session);
    let ctx = PumpCtx {
        state: &state,
        writer: &writer,
        agent: &agent,
        session,
        version: AcpVersion::V1,
        prompt_turn: TURN,
    };
    let driver = async {
        let mut chunks = String::new();
        for text in ["Hel", "lo"] {
            fx.push(UpdateKind::Delta {
                turn: TURN,
                channel: StreamChannel::Text,
                text: text.into(),
            })
            .await;
            let frame = fx.frame().await;
            let update = &frame["params"]["update"];
            assert_eq!(
                update["sessionUpdate"].as_str(),
                Some("agent_message_chunk")
            );
            chunks.push_str(update["content"]["text"].as_str().expect("chunk text"));
        }
        fx.push(ended()).await;
        chunks
    };
    let (end, chunks) = tokio::join!(prompt_pump(&ctx, source, &cancel), driver);
    assert!(matches!(end, PromptEnd::Stopped(Stop::EndTurn)));
    assert_eq!(chunks, "Hello");
}
