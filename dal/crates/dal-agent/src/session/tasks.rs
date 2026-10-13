//! Session task ownership and invocation-local scope scheduling.
//!
//! [`SessionTasks`] cancels and drains session background work before shutdown;
//! [`ScopeTable`] queues and accounts script operations for its injected executor.

use std::collections::BTreeMap;
use std::num::{NonZeroU8, NonZeroU64};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dal_core::ext::{NativeOp, OpId};
use dal_core::{
    CallId, ModelPrice, ModelRequest, OnError, ScopeSpec, ScopeSpecError, ScopeUsage, StreamEvent,
};
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::ext::scope::PriceFn;
use crate::ext::{
    CancelTarget, EffectStatus, FailureCode, HostTerminal, OpFailure, OpOutcome, OpRecord,
    OpRequest, OpValue, ScopeId, Submit, TaskId,
};

#[cfg(test)]
mod tests;

/// The largest per-scope concurrency bound (E05 R10).
pub(crate) const MAX_SCOPE_CONCURRENCY: u8 = 64;
/// The largest number of live scopes per root invocation (E05 R10).
pub(crate) const MAX_LIVE_SCOPES: usize = 4;
/// The largest number of undelivered scope tasks per root (E05 R10).
pub(crate) const MAX_OUTSTANDING: u8 = 64;
/// The retained native result budget of one invocation (E05 R10).
pub(crate) const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
/// The retained native result budget of the whole host (E05 R10).
pub(crate) const MAX_HOST_RESULT_BYTES: usize = 16 * 1024 * 1024;
/// Owns background work for one live session.
#[must_use = "session background tasks must stop before the journal closes"]
#[derive(Clone)]
pub(crate) struct SessionTasks {
    tracker: TaskTracker,
    stop: CancellationToken,
}

impl SessionTasks {
    /// Creates the session task owner.
    pub(crate) fn new() -> Self {
        Self {
            tracker: TaskTracker::new(),
            stop: CancellationToken::new(),
        }
    }

    /// Returns the number of session-owned tasks still tracked.
    pub(crate) fn tracked(&self) -> usize {
        self.tracker.len()
    }

    /// Runs `work` only while the owning session remains open.
    pub(crate) fn spawn<F>(&self, work: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let stop = self.stop.clone();
        drop(self.tracker.spawn(async move {
            let _ = stop.run_until_cancelled_owned(work).await;
        }));
    }

    /// Spawns cleanup work that the owner drains before session shutdown.
    pub(crate) fn spawn_cleanup<F>(&self, handle: &tokio::runtime::Handle, work: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        drop(self.tracker.spawn_on(work, handle));
    }

    /// Cancels and waits for every task before session shutdown continues.
    pub(crate) async fn stop(&self) {
        self.stop.cancel();
        self.tracker.close();
        self.tracker.wait().await;
    }
}

/// Why a scope operation was refused (E05).
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ScopeError {
    /// The handle does not belong to this invocation.
    #[error("unknown scope")]
    Unknown(ScopeId),
    /// The scope no longer admits submissions.
    #[error("scope {0:?} is closed to new submissions")]
    NotOpen(ScopeId),
}

/// The lifecycle state of one invocation-owned scope (E05).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeState {
    /// Admits submissions.
    Open,
    /// Collecting; submissions are refused.
    Sealed,
    /// Cancelled and draining recorded outcomes.
    Closing,
    /// Terminal; recorded outcomes remain projectable.
    Closed,
}

/// One lifecycle transition of a scope (E05).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeEvent {
    /// A task submission.
    Submit,
    /// A collection seal.
    Seal,
    /// An owner cancellation.
    Cancel,
    /// Every recorded outcome has been delivered.
    Drained,
}

/// The queue state of one task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskState {
    /// Submitted, not yet issued to the executor.
    Queued,
    /// Handed to the executor.
    Issued,
    /// Terminally recorded.
    Done,
}

