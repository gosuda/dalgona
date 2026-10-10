//! Fan-out scopes: handles, FIFO admission, error policy, and budgets.
//!
//! A [`Scope`] owns every handle it starts. Handles beyond `limit` wait in
//! FIFO order as `pending`; a finished handle's result stays readable until
//! the scope drops, and dropping the scope cancels every unfinished handle.
//! One [`Ledger`] per scope rolls usage up through every enclosing scope.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use dal_core::{
    AgentReport, AgentStart, AgentsOp, AgentsReply, Budget, DenyReason, HandleStatus, InferFailure,
    Inference, MailMode, ModelPrice, ModelRequest, ModelRoute, OnError, Receipt, ScopeSpec,
    ScopeSpecError, ScopeUsage, SessionId, StreamEvent, Usage,
};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::synthetic::{self, Lineage};
use super::{BoxFuture, Caller, Services};
use crate::error::{ServiceError, deny_text};
use crate::host::ops::request_reference;

/// The lifecycle state of one scope handle.
pub use dal_core::HandleStatus as ScopeStatus;

/// The largest `limit` a scope accepts; it never raises the member cap.
pub const GLOBAL_MEMBER_CAP: u16 = 500;

/// Why a scope operation was refused or a handle failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ScopeError {
    /// A budget limit was reached.
    #[error("scope budget exhausted")]
    Exhausted,
    /// The scope, the handle, or the enclosing call was cancelled.
    #[error("scope cancelled")]
    Cancelled,
    /// A USD-limited scope reached a route with no known price.
    #[error("model `{model}` has no known price; a usd budget cannot admit it")]
    UnpricedModel {
        /// The unpriced model reference.
        model: Box<str>,
    },
    /// The scope specification was invalid.
    #[error("{0}")]
    Spec(#[from] ScopeSpecError),
    /// The operation is unavailable in this context.
    #[error("{}", deny_text(.0))]
    Denied(DenyReason),
    /// An inference failed, retaining typed synthetic cycle and depth data.
    #[error("{0}")]
    Infer(InferFailure),
    /// The handle's work failed with this text.
    #[error("{0}")]
    Failed(Box<str>),
}

/// The value of one finished handle.
#[derive(Clone, Debug, PartialEq)]
pub enum ScopeValue {
    /// A member session's report.
    Agent(AgentReport),
    /// A whole inference.
    Inference(Inference),
}

/// The stable identity of one handle inside its scope.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScopeHandleId(u64);

impl fmt::Display for ScopeHandleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "h{}", self.0)
    }
}

pub(crate) type PriceFn = Arc<dyn Fn(&ModelRoute) -> Option<ModelPrice> + Send + Sync>;

/// The service calls behind one scope: inference and member sessions both
/// run through [`Services`], so every gate and every denial applies.
struct Runtime {
    services: Arc<dyn Services>,
    caller: Caller,
    lineage: Lineage,
    price: Option<PriceFn>,
}

fn service_error(error: ServiceError) -> ScopeError {
    match error {
        ServiceError::Denied(reason) => ScopeError::Denied(reason),
        ServiceError::Cancelled => ScopeError::Cancelled,
        other => ScopeError::Failed(other.to_string().into()),
    }
}

/// Closes a member session whose wait failed, so no child outlives its
/// failed handle. A close that fails too keeps both failures in the error.
async fn close_after_failure(
    services: &Arc<dyn Services>,
    caller: &Caller,
    id: SessionId,
    wait_error: ServiceError,
) -> ScopeError {
    let failure = service_error(wait_error);
    match services.agents(caller, AgentsOp::Cancel { id }).await {
        Ok(AgentsReply::Cancelled { .. }) => failure,
        Ok(_) => ScopeError::Failed(
            format!("{failure}; closing child session {id} returned an unexpected reply. Retry cancelling that session.").into(),
        ),
        Err(close_error) => ScopeError::Failed(
            format!("{failure}; closing child session {id} also failed: {close_error}. Retry cancelling that session.").into(),
        ),
    }
}

/// Closes the child a cancelled start may have created, so no member
/// session outlives its cancelled handle. A start that never opened a
/// child leaves nothing to close.
async fn close_cancelled_start(
    services: &Arc<dyn Services>,
    caller: &Caller,
    child: &Arc<Mutex<Option<SessionId>>>,
    started: Result<AgentsReply, ServiceError>,
) {
    let Ok(AgentsReply::Started { id }) = started else {
        return;
    };
    *locked(child) = Some(id);
    let _ = services.agents(caller, AgentsOp::Cancel { id }).await;
}

