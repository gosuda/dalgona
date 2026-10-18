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
    FetchRequest, FetchResponse, HandleStatus, Inference, JobsOp, JobsReply, ModelPrice,
    ModelRequest, ModelRoute, Name, Notice, OnError, Origin, Part, PrivateEffect, Purpose,
    Question, RequestParams, RunOutput, RunRequest, ScopeSpec, ServiceSet, SessionId, SidecarOp,
    StateError, StateOp, StateRecord, Stop, TurnOp, TurnOpReply, Usage,
};
use dal_provider::{EventStream, ProviderError, StopReason, StreamEvent};
use futures::channel::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{Scope, ScopeError, ScopeValue, Shared, locked};
use crate::error::ServiceError;
use crate::ext::services::ServiceFuture;
use crate::ext::{Caller, CallerKind, RawValue, Services, ToolOutcome};

macro_rules! never {
    () => {
        unreachable!("scope tests call only inference and member sessions")
    };
}

type Upstream = mpsc::UnboundedSender<Result<StreamEvent, ProviderError>>;

type AgentsResult = Result<AgentsReply, ServiceError>;

struct Held {
    stream: Mutex<Option<EventStream>>,
    report: Mutex<VecDeque<oneshot::Receiver<AgentReport>>>,
    awaits: Mutex<VecDeque<AgentsResult>>,
    close_replies: Mutex<VecDeque<AgentsResult>>,
    started: Mutex<Vec<SessionId>>,
    closed: Mutex<Vec<SessionId>>,
    hold_start: Mutex<Option<oneshot::Receiver<()>>>,
    awaiting: AtomicUsize,
    inferred: AtomicUsize,
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
            AgentsOp::Start(_) => {
                let id = member();
                locked(&self.started).push(id);
                let held = locked(&self.hold_start)
                    .take()
                    .unwrap_or_else(|| oneshot::channel().1);
                return Box::pin(async move {
                    let _ = held.await;
                    Ok(AgentsReply::Started { id })
                });
            }
            AgentsOp::Await { .. } => {
                self.awaiting.fetch_add(1, Ordering::SeqCst);
                if let Some(scripted) = locked(&self.awaits).pop_front() {
                    return Box::pin(async move { scripted });
                }
                let report = locked(&self.report).pop_front();
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
            AgentsOp::Cancel { id } => {
                locked(&self.closed).push(id);
                locked(&self.close_replies)
                    .pop_front()
                    .unwrap_or(Ok(AgentsReply::Cancelled { id }))
            }
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
    fn model_ready(
        &self,
        _who: &Caller,
        _model: dal_core::ModelRoute,
    ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
        never!()
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        never!()
    }
    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        self.inferred.fetch_add(1, Ordering::SeqCst);
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
    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
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
fn caller() -> Caller {
    Caller::new(
        "scope-test".parse::<Name>().expect("name"),
        Origin::Builtin,
        ServiceSet::EMPTY,
        NonZeroU32::MIN,
        CallerKind::Handler,
        None,
    )
}

struct Rig {
    scope: Scope,
    shared: Arc<Shared>,
    held: Arc<Held>,
    upstream: Upstream,
    upstream_cancelled: Arc<AtomicBool>,
}

fn rig_with(budget: Budget, price: Option<super::PriceFn>) -> Rig {
    let (upstream, source) = mpsc::unbounded();
    let upstream_cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&upstream_cancelled);
    let held = Arc::new(Held {
        stream: Mutex::new(Some(EventStream::new(source, move || {
            flag.store(true, Ordering::SeqCst);
        }))),
        report: Mutex::new(VecDeque::new()),
        awaits: Mutex::new(VecDeque::new()),
        close_replies: Mutex::new(VecDeque::new()),
        started: Mutex::new(Vec::new()),
        closed: Mutex::new(Vec::new()),
        hold_start: Mutex::new(None),
        awaiting: AtomicUsize::new(0),
        inferred: AtomicUsize::new(0),
    });
    let services: Arc<dyn Services> = held.clone();
    let scope = Scope::open(
        services,
        caller(),
        CancellationToken::new(),
        &ScopeSpec {
            limit: 4,
            on_error: OnError::Settle,
            budget,
        },
        price,
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

fn rig() -> Rig {
    rig_with(Budget::default(), None)
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
        name: Some("member".into()),
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

fn measured_usage() -> Usage {
    Usage {
        input_tokens: 3,
        cached_input_tokens: 0,
        output_tokens: 2,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

/// An all-zero usage with no cost: the provider measured nothing.
fn unreported_usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

fn finish_events_with(upstream: &Upstream, usage: Option<Usage>) {
    let mut events: Vec<Result<StreamEvent, ProviderError>> = vec![
        Ok(StreamEvent::TextDelta { text: "llo".into() }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
    ];
    if let Some(usage) = usage {
        events.push(Ok(StreamEvent::Usage { usage }));
    }
    events.push(Ok(StreamEvent::Stop {
        reason: StopReason::EndTurn,
    }));
    for event in events {
        let _ = upstream.unbounded_send(event);
    }
    upstream.close_channel();
}

fn finish_events(upstream: &Upstream) {
    finish_events_with(upstream, Some(measured_usage()));
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

#[tokio::test]
async fn a_handle_dropped_before_its_first_poll_stays_cancelled() {
    let rig = rig();
    let handle = rig.scope.infer(request()).expect("admitted");
    let Rig {
        scope,
        shared,
        held,
        ..
    } = rig;
    // No yield between launch and drop: the drive is granted before it has
    // ever polled, and the cancelled handle must not restart to Running or
    // reach upstream work.
    drop(scope);
    quiesced(&shared).await;
    assert_eq!(handle.status(), HandleStatus::Cancelled);
    assert!(matches!(handle.result().await, Err(ScopeError::Cancelled)));
    assert_eq!(
        held.inferred.load(Ordering::SeqCst),
        0,
        "the drive never starts upstream work for a cancelled handle"
    );
}

async fn failed_member(rig: &Rig) -> (SessionId, String) {
    let handle = rig.scope.agent(start()).expect("admitted");
    let error = timed(handle.result()).await.expect_err("the member failed");
    let started = locked(&rig.held.started).clone();
    assert_eq!(started.len(), 1, "one member session started");
    (started[0], error.to_string())
}

fn wait_failure() -> ServiceError {
    ServiceError::failed(None, "the await broke")
}

#[tokio::test]
async fn a_failed_await_cancels_the_member_session_and_keeps_the_wait_error() {
    let rig = rig();
    locked(&rig.held.awaits).push_back(Err(wait_failure()));
    let (member, error) = failed_member(&rig).await;
    assert_eq!(
        *locked(&rig.held.closed),
        vec![member],
        "the child is closed"
    );
    assert_eq!(error, "the await broke");
}

#[tokio::test]
async fn a_failed_second_await_cancels_the_member_session_and_keeps_the_wait_error() {
    let rig = rig();
    let pending = SessionId::new_v7();
    locked(&rig.held.awaits).push_back(Ok(AgentsReply::Pending { id: pending }));
    locked(&rig.held.awaits).push_back(Err(wait_failure()));
    let (_, error) = failed_member(&rig).await;
    assert_eq!(
        *locked(&rig.held.closed),
        vec![pending],
        "the pending child is closed"
    );
    assert_eq!(error, "the await broke");
}

#[tokio::test]
async fn an_unexpected_close_reply_is_reported_with_the_wait_error() {
    let rig = rig();
    locked(&rig.held.awaits).push_back(Err(wait_failure()));
    locked(&rig.held.close_replies).push_back(Ok(AgentsReply::Pending { id: member() }));
    let (member, error) = failed_member(&rig).await;
    assert_eq!(*locked(&rig.held.closed), vec![member]);
    assert_eq!(
        error,
        format!(
            "the await broke; closing child session {member} returned an unexpected reply. Retry cancelling that session."
        )
    );
}

#[tokio::test]
async fn a_failed_close_is_reported_with_the_wait_error() {
    let rig = rig();
    locked(&rig.held.awaits).push_back(Err(wait_failure()));
    locked(&rig.held.close_replies).push_back(Err(ServiceError::failed(None, "the close broke")));
    let (member, error) = failed_member(&rig).await;
    assert_eq!(*locked(&rig.held.closed), vec![member]);
    assert_eq!(
        error,
        format!(
            "the await broke; closing child session {member} also failed: the close broke. Retry cancelling that session."
        )
    );
}

#[tokio::test]
async fn a_cancelled_start_closes_the_child_it_created() {
    let rig = rig();
    // The start lingers past the cancel, as a live backend start does, so
    // the child opens while the scope is already cancelling.
    let (hold_tx, hold_rx) = oneshot::channel::<()>();
    locked(&rig.held.hold_start).replace(hold_rx);
    let handle = rig.scope.agent(start()).expect("admitted");
    until(|| locked(&rig.held.hold_start).is_none()).await;
    rig.scope.cancel();
    hold_tx.send(()).expect("the gate opens");
    timed(handle.result())
        .await
        .expect_err("the handle cancels");
    assert_eq!(handle.status(), HandleStatus::Cancelled);
    let started = locked(&rig.held.started).clone();
    assert_eq!(started.len(), 1, "the start drained and created its child");
    assert_eq!(
        *locked(&rig.held.closed),
        vec![started[0]],
        "the cancelled start rolled its child back"
    );
}

#[tokio::test]
async fn cancelling_while_the_start_is_blocked_settles_first_and_rolls_the_child_back() {
    let rig = rig();
    let (hold_tx, hold_rx) = oneshot::channel::<()>();
    locked(&rig.held.hold_start).replace(hold_rx);
    let handle = rig.scope.agent(start()).expect("admitted");
    until(|| locked(&rig.held.hold_start).is_none()).await;
    rig.scope.cancel();
    timed(handle.result())
        .await
        .expect_err("the handle cancels while the start stays blocked");
    assert_eq!(handle.status(), HandleStatus::Cancelled);
    assert_eq!(timed(rig.scope.all()).await.len(), 1);
    hold_tx.send(()).expect("the gate opens");
    until(|| !locked(&rig.held.closed).is_empty()).await;
    let started = locked(&rig.held.started).clone();
    assert_eq!(started.len(), 1);
    assert_eq!(*locked(&rig.held.closed), vec![started[0]]);
}

#[tokio::test]
async fn dropping_the_scope_while_the_start_is_blocked_still_rolls_its_child_back() {
    let rig = rig();
    let (hold_tx, hold_rx) = oneshot::channel::<()>();
    locked(&rig.held.hold_start).replace(hold_rx);
    let handle = rig.scope.agent(start()).expect("admitted");
    until(|| locked(&rig.held.hold_start).is_none()).await;
    let Rig {
        scope,
        shared,
        held,
        ..
    } = rig;
    drop(scope);
    assert_eq!(handle.status(), HandleStatus::Cancelled);
    quiesced(&shared).await;
    // The start the drive was awaiting survives the scope in the cleanup set:
    // it resolves, and the child it produced is closed rather than stranded.
    hold_tx.send(()).expect("the gate opens");
    until(|| !locked(&held.closed).is_empty()).await;
    let started = locked(&held.started).clone();
    assert_eq!(started.len(), 1);
    assert_eq!(*locked(&held.closed), vec![started[0]]);
}

async fn admitted_inference(scope: &Scope, rig: &Rig) -> super::ScopeHandle {
    let handle = scope.infer(request()).expect("admitted");
    until(|| locked(&rig.held.stream).is_none()).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(handle.status(), HandleStatus::Running);
    handle
}

fn unit_price() -> super::PriceFn {
    Arc::new(|_| {
        Some(ModelPrice {
            input: 1.0,
            cached_input: 1.0,
            output: 1.0,
            reasoning: 1.0,
            cache_write: None,
            tiers: Box::default(),
        })
    })
}

#[tokio::test]
async fn missing_usage_after_a_successful_inference_exhausts_a_token_limited_scope() {
    let rig = rig_with(
        Budget {
            input_tokens: Some(10),
            ..Budget::default()
        },
        None,
    );
    let handle = admitted_inference(&rig.scope, &rig).await;
    finish_events_with(&rig.upstream, None);
    let value = timed(handle.result())
        .await
        .expect("the inference succeeds");
    assert!(matches!(value, ScopeValue::Inference(_)));
    assert_eq!(handle.status(), HandleStatus::Done);
    assert!(
        matches!(rig.scope.infer(request()), Err(ScopeError::Exhausted)),
        "a response with no usage exhausts the token budget instead of charging zero"
    );
}

#[tokio::test]
async fn all_zero_usage_exhausts_token_limits_without_failing_the_response() {
    let rig = rig_with(
        Budget {
            input_tokens: Some(100),
            output_tokens: Some(100),
            ..Budget::default()
        },
        None,
    );
    let handle = admitted_inference(&rig.scope, &rig).await;
    finish_events_with(&rig.upstream, Some(unreported_usage()));
    let value = timed(handle.result())
        .await
        .expect("the inference succeeds");
    assert!(matches!(value, ScopeValue::Inference(_)));
    assert_eq!(handle.status(), HandleStatus::Done);
    assert_eq!(
        handle.usage(),
        unreported_usage(),
        "the raw zero stays visible and unpriced"
    );
    assert!(
        matches!(rig.scope.infer(request()), Err(ScopeError::Exhausted)),
        "an all-zero usage exhausts the token budgets instead of charging zero"
    );
}

#[tokio::test]
async fn priced_all_zero_usage_still_exhausts_a_usd_limited_scope() {
    let rig = rig_with(
        Budget {
            usd: Some(1.0),
            ..Budget::default()
        },
        Some(unit_price()),
    );
    let handle = admitted_inference(&rig.scope, &rig).await;
    finish_events_with(&rig.upstream, Some(unreported_usage()));
    let value = timed(handle.result())
        .await
        .expect("the inference succeeds");
    assert!(matches!(value, ScopeValue::Inference(_)));
    // Local pricing would value the raw zero at $0.00; the ledger judges the
    // raw event before pricing, so the next request is refused.
    assert!(
        matches!(rig.scope.infer(request()), Err(ScopeError::Exhausted)),
        "a priced all-zero usage exhausts the USD budget instead of charging $0.00"
    );
}

#[tokio::test]
async fn measured_usage_below_the_limit_stays_admitted() {
    let rig = rig_with(
        Budget {
            input_tokens: Some(10),
            ..Budget::default()
        },
        None,
    );
    let handle = admitted_inference(&rig.scope, &rig).await;
    finish_events(&rig.upstream);
    timed(handle.result())
        .await
        .expect("the inference succeeds");
    assert_eq!(locked(&rig.shared.ledger.state).used.input_tokens, 3);
    assert!(
        rig.scope.infer(request()).is_ok(),
        "measured usage below the limit does not exhaust"
    );
}

#[tokio::test]
async fn request_only_budgets_do_not_need_a_spend_measurement() {
    let rig = rig_with(
        Budget {
            requests: Some(4),
            ..Budget::default()
        },
        None,
    );
    let handle = admitted_inference(&rig.scope, &rig).await;
    finish_events_with(&rig.upstream, None);
    timed(handle.result())
        .await
        .expect("the inference succeeds");
    assert!(
        rig.scope.infer(request()).is_ok(),
        "a request-only scope stays open after an unmeasured response"
    );
}

#[tokio::test]
async fn an_agent_handle_without_usage_leaves_spend_limits_open() {
    let rig = rig_with(
        Budget {
            input_tokens: Some(10),
            ..Budget::default()
        },
        None,
    );
    let (report_tx, report_rx) = oneshot::channel();
    locked(&rig.held.report).push_back(report_rx);
    let handle = rig.scope.agent(start()).expect("admitted");
    until(|| rig.held.awaiting.load(Ordering::SeqCst) == 1).await;
    report_tx.send(report()).expect("the report delivers");
    let value = timed(handle.result()).await.expect("the member completes");
    assert!(matches!(value, ScopeValue::Agent(_)));
    assert!(
        rig.scope.infer(request()).is_ok(),
        "a member session's legitimate None usage is not unmetered spend"
    );
}

#[tokio::test]
async fn unreported_usage_in_a_child_scope_exhausts_the_parent_budget() {
    let rig = rig_with(
        Budget {
            input_tokens: Some(10),
            ..Budget::default()
        },
        None,
    );
    let parent_ledger = Arc::clone(&rig.shared.ledger);
    let lineage = crate::ext::synthetic::Lineage::default().with_ledger(parent_ledger);
    let child = crate::ext::synthetic::enter(lineage, async {
        let services: Arc<dyn Services> = rig.held.clone();
        Scope::open(
            services,
            caller(),
            CancellationToken::new(),
            &ScopeSpec {
                limit: 2,
                on_error: OnError::Settle,
                budget: Budget::default(),
            },
            None,
        )
        .expect("child scope opens")
    })
    .await;
    assert!(
        child.inner.shared.ledger.parent.is_some(),
        "the child rolls up to the parent ledger"
    );
    let handle = admitted_inference(&child, &rig).await;
    finish_events_with(&rig.upstream, None);
    let value = timed(handle.result())
        .await
        .expect("the inference succeeds");
    assert!(matches!(value, ScopeValue::Inference(_)));
    assert!(
        matches!(rig.scope.infer(request()), Err(ScopeError::Exhausted)),
        "the unmetered child spend propagates to the parent ledger"
    );
}