/// One scheduled scope task.
struct TaskRecord {
    /// The owning scope.
    scope: ScopeId,
    /// The core call identity minted at submission.
    call: CallId,
    /// The requested operation.
    op: OpId,
    /// The request, taken once when the executor is issued the task.
    req: Option<OpRequest>,
    /// The observation cutoff frozen at submission (T-E05).
    cutoff: Option<u64>,
    /// The queue state.
    state: TaskState,
    /// The recorded outcome, present once settled.
    outcome: Option<OpOutcome>,
    /// Whether the outcome was delivered at least once.
    delivered: bool,
    /// The task's cancellation signal.
    cancel: CancellationToken,
    /// The resolved model price, when this task is a model request.
    price: Option<ModelPrice>,
}

/// One invocation-owned scope.
struct ScopeEntry {
    /// This scope's handle.
    id: ScopeId,
    /// The lifecycle state.
    state: ScopeState,
    /// The active-operation bound (E05).
    limit: NonZeroU8,
    /// This scope's failure policy and resource budgets.
    spec: ScopeSpec,
    /// The scope cancellation signal.
    cancel: CancellationToken,
    /// The wall deadline from scope creation.
    deadline: Option<Instant>,
    /// The number of admitted handles, including pending work.
    admitted: u64,
    /// Normalized usage from completed model requests.
    usage: ScopeUsage,
    /// The first budget or cancellation refusal.
    refusal: Option<ScopeSpecError>,
    /// The tasks issued and not yet finished.
    active: u8,
    /// The member tasks in submission order (T-S01).
    tasks: Vec<TaskId>,
    /// The retained result bytes of this scope (T-S04).
    bytes: usize,
}

impl ScopeEntry {
    /// Applies one lifecycle event (T-S02).
    fn transition(&mut self, ev: ScopeEvent) -> Result<(), ScopeError> {
        match ev {
            ScopeEvent::Submit => {
                if self.state == ScopeState::Open {
                    Ok(())
                } else {
                    Err(ScopeError::NotOpen(self.id))
                }
            }
            ScopeEvent::Seal => {
                if self.state == ScopeState::Open {
                    self.state = ScopeState::Sealed;
                }
                Ok(())
            }
            ScopeEvent::Cancel => {
                if matches!(self.state, ScopeState::Open | ScopeState::Sealed) {
                    self.state = ScopeState::Closing;
                }
                Ok(())
            }
            ScopeEvent::Drained => match self.state {
                ScopeState::Sealed | ScopeState::Closing => {
                    self.state = ScopeState::Closed;
                    Ok(())
                }
                ScopeState::Open => Err(ScopeError::NotOpen(self.id)),
                ScopeState::Closed => Ok(()),
            },
        }
    }

    /// Zeroes and returns this scope's retained result bytes.
    fn take_bytes(&mut self) -> usize {
        let released = self.bytes;
        self.bytes = 0;
        released
    }
}

/// Mints the next nonzero handle value, refusing overflow as a host limit.
fn mint_raw(next: &mut u64, what: &'static str) -> Result<NonZeroU64, HostTerminal> {
    let raw = *next;
    let bumped = raw
        .checked_add(1)
        .ok_or(HostTerminal::LimitExceeded { what })?;
    *next = bumped;
    NonZeroU64::new(raw).ok_or(HostTerminal::LimitExceeded { what })
}

/// The scopes and tasks of one root invocation (E05).
///
/// Queued and completed-undelivered tasks count against [`MAX_OUTSTANDING`];
/// retained results count against [`MAX_RESULT_BYTES`] per invocation and
/// [`MAX_HOST_RESULT_BYTES`] across the host (T-S04).
pub(crate) struct ScopeTable {
    scopes: BTreeMap<ScopeId, ScopeEntry>,
    tasks: BTreeMap<TaskId, TaskRecord>,
    outstanding: u8,
    bytes: usize,
    host_bytes: Arc<AtomicUsize>,
    price: PriceFn,
    next_scope: u64,
    next_task: u64,
}

impl ScopeTable {
    /// Creates the table of one root invocation.
    ///
    /// `host_bytes` is the host-wide retained-result counter shared by every
    /// invocation; the session script host owns it and injects it here (E05).
    pub(crate) fn new(host_bytes: Arc<AtomicUsize>, price: PriceFn) -> Self {
        Self {
            scopes: BTreeMap::new(),
            tasks: BTreeMap::new(),
            outstanding: 0,
            bytes: 0,
            host_bytes,
            price,
            next_scope: 1,
            next_task: 1,
        }
    }