impl Runtime {
    fn price(&self, route: &ModelRoute) -> Option<ModelPrice> {
        match &self.price {
            Some(price) => price(route),
            None => dal_provider::compiled_price(&request_reference(route)),
        }
    }

    fn infer(
        &self,
        request: ModelRequest,
        ledger: Arc<Ledger>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Inference, ScopeError>> {
        let services = Arc::clone(&self.services);
        let caller = self.caller.clone();
        let lineage = self.lineage.clone().with_ledger(ledger);
        Box::pin(synthetic::enter(lineage, async move {
            let run = async {
                let stream = services
                    .infer_stream(&caller, request)
                    .await
                    .map_err(service_error)?;
                synthetic::collect(stream).await.map_err(|failure| {
                    if failure == InferFailure::Cancelled {
                        ScopeError::Cancelled
                    } else {
                        ScopeError::Infer(failure)
                    }
                })
            };
            tokio::select! {
                result = run => result,
                () = cancel.cancelled() => Err(ScopeError::Cancelled),
            }
        }))
    }

    fn agent(
        &self,
        start: AgentStart,
        child: Arc<Mutex<Option<SessionId>>>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<AgentReport, ScopeError>> {
        let services = Arc::clone(&self.services);
        let caller = self.caller.clone();
        Box::pin(async move {
            // The start is drained even when the scope is cancelled: the
            // backend may already have opened the child, and dropping the
            // call would strand that half-created child. A child the
            // cancelled start produced is closed, so no member outlives
            // its cancelled handle.
            let started = services.agents(&caller, AgentsOp::Start(start)).await;
            if cancel.is_cancelled() {
                close_cancelled_start(&services, &caller, &child, started).await;
                return Err(ScopeError::Cancelled);
            }
            let id = match started.map_err(service_error)? {
                AgentsReply::Started { id } => id,
                AgentsReply::Refused { reason } => {
                    return Err(ScopeError::Failed(reason.to_string().into()));
                }
                _ => {
                    return Err(ScopeError::Failed(
                        "the member session did not start".into(),
                    ));
                }
            };
            *locked(&child) = Some(id);
            let awaited = tokio::select! {
                reply = services.agents(&caller, AgentsOp::Await { id, timeout: None }) => reply,
                () = cancel.cancelled() => {
                    let _ = services.agents(&caller, AgentsOp::Cancel { id }).await;
                    return Err(ScopeError::Cancelled);
                }
            };
            let mut awaited = match awaited {
                Ok(reply) => reply,
                Err(error) => return Err(close_after_failure(&services, &caller, id, error).await),
            };
            loop {
                match awaited {
                    AgentsReply::Await { report } => return Ok(report),
                    AgentsReply::Cancelled { .. } => return Err(ScopeError::Cancelled),
                    AgentsReply::Pending { id } => {
                        tokio::select! {
                            reply = services.agents(&caller, AgentsOp::Await { id, timeout: None }) => {
                                awaited = match reply {
                                    Ok(reply) => reply,
                                    Err(error) => {
                                        return Err(close_after_failure(&services, &caller, id, error).await);
                                    }
                                };
                            }
                            () = cancel.cancelled() => {
                                let _ = services.agents(&caller, AgentsOp::Cancel { id }).await;
                                return Err(ScopeError::Cancelled);
                            }
                        }
                    }
                    _ => {
                        return Err(ScopeError::Failed(
                            "the member session gave no report".into(),
                        ));
                    }
                }
            }
        })
    }

    fn send(
        &self,
        to: SessionId,
        text: Box<str>,
        mode: MailMode,
    ) -> BoxFuture<'static, Result<Receipt, ServiceError>> {
        let services = Arc::clone(&self.services);
        let caller = self.caller.clone();
        Box::pin(async move {
            let send = AgentsOp::Send {
                to,
                text,
                mode,
                reply_to: None,
            };
            match services.agents(&caller, send).await? {
                AgentsReply::Delivered(receipt) => Ok(receipt),
                _ => Err(ServiceError::failed(
                    Some(dal_core::Service::Agents),
                    "the mailbox gave no receipt",
                )),
            }
        })
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Budget accounting for one scope, rolled up through its parents.
pub(crate) struct Ledger {
    budget: Budget,
    started: Instant,
    parent: Option<Arc<Ledger>>,
    cancel: CancellationToken,
    state: Mutex<LedgerState>,
}

struct LedgerState {
    used: ScopeUsage,
    admitted: u64,
    refusal: Option<ScopeError>,
}

impl Ledger {
    fn new(budget: Budget, parent: Option<Arc<Ledger>>, cancel: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            budget,
            started: Instant::now(),
            parent,
            cancel,
            state: Mutex::new(LedgerState {
                used: ScopeUsage::default(),
                admitted: 0,
                refusal: None,
            }),
        })
    }

    fn refuse(&self, reason: ScopeError) {
        let mut state = locked(&self.state);
        if state.refusal.is_none() {
            state.refusal = Some(reason);
        }
        drop(state);
        self.cancel.cancel();
    }

    fn refusal(&self) -> Option<ScopeError> {
        if let Some(reason) = locked(&self.state).refusal.clone() {
            return Some(reason);
        }
        if self
            .budget
            .wall
            .is_some_and(|wall| self.started.elapsed() >= wall)
        {
            self.refuse(ScopeError::Exhausted);
            return Some(ScopeError::Exhausted);
        }
        self.parent.as_ref().and_then(|parent| parent.refusal())
    }

    fn admit(&self) -> Result<(), ScopeError> {
        if let Some(reason) = self.refusal() {
            return Err(reason);
        }
        {
            let mut state = locked(&self.state);
            let count = state.admitted.max(state.used.requests);
            if self.budget.requests.is_some_and(|limit| count >= limit) {
                return Err(ScopeError::Exhausted);
            }
            state.admitted += 1;
        }
        if let Some(parent) = &self.parent
            && let Err(error) = parent.admit()
        {
            locked(&self.state).admitted -= 1;
            return Err(error);
        }
        Ok(())
    }

    fn requires_usd(&self) -> bool {
        self.budget.usd.is_some()
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.requires_usd())
    }

