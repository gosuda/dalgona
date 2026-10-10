//! Scope outcomes against held upstream completions: a cancelled or dropped
//! scope stays cancelled, and a finished handle is accounted once.

use std::collections::VecDeque;
use std::future::Future;
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_core::ext::{McpDeclaration, McpRequest, McpResponse};
use dal_core::{
    AgentReport, AgentStart, AgentsOp, AgentsReply, Answer, Budget, CallId, ContextItem, EntryId,
    FetchRequest, FetchResponse, HandleStatus, Inference, JobsOp, JobsReply, ModelRequest,
    ModelRoute, Name, Notice, OnError, Origin, Part, Purpose, Question, RequestParams, RunOutput,
    RunRequest, ScopeSpec, ServiceSet, SessionId, SidecarOp, StateError, StateOp, StateRecord,
    Stop, TurnOp, TurnOpReply, Usage,
};
use dal_provider::{EventStream, ProviderError, StopReason, StreamEvent};
use futures::channel::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{Scope, ScopeError, ScopeValue, Shared, locked};
use crate::ext::services::ServiceFuture;
use crate::ext::{Caller, CallerKind, RawValue, Services, ToolOutcome};

macro_rules! never {
    () => {
        unreachable!("scope tests call only inference and member sessions")
    };
}

type Upstream = mpsc::UnboundedSender<Result<StreamEvent, ProviderError>>;

struct Held {
    stream: Mutex<Option<EventStream>>,
    report: Mutex<VecDeque<oneshot::Receiver<AgentReport>>>,
    awaiting: AtomicUsize,
}

impl Services for Held {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        never!()
    }
    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        never!()
    }
    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        never!()
    }
    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        never!()
    }
    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        never!()
    }
    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        never!()
    }
    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        never!()
    }
    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        never!()
    }
    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn crate::ext::tool::Tool>, dal_core::Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        never!()
    }
    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        never!()
    }
    fn agents(&self, _who: &Caller, op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        let reply = match op {
            AgentsOp::Start(_) => Ok(AgentsReply::Started { id: member() }),
            AgentsOp::Await { .. } => {
                let report = locked(&self.report).pop_front();
                self.awaiting.fetch_add(1, Ordering::SeqCst);
                return Box::pin(async move {
                    match report {
                        Some(report) => match report.await {
                            Ok(report) => Ok(AgentsReply::Await { report }),
                            Err(_) => std::future::pending().await,
                        },
                        None => std::future::pending().await,
                    }
                });
            }
            AgentsOp::Cancel { id } => Ok(AgentsReply::Cancelled { id }),
            _ => never!(),
        };
        Box::pin(async move { reply })
    }
    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        never!()
    }
    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        never!()
    }
    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<crate::ext::Doc>> {
        never!()
    }
    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        never!()
    }
    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        never!()
    }
    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        never!()
    }
    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        never!()
    }
    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        let stream = locked(&self.stream).take().expect("one scripted stream");
        Box::pin(async move { Ok(stream) })
    }
    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        never!()
    }
    fn notify(&self, _who: &Caller, _notice: Notice) {
        never!()
    }
    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        never!()
    }
    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        never!()
    }
    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        never!()
    }
    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        never!()
    }
}

fn member() -> SessionId {
    SessionId::new_v7()
}

struct Rig {
    scope: Scope,
    shared: Arc<Shared>,
    held: Arc<Held>,
    upstream: Upstream,
    upstream_cancelled: Arc<AtomicBool>,
}

fn rig() -> Rig {
    let (upstream, source) = mpsc::unbounded();
    let upstream_cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&upstream_cancelled);
    let held = Arc::new(Held {
        stream: Mutex::new(Some(EventStream::new(source, move || {
            flag.store(true, Ordering::SeqCst);
        }))),
        report: Mutex::new(VecDeque::new()),
        awaiting: AtomicUsize::new(0),
    });
    let caller = Caller::new(
        "scope-test".parse::<Name>().expect("name"),
        Origin::Builtin,
        ServiceSet::EMPTY,
        NonZeroU32::MIN,
        CallerKind::Handler,
        None,
    );
    let services: Arc<dyn Services> = held.clone();
    let scope = Scope::open(
        services,
        caller,
        CancellationToken::new(),
        &ScopeSpec {
            limit: 4,
            on_error: OnError::Settle,
            budget: Budget::default(),
        },
        None,
    )
    .expect("scope opens");
    let shared = Arc::clone(&scope.inner.shared);
    Rig {
        scope,
        shared,
        held,
        upstream,
        upstream_cancelled,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: dal_core::Family::Chat,
            model: "scripted".into(),
        },
        system: "".into(),
        tools: Arc::from(Vec::new()),
        context: Arc::from(vec![ContextItem::User {
            parts: vec![Part::Text { text: "hi".into() }],
        }]),
        params: RequestParams::default(),
        cache_key: None,
    }
}

fn start() -> AgentStart {
    AgentStart {
        call: CallId::new("member"),
        name: "member".into(),
        prompt: "work".into(),
        model: None,
        role: None,
        system: None,
        tools: None,
        workspace: None,
    }
}

fn report() -> AgentReport {
    AgentReport {
        stop: Stop::EndTurn,
        text: "done".into(),
        session: member(),
        entry: EntryId::new(NonZeroU64::MIN),
    }
}

fn first_event() -> StreamEvent {
    StreamEvent::TextDelta { text: "he".into() }
}