    /// Opens one scope after validating its concurrency and resource bounds (E05).
    ///
    /// The root permits at most [`MAX_LIVE_SCOPES`] live scopes; closed
    /// scopes remain projectable and do not count against the bound.
    pub(crate) fn open(
        &mut self,
        spec: ScopeSpec,
        cancel: CancellationToken,
    ) -> Result<ScopeId, HostTerminal> {
        spec.validate(u16::from(MAX_SCOPE_CONCURRENCY))
            .map_err(HostTerminal::InvalidScope)?;
        let limit = u8::try_from(spec.limit)
            .ok()
            .and_then(NonZeroU8::new)
            .ok_or(HostTerminal::InvalidScope(ScopeSpecError::InvalidLimit {
                limit: spec.limit,
                cap: u16::from(MAX_SCOPE_CONCURRENCY),
            }))?;
        let live = self
            .scopes
            .values()
            .filter(|entry| entry.state != ScopeState::Closed)
            .count();
        if live >= MAX_LIVE_SCOPES {
            return Err(HostTerminal::LimitExceeded {
                what: "live_scopes",
            });
        }
        let deadline = spec.budget.wall.map(|wall| Instant::now() + wall);
        let raw = mint_raw(&mut self.next_scope, "scope_ids")?;
        let id = ScopeId::new(raw);
        self.scopes.insert(
            id,
            ScopeEntry {
                id,
                state: ScopeState::Open,
                limit,
                spec,
                cancel,
                deadline,
                admitted: 0,
                usage: ScopeUsage::default(),
                refusal: None,
                active: 0,
                tasks: Vec::new(),
                bytes: 0,
            },
        );
        Ok(id)
    }

    /// Enqueues one task in submission order (E05 T-S02 T-E05).
    ///
    /// Budget refusals do not mint a task. The cutoff freezes at submission.
    pub(crate) fn submit(
        &mut self,
        s: ScopeId,
        req: OpRequest,
        cutoff: Option<u64>,
    ) -> Result<Submit, ScopeError> {
        self.expire();
        let entry = self.scopes.get(&s).ok_or(ScopeError::Unknown(s))?;
        if let Some(reason) = &entry.refusal {
            return Ok(Submit::Refused(reason.clone()));
        }
        if entry
            .spec
            .budget
            .requests
            .is_some_and(|limit| entry.admitted >= limit)
        {
            return Ok(Submit::Refused(ScopeSpecError::Exhausted));
        }
        let model_request = model_request(&req);
        let price = model_request
            .as_ref()
            .and_then(|request| (self.price)(&request.model));
        if entry.spec.budget.usd.is_some()
            && is_model_op(&req.op)
            && let Some(request) = &model_request
            && price.is_none()
        {
            return Ok(Submit::Refused(ScopeSpecError::UnpricedModel {
                model: request.model.id().into(),
            }));
        }
        let entry = self.scopes.get_mut(&s).ok_or(ScopeError::Unknown(s))?;
        entry.transition(ScopeEvent::Submit)?;
        if self.outstanding >= MAX_OUTSTANDING {
            return Ok(Submit::Terminal(HostTerminal::LimitExceeded {
                what: "outstanding",
            }));
        }
        let raw = match mint_raw(&mut self.next_task, "task_ids") {
            Ok(raw) => raw,
            Err(terminal) => return Ok(Submit::Terminal(terminal)),
        };
        let id = TaskId::new(raw);
        let op = req.op.clone();
        let record = TaskRecord {
            scope: s,
            call: CallId::new(format!("scope-{raw}")),
            op,
            req: Some(req),
            cutoff,
            state: TaskState::Queued,
            outcome: None,
            delivered: false,
            cancel: entry.cancel.child_token(),
            price,
        };
        entry.admitted = entry.admitted.saturating_add(1);
        entry.tasks.push(id);
        self.tasks.insert(id, record);
        self.outstanding += 1;
        Ok(Submit::Queued(id))
    }