    fn over(&self, used: &ScopeUsage) -> bool {
        let budget = &self.budget;
        budget
            .input_tokens
            .is_some_and(|limit| used.input_tokens >= limit)
            || budget
                .output_tokens
                .is_some_and(|limit| used.output_tokens >= limit)
            || (budget.usd.is_some() && used.cost_usd.is_none())
            || budget
                .usd
                .zip(used.cost_usd)
                .is_some_and(|(limit, cost)| cost >= limit)
    }

    fn charge(&self, usage: &ScopeUsage) {
        let exhausted = {
            let mut state = locked(&self.state);
            state.used.add(usage);
            self.over(&state.used)
        };
        if exhausted {
            self.refuse(ScopeError::Exhausted);
        }
        if let Some(parent) = &self.parent {
            parent.charge(usage);
        }
    }
}

struct HandleState {
    id: ScopeHandleId,
    status: watch::Sender<HandleStatus>,
    result: Mutex<Option<Result<ScopeValue, ScopeError>>>,
    usage: Mutex<Usage>,
    cancel: CancellationToken,
    member: Option<Arc<Mutex<Option<SessionId>>>>,
    rt: Arc<Runtime>,
}

/// One unit of fan-out work owned by a [`Scope`].
#[derive(Clone)]
pub struct ScopeHandle {
    state: Arc<HandleState>,
}

fn terminal(status: HandleStatus) -> bool {
    matches!(
        status,
        HandleStatus::Done | HandleStatus::Failed | HandleStatus::Cancelled
    )
}

impl ScopeHandle {
    /// The handle's identity inside its scope.
    #[must_use]
    pub fn id(&self) -> ScopeHandleId {
        self.state.id
    }

    /// The handle's current status.
    #[must_use]
    pub fn status(&self) -> ScopeStatus {
        *self.state.status.borrow()
    }