fn finish_events(upstream: &Upstream) {
    let usage = Usage {
        input_tokens: 3,
        cached_input_tokens: 0,
        output_tokens: 2,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    };
    for event in [
        Ok(StreamEvent::TextDelta { text: "llo".into() }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage { usage }),
        Ok(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ] {
        let _ = upstream.unbounded_send(event);
    }
    upstream.close_channel();
}

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition reached");
}

async fn collecting(rig: &Rig) -> super::ScopeHandle {
    let handle = rig.scope.infer(request()).expect("admitted");
    rig.upstream
        .unbounded_send(Ok(first_event()))
        .expect("upstream open");
    until(|| locked(&rig.held.stream).is_none()).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(handle.status(), HandleStatus::Running);
    handle
}

async fn quiesced(shared: &Arc<Shared>) {
    until(|| Arc::strong_count(shared) == 1).await;
}

fn done(shared: &Shared) -> usize {
    locked(&shared.book).done
}

fn progress(shared: &Shared) -> u64 {
    *shared.progress.borrow()
}

fn requests(shared: &Shared) -> u64 {
    locked(&shared.ledger.state).used.requests
}

async fn timed<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("settles in time")
}

#[tokio::test]
async fn a_dropped_scope_keeps_an_open_inference_cancelled_after_upstream_completes() {
    let rig = rig();
    let handle = collecting(&rig).await;
    let Rig {
        scope,
        shared,
        upstream,
        ..
    } = rig;
    drop(scope);
    assert_eq!(handle.status(), HandleStatus::Cancelled);
    let settled = (done(&shared), progress(&shared), requests(&shared));

    finish_events(&upstream);
    quiesced(&shared).await;

    assert_eq!(handle.status(), HandleStatus::Cancelled);
    assert_eq!(handle.error(), Some(ScopeError::Cancelled));
    assert!(matches!(handle.result().await, Err(ScopeError::Cancelled)));
    assert_eq!(handle.usage().input_tokens, 0, "no late usage");
    assert_eq!(
        (done(&shared), progress(&shared), requests(&shared)),
        settled,
        "no late counter mutation"
    );
}

#[tokio::test]
async fn a_dropped_scope_reaches_the_open_inference_stream() {
    let rig = rig();
    let handle = collecting(&rig).await;
    let Rig {
        scope,
        shared,
        upstream_cancelled,
        ..
    } = rig;
    drop(scope);
    quiesced(&shared).await;
    assert!(
        upstream_cancelled.load(Ordering::SeqCst),
        "collection stopped and the upstream stream was dropped"
    );
    assert_eq!(handle.status(), HandleStatus::Cancelled);
}

#[tokio::test]
async fn cancelling_a_scope_reaches_the_open_inference_stream() {
    let rig = rig();
    let handle = collecting(&rig).await;
    rig.scope.cancel();
    let result = timed(handle.result()).await;
    assert!(matches!(result, Err(ScopeError::Cancelled)));
    assert_eq!(handle.status(), HandleStatus::Cancelled);
    assert!(rig.upstream_cancelled.load(Ordering::SeqCst));
    let settled = (
        done(&rig.shared),
        progress(&rig.shared),
        requests(&rig.shared),
    );
    finish_events(&rig.upstream);
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        (
            done(&rig.shared),
            progress(&rig.shared),
            requests(&rig.shared)
        ),
        settled,
        "late upstream completion does not mutate accounting"
    );
    assert_eq!(
        requests(&rig.shared),
        0,
        "a cancelled handle charges nothing"
    );
}

#[tokio::test]
async fn a_dropped_scope_keeps_a_member_session_cancelled_when_its_report_arrives() {
    for _ in 0..48 {
        let rig = rig();
        let (report_tx, report_rx) = oneshot::channel();
        locked(&rig.held.report).push_back(report_rx);
        let handle = rig.scope.agent(start()).expect("admitted");
        until(|| rig.held.awaiting.load(Ordering::SeqCst) == 1).await;
        let Rig { scope, shared, .. } = rig;
        drop(scope);
        let settled = (done(&shared), progress(&shared));
        assert!(report_tx.send(report()).is_ok() || handle.status() == HandleStatus::Cancelled);

        quiesced(&shared).await;

        assert_eq!(handle.status(), HandleStatus::Cancelled);
        assert!(matches!(handle.result().await, Err(ScopeError::Cancelled)));
        assert_eq!((done(&shared), progress(&shared)), settled);
    }
}

#[tokio::test]
async fn a_finished_handle_is_accounted_once_and_survives_scope_drop() {
    let rig = rig();
    let handle = collecting(&rig).await;
    finish_events(&rig.upstream);
    let finished = rig.scope.next().await.expect("one finished handle");
    assert_eq!(finished.id(), handle.id());
    assert!(rig.scope.next().await.is_none(), "delivered once");
    assert_eq!(handle.status(), HandleStatus::Done);
    assert_eq!(handle.usage().input_tokens, 3);
    assert_eq!(requests(&rig.shared), 1);
    assert_eq!(locked(&rig.shared.ledger.state).used.input_tokens, 3);
    assert_eq!((done(&rig.shared), progress(&rig.shared)), (1, 1));

    let Rig { scope, shared, .. } = rig;
    drop(scope);
    quiesced(&shared).await;

    assert_eq!(handle.status(), HandleStatus::Done);
    assert!(matches!(
        handle.result().await,
        Ok(ScopeValue::Inference(_))
    ));
    assert_eq!(requests(&shared), 1);
    assert_eq!((done(&shared), progress(&shared)), (1, 1));
}