    /// Issues queued tasks to the executor, honouring each scope's bound.
    ///
    /// Tasks issue in submission order; a scope at its limit keeps its queue
    /// without blocking other scopes (E05).
    #[must_use]
    pub(crate) fn runnable(&mut self) -> Vec<(TaskId, OpRequest, Option<u64>, CancellationToken)> {
        self.expire();
        let queued: Vec<TaskId> = self
            .tasks
            .iter()
            .filter(|(_, record)| record.state == TaskState::Queued)
            .map(|(id, _)| *id)
            .collect();
        let mut ready = Vec::with_capacity(queued.len());
        for id in queued {
            let Some(record) = self.tasks.get(&id) else {
                continue;
            };
            let scope = record.scope;
            let Some(entry) = self.scopes.get(&scope) else {
                continue;
            };
            if entry.active >= entry.limit.get() {
                continue;
            }
            let Some(record) = self.tasks.get_mut(&id) else {
                continue;
            };
            let Some(req) = record.req.take() else {
                continue;
            };
            record.state = TaskState::Issued;
            if let Some(entry) = self.scopes.get_mut(&scope) {
                entry.active += 1;
            }
            ready.push((id, req, record.cutoff, record.cancel.clone()));
        }
        ready
    }

    /// Records one executor outcome (E05 T-S03 T-S04).
    ///
    /// Retaining the result is refused with [`HostTerminal::LimitExceeded`]
    /// before the counters move when the invocation's 4 MiB or the host's
    /// 16 MiB budget would overflow; the host then falls back to the core
    /// blob mechanism and re-reports the reference-sized result. Unknown or
    /// already-settled tasks absorb the late arrival, so a cancelled task's
    /// terminal record is never replaced.
    pub(crate) fn complete(
        &mut self,
        t: TaskId,
        o: OpOutcome,
        bytes: usize,
    ) -> Result<(), HostTerminal> {
        let Some(record) = self.tasks.get(&t) else {
            return Ok(());
        };
        if record.state == TaskState::Done {
            return Ok(());
        }
        let over_invocation = self.bytes.saturating_add(bytes) > MAX_RESULT_BYTES;
        let over_host =
            self.host_bytes.load(Ordering::SeqCst).saturating_add(bytes) > MAX_HOST_RESULT_BYTES;
        if over_invocation || over_host {
            return Err(HostTerminal::LimitExceeded {
                what: "result_bytes",
            });
        }
        let scope = record.scope;
        let op = record.op.clone();
        let price = record.price;
        let was_issued = record.state == TaskState::Issued;
        let charge = scope_charge(&op, &o, price);
        let failed = !matches!(&o, OpOutcome::Ok { .. });
        if let Some(record) = self.tasks.get_mut(&t) {
            record.state = TaskState::Done;
            record.outcome = Some(o);
        }
        if let Some(entry) = self.scopes.get_mut(&scope) {
            entry.bytes += bytes;
            if was_issued {
                entry.active -= 1;
            }
            if let Some(charge) = charge {
                entry.usage.add(&charge);
            }
        }
        self.bytes += bytes;
        self.host_bytes.fetch_add(bytes, Ordering::SeqCst);
        let reason = self.scopes.get(&scope).and_then(|entry| {
            if usage_exhausted(&entry.spec, &entry.usage) {
                Some(ScopeSpecError::Exhausted)
            } else if failed && entry.spec.on_error == OnError::Cancel {
                Some(ScopeSpecError::Cancelled)
            } else {
                None
            }
        });
        if let Some(reason) = reason {
            self.stop_scope(scope, reason);
        }
        Ok(())
    }

    /// Delivers one recorded outcome (E05 T-E10).
    ///
    /// Idempotent: the first delivery credits the outstanding count and the
    /// scope's bound exactly once; repeated deliveries re-serve the same
    /// recorded outcome without re-execution. The last delivery of a sealed
    /// scope closes it and releases its retained bytes.
    pub(crate) fn deliver(&mut self, t: TaskId) -> Option<OpOutcome> {
        let record = self.tasks.get(&t)?;
        let outcome = record.outcome.as_ref()?.clone();
        if record.delivered {
            return Some(outcome);
        }
        let scope = record.scope;
        let Some(record) = self.tasks.get_mut(&t) else {
            return Some(outcome);
        };
        record.delivered = true;
        self.outstanding -= 1;
        self.close_if_drained(scope);
        Some(outcome)
    }