    /// Waits for the terminal state and returns the value or the error.
    #[must_use = "await it to receive the value or error"]
    pub fn result(&self) -> BoxFuture<'_, Result<ScopeValue, ScopeError>> {
        Box::pin(async move {
            let mut status = self.state.status.subscribe();
            if status.wait_for(|status| terminal(*status)).await.is_err() {
                return Err(ScopeError::Cancelled);
            }
            locked(&self.state.result)
                .clone()
                .unwrap_or(Err(ScopeError::Cancelled))
        })
    }

    /// The handle's error once it failed or was cancelled.
    #[must_use]
    pub fn error(&self) -> Option<ScopeError> {
        locked(&self.state.result)
            .as_ref()
            .and_then(|result| result.as_ref().err().cloned())
    }

    /// The normalized usage the handle spent.
    #[must_use]
    pub fn usage(&self) -> Usage {
        *locked(&self.state.usage)
    }

    /// Cancels the handle when it has not finished.
    pub fn cancel(&self) {
        self.state.cancel.cancel();
    }

    /// Sends one mailbox message to a member handle.
    #[must_use = "await it to deliver the message"]
    pub fn send(&self, text: &str, mode: MailMode) -> BoxFuture<'_, Result<Receipt, ServiceError>> {
        let target = self.state.member.as_ref().and_then(|child| *locked(child));
        let text: Box<str> = text.into();
        Box::pin(async move {
            let Some(to) = target else {
                return Err(ServiceError::Denied(DenyReason::Unavailable {
                    what: "a running member session".into(),
                }));
            };
            self.state.rt.send(to, text, mode).await
        })
    }
}

struct Book {
    all: Vec<ScopeHandle>,
    finished: VecDeque<ScopeHandle>,
    delivered: usize,
    done: usize,
    running: usize,
    waiting: VecDeque<(ScopeHandleId, oneshot::Sender<()>)>,
}

struct Shared {
    rt: Arc<Runtime>,
    ledger: Arc<Ledger>,
    on_error: OnError,
    limit: usize,
    next_id: AtomicU64,
    book: Mutex<Book>,
    progress: watch::Sender<u64>,
}

struct Inner {
    shared: Arc<Shared>,
    tasks: Mutex<JoinSet<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let handles = std::mem::take(&mut locked(&self.shared.book).all);
        for handle in handles {
            let state = &handle.state;
            let mut result = locked(&state.result);
            if result.is_some() {
                continue;
            }
            *result = Some(Err(ScopeError::Cancelled));
            state.status.send_replace(HandleStatus::Cancelled);
            drop(result);
            state.cancel.cancel();
            self.shared.progress.send_if_modified(|count| {
                *count += 1;
                true
            });
        }
        self.shared.ledger.cancel.cancel();
        self.tasks
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .detach_all();
    }
}

/// A set of concurrently running handles with a budget and an error policy.
pub struct Scope {
    inner: Inner,
}

type Work = Box<
    dyn FnOnce(
            CancellationToken,
        ) -> BoxFuture<'static, Result<(ScopeValue, Option<Usage>), ScopeError>>
        + Send,
>;