    /// Seals one scope and returns its members in submission order (T-S01).
    ///
    /// Sealing is idempotent; an empty scope closes immediately (E05).
    pub(crate) fn seal(&mut self, s: ScopeId) -> Result<Box<[TaskId]>, ScopeError> {
        let entry = self.scopes.get_mut(&s).ok_or(ScopeError::Unknown(s))?;
        entry.transition(ScopeEvent::Seal)?;
        if entry.tasks.is_empty() {
            let _ = entry.transition(ScopeEvent::Drained);
            return Ok(Box::new([]));
        }
        Ok(Box::from(entry.tasks.as_slice()))
    }

    /// Cancels one task or a whole scope as the owner (E05 T-S03).
    ///
    /// Cancelled work settles as a recoverable [`FailureCode::Cancelled`]
    /// result; the returned ids are the tasks newly stopped. Repeated
    /// cancels are absorbed.
    #[must_use]
    pub(crate) fn cancel(&mut self, target: CancelTarget) -> Vec<TaskId> {
        match target {
            CancelTarget::Task(t) => self.stop_owned(t).into_iter().collect(),
            CancelTarget::Scope(s) => self.stop_scope(s, ScopeSpecError::Cancelled),
        }
    }

    /// Cancels every unfinished task as the root (E05 T-S03).
    ///
    /// External root cancellation stays terminal: stopped tasks record
    /// [`OpOutcome::Terminal`] and never expose a recoverable result.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn cancel_root(&mut self) -> Vec<TaskId> {
        for entry in self.scopes.values_mut() {
            entry.cancel.cancel();
            let _ = entry.transition(ScopeEvent::Cancel);
        }
        let all: Vec<TaskId> = self.tasks.keys().copied().collect();
        all.into_iter().filter_map(|t| self.stop_root(t)).collect()
    }

    /// Settles one unfinished task as owner-cancelled and recoverable.
    fn stop_owned(&mut self, t: TaskId) -> Option<TaskId> {
        let record = self.tasks.get(&t)?;
        let outcome = OpOutcome::Failed {
            failure: OpFailure {
                code: FailureCode::Cancelled,
                message: Box::from("cancelled by the owner"),
                details: None,
            },
            record: OpRecord {
                call: record.call.clone(),
                op: record.op.clone(),
                status: EffectStatus::Cancelled,
            },
        };
        self.settle_stopped(t, outcome)
    }

    /// Settles one unfinished task with the terminal root-cancellation outcome.
    #[cfg(test)]
    fn stop_root(&mut self, t: TaskId) -> Option<TaskId> {
        self.settle_stopped(t, OpOutcome::Terminal(HostTerminal::Cancelled))
    }

    /// Records one stop outcome and frees the scope's active slot.
    fn settle_stopped(&mut self, t: TaskId, outcome: OpOutcome) -> Option<TaskId> {
        let record = self.tasks.get_mut(&t)?;
        if record.state == TaskState::Done {
            return None;
        }
        let was_issued = record.state == TaskState::Issued;
        let scope = record.scope;
        record.cancel.cancel();
        record.state = TaskState::Done;
        record.outcome = Some(outcome);
        if was_issued && let Some(entry) = self.scopes.get_mut(&scope) {
            entry.active -= 1;
        }
        Some(t)
    }

    fn stop_scope(&mut self, s: ScopeId, reason: ScopeSpecError) -> Vec<TaskId> {
        let Some(entry) = self.scopes.get_mut(&s) else {
            return Vec::new();
        };
        if entry.state == ScopeState::Closed {
            return Vec::new();
        }
        if entry.refusal.is_none() {
            entry.refusal = Some(reason);
            entry.deadline = None;
        }
        entry.cancel.cancel();
        let _ = entry.transition(ScopeEvent::Cancel);
        let members = entry.tasks.clone();
        let stopped = members
            .into_iter()
            .filter_map(|task| self.stop_owned(task))
            .collect();
        self.close_if_drained(s);
        stopped
    }

    fn close_if_drained(&mut self, s: ScopeId) {
        let drained = self.scopes.get(&s).is_some_and(|entry| {
            matches!(entry.state, ScopeState::Sealed | ScopeState::Closing)
                && entry
                    .tasks
                    .iter()
                    .all(|id| self.tasks.get(id).is_some_and(|record| record.delivered))
        });
        if !drained {
            return;
        }
        let Some(entry) = self.scopes.get_mut(&s) else {
            return;
        };
        let _ = entry.transition(ScopeEvent::Drained);
        let released = entry.take_bytes();
        self.bytes -= released;
        self.host_bytes.fetch_sub(released, Ordering::SeqCst);
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.scopes
            .values()
            .filter(|entry| entry.state != ScopeState::Closed && entry.refusal.is_none())
            .filter_map(|entry| entry.deadline)
            .min()
    }

    pub(crate) fn expire(&mut self) {
        let now = Instant::now();
        let expired: Vec<ScopeId> = self
            .scopes
            .values()
            .filter(|entry| {
                entry.state != ScopeState::Closed
                    && entry.deadline.is_some_and(|deadline| deadline <= now)
            })
            .map(|entry| entry.id)
            .collect();
        for scope in expired {
            self.stop_scope(scope, ScopeSpecError::Exhausted);
        }
    }

    /// Reports bounded cleanup (E05 T-S08).
    ///
    /// Returns whether every owned operation settled and the call ids still
    /// unfinished at the cleanup bound, in submission order. Every scope
    /// closes and releases its retained bytes; the table is dropped after.
    pub(crate) fn cleanup(&mut self) -> (bool, Box<[CallId]>) {
        let unfinished: Box<[CallId]> = self
            .tasks
            .values()
            .filter(|record| record.state != TaskState::Done)
            .map(|record| record.call.clone())
            .collect();
        for entry in self.scopes.values_mut() {
            entry.cancel.cancel();
            if entry.state != ScopeState::Closed {
                let released = entry.take_bytes();
                self.bytes -= released;
                self.host_bytes.fetch_sub(released, Ordering::SeqCst);
                entry.state = ScopeState::Closed;
            }
        }
        (unfinished.is_empty(), unfinished)
    }

    /// Returns the undelivered task count of this invocation (E05).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn outstanding(&self) -> u8 {
        self.outstanding
    }

    /// Reports whether one task has a recorded outcome (E05).
    #[must_use]
    pub(crate) fn settled(&self, t: TaskId) -> bool {
        self.tasks
            .get(&t)
            .is_some_and(|record| record.outcome.is_some())
    }

    /// Returns the call identity minted for one task at submission (T-S08).
    #[must_use]
    pub(crate) fn call_id(&self, t: TaskId) -> Option<CallId> {
        self.tasks.get(&t).map(|record| record.call.clone())
    }
}

fn is_model_op(op: &OpId) -> bool {
    matches!(
        op,
        OpId::Native(NativeOp::ModelsInfer | NativeOp::ModelsForward)
    )
}

fn model_request(req: &OpRequest) -> Option<ModelRequest> {
    is_model_op(&req.op)
        .then(|| req.args.decode_as::<ModelRequest>().ok())
        .flatten()
}

fn scope_charge(op: &OpId, outcome: &OpOutcome, price: Option<ModelPrice>) -> Option<ScopeUsage> {
    if !is_model_op(op) {
        return None;
    }
    let OpOutcome::Ok {
        value: OpValue::Json(value),
        ..
    } = outcome
    else {
        return None;
    };
    let Ok(inference) = value.decode_as::<dal_core::Inference>() else {
        return None;
    };
    inference
        .events
        .iter()
        .find_map(|event| match event {
            StreamEvent::Usage(usage) => Some(*usage),
            _ => None,
        })
        .map(|mut usage| {
            usage.cost_usd = usage.cost_usd(price.as_ref(), None);
            ScopeUsage {
                requests: 1,
                input_tokens: usage.input_tokens,
                output_tokens: usage
                    .output_tokens
                    .saturating_add(usage.reasoning_tokens.unwrap_or(0)),
                cost_usd: usage.cost_usd,
            }
        })
}

fn usage_exhausted(spec: &ScopeSpec, usage: &ScopeUsage) -> bool {
    let budget = &spec.budget;
    budget
        .input_tokens
        .is_some_and(|limit| usage.input_tokens >= limit)
        || budget
            .output_tokens
            .is_some_and(|limit| usage.output_tokens >= limit)
        || (budget.usd.is_some() && usage.cost_usd.is_none())
        || budget
            .usd
            .zip(usage.cost_usd)
            .is_some_and(|(limit, cost)| cost >= limit)
}