impl Scope {
    /// Opens a scope over `services`, for a battery outside any model run.
    ///
    /// Inference and member sessions run through `services` as `caller`, so
    /// every inject check, grant, and denial of those services applies.
    /// `cancel` bounds every handle. USD budgets price routes from the
    /// compiled table only; a route without a compiled price is refused with
    /// [`ScopeError::UnpricedModel`].
    ///
    /// # Errors
    /// Returns [`ScopeError::Spec`] for an invalid limit or budget.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public constructor takes ownership of its Arc and token by design"
    )]
    pub fn over(
        services: Arc<dyn Services>,
        caller: &Caller,
        cancel: CancellationToken,
        spec: ScopeSpec,
    ) -> Result<Self, ScopeError> {
        Self::open(services, caller.clone(), cancel, &spec, None)
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "parent token is consumed to derive the scope's child token"
    )]
    pub(crate) fn open(
        services: Arc<dyn Services>,
        caller: Caller,
        cancel: CancellationToken,
        spec: &ScopeSpec,
        price: Option<PriceFn>,
    ) -> Result<Self, ScopeError> {
        spec.validate(GLOBAL_MEMBER_CAP)?;
        let lineage = synthetic::lineage();
        let parent = lineage.ledger();
        let parent_cancel = parent.as_ref().map(|parent| parent.cancel.clone());
        let cancel = cancel.child_token();
        let ledger = Ledger::new(spec.budget.clone(), parent, cancel.clone());
        let rt = Arc::new(Runtime {
            services,
            caller,
            lineage,
            price,
        });
        let mut tasks = JoinSet::new();
        if let Some(parent_cancel) = parent_cancel {
            let child = Arc::clone(&ledger);
            tasks.spawn(async move {
                tokio::select! {
                    () = parent_cancel.cancelled() => child.refuse(ScopeError::Cancelled),
                    () = child.cancel.cancelled() => {}
                }
            });
        }
        if let Some(wall) = spec.budget.wall {
            let timer = Arc::clone(&ledger);
            tasks.spawn(async move {
                tokio::select! {
                    () = tokio::time::sleep(wall) => timer.refuse(ScopeError::Exhausted),
                    () = timer.cancel.cancelled() => {}
                }
            });
        }
        let (progress, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            rt,
            ledger,
            on_error: spec.on_error,
            limit: usize::from(spec.limit),
            next_id: AtomicU64::new(1),
            book: Mutex::new(Book {
                all: Vec::new(),
                finished: VecDeque::new(),
                delivered: 0,
                done: 0,
                running: 0,
                waiting: VecDeque::new(),
            }),
            progress,
        });
        Ok(Self {
            inner: Inner {
                shared,
                tasks: Mutex::new(tasks),
            },
        })
    }

    /// Starts one member session; it waits in FIFO order past `limit`.
    ///
    /// # Errors
    /// Returns the scope's refusal when it is exhausted or cancelled; a
    /// denied or failed start surfaces through the handle's result.
    pub fn agent(&self, request: AgentStart) -> Result<ScopeHandle, ScopeError> {
        let shared = &self.inner.shared;
        if shared.ledger.requires_usd() {
            let model = request
                .model
                .clone()
                .unwrap_or_else(|| "(inherited)".into());
            let priced = request
                .model
                .as_deref()
                .is_some_and(|name| shared.rt.price(&ModelRoute::from_id(name)).is_some());
            if !priced {
                return Err(ScopeError::UnpricedModel { model });
            }
        }
        shared.ledger.admit()?;
        let child = Arc::new(Mutex::new(None));
        let rt = Arc::clone(&shared.rt);
        let slot = Arc::clone(&child);
        let work: Work = Box::new(move |cancel| {
            Box::pin(async move {
                let report = rt.agent(request, slot, cancel).await?;
                Ok((ScopeValue::Agent(report), None))
            })
        });
        Ok(self.launch(work, Some(child)))
    }

    /// Starts one inference; it waits in FIFO order past `limit`.
    ///
    /// # Errors
    /// Returns [`ScopeError::UnpricedModel`] for an unpriced route under a
    /// USD budget, and the scope's refusal when it is exhausted or
    /// cancelled.
    pub fn infer(&self, request: ModelRequest) -> Result<ScopeHandle, ScopeError> {
        let shared = &self.inner.shared;
        let price = shared.rt.price(&request.model);
        if shared.ledger.requires_usd() && price.is_none() {
            return Err(ScopeError::UnpricedModel {
                model: request.model.id().into(),
            });
        }
        shared.ledger.admit()?;
        let rt = Arc::clone(&shared.rt);
        let ledger = Arc::clone(&shared.ledger);
        let work: Work = Box::new(move |cancel| {
            Box::pin(async move {
                let inference = rt.infer(request, ledger, cancel).await?;
                let usage = inference
                    .events
                    .iter()
                    .find_map(|event| match event {
                        StreamEvent::Usage(usage) => Some(*usage),
                        _ => None,
                    })
                    .map(|mut usage| {
                        usage.cost_usd = usage.cost_usd(price.as_ref(), None);
                        usage
                    });
                Ok((ScopeValue::Inference(inference), usage))
            })
        });
        Ok(self.launch(work, None))
    }

    /// Returns the next finished handle, or `None` when every handle has
    /// been returned.
    pub fn next(&self) -> BoxFuture<'_, Option<ScopeHandle>> {
        Box::pin(async move {
            let shared = &self.inner.shared;
            let mut progress = shared.progress.subscribe();
            loop {
                {
                    let mut book = locked(&shared.book);
                    if let Some(handle) = book.finished.pop_front() {
                        book.delivered += 1;
                        return Some(handle);
                    }
                    if book.delivered == book.all.len() {
                        return None;
                    }
                }
                if progress.changed().await.is_err() {
                    return None;
                }
            }
        })
    }

    /// Returns when `count` handles finished, or every handle finished.
    pub fn wait(&self, count: usize) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let shared = &self.inner.shared;
            let mut progress = shared.progress.subscribe();
            loop {
                {
                    let book = locked(&shared.book);
                    if book.done >= count.min(book.all.len()) {
                        return;
                    }
                }
                if progress.changed().await.is_err() {
                    return;
                }
            }
        })
    }

    /// Joins every handle and returns them in creation order.
    pub fn all(&self) -> BoxFuture<'_, Vec<ScopeHandle>> {
        Box::pin(async move {
            let shared = &self.inner.shared;
            let mut progress = shared.progress.subscribe();
            loop {
                {
                    let book = locked(&shared.book);
                    if book.done == book.all.len() {
                        return book.all.clone();
                    }
                }
                if progress.changed().await.is_err() {
                    return locked(&shared.book).all.clone();
                }
            }
        })
    }

    /// Cancels every unfinished handle and refuses new ones.
    pub fn cancel(&self) {
        self.inner.shared.ledger.refuse(ScopeError::Cancelled);
    }

    fn launch(&self, work: Work, member: Option<Arc<Mutex<Option<SessionId>>>>) -> ScopeHandle {
        let shared = Arc::clone(&self.inner.shared);
        let id = ScopeHandleId(shared.next_id.fetch_add(1, Ordering::Relaxed));
        let (status, _) = watch::channel(HandleStatus::Pending);
        let state = Arc::new(HandleState {
            id,
            status,
            result: Mutex::new(None),
            usage: Mutex::new(zero_usage()),
            cancel: shared.ledger.cancel.child_token(),
            member,
            rt: Arc::clone(&shared.rt),
        });
        let handle = ScopeHandle { state };
        let (gate_tx, gate_rx) = oneshot::channel();
        {
            let mut book = locked(&shared.book);
            book.all.push(handle.clone());
            if book.running < shared.limit {
                book.running += 1;
                let _ = gate_tx.send(());
            } else {
                book.waiting.push_back((id, gate_tx));
            }
        }
        let mut tasks = locked(&self.inner.tasks);
        while tasks.try_join_next().is_some() {}
        tasks.spawn(drive(shared, handle.clone(), gate_rx, work));
        handle
    }
}

fn zero_usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

async fn drive(shared: Arc<Shared>, handle: ScopeHandle, gate: oneshot::Receiver<()>, work: Work) {
    let state = &handle.state;
    let mut gate = gate;
    let granted = tokio::select! {
        granted = &mut gate => granted.is_ok(),
        () = state.cancel.cancelled() => {
            gate.close();
            gate.try_recv().is_ok()
        }
    };
    let started = if granted {
        let result = locked(&state.result);
        if result.is_some() || state.cancel.is_cancelled() {
            false
        } else {
            state.status.send_replace(HandleStatus::Running);
            true
        }
    } else {
        false
    };
    let outcome = if started {
        work(state.cancel.clone()).await
    } else {
        Err(ScopeError::Cancelled)
    };
    finish(&shared, &handle, granted, outcome);
}

fn finish(
    shared: &Shared,
    handle: &ScopeHandle,
    started: bool,
    outcome: Result<(ScopeValue, Option<Usage>), ScopeError>,
) {
    let state = &handle.state;
    let status = match &outcome {
        Ok(_) => HandleStatus::Done,
        Err(ScopeError::Cancelled | ScopeError::Infer(InferFailure::Cancelled)) => {
            HandleStatus::Cancelled
        }
        Err(_) => HandleStatus::Failed,
    };
    let mut slot = locked(&state.result);
    if slot.is_some() {
        return;
    }
    let result = match outcome {
        Ok((value, usage)) => {
            if let Some(usage) = usage {
                *locked(&state.usage) = usage;
                shared.ledger.charge(&charge_of(&usage));
            }
            Ok(value)
        }
        Err(error) => Err(error),
    };
    *slot = Some(result);
    state.status.send_replace(status);
    drop(slot);
    if status == HandleStatus::Failed && shared.on_error == OnError::Cancel {
        shared.ledger.refuse(ScopeError::Cancelled);
    }
    let mut book = locked(&shared.book);
    book.done += 1;
    book.finished.push_back(handle.clone());
    if started {
        book.running -= 1;
        pump(&mut book);
    } else {
        book.waiting.retain(|(id, _)| *id != state.id);
    }
    drop(book);
    shared.progress.send_modify(|count| *count += 1);
}

fn pump(book: &mut Book) {
    while let Some((_, gate)) = book.waiting.pop_front() {
        if gate.send(()).is_ok() {
            book.running += 1;
            return;
        }
    }
}

fn charge_of(usage: &Usage) -> ScopeUsage {
    ScopeUsage {
        requests: 1,
        input_tokens: usage.input_tokens,
        output_tokens: usage
            .output_tokens
            .saturating_add(usage.reasoning_tokens.unwrap_or(0)),
        cost_usd: usage.cost_usd,
    }
}

#[cfg(test)]
mod tests;
