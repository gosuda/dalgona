//! The caller-owned provider run: one server run kept open across host tool
//! exchanges.
//!
//! [`ProviderRun::next_step`] hands out each response as an ordinary
//! [`EventStream`]. A step that ends in `Stop` with [`StopReason::ToolUse`]
//! leaves the run awaiting the host's results; [`ProviderRun::respond`]
//! delivers them and the next step follows. Any other `Stop` closes the run.
//!
//! The run builds on the [`EventStream`] rules and does not replace them. A
//! clean `Stop` never calls the stream's cancellation action, so the run
//! learns of a step's end from its own step source, which updates the run
//! state before it yields the `Stop`. A stream dropped before its terminal, or
//! ended by an error or a cut, runs the action, which releases the run. The
//! step source is fused after its `Stop`: events the transport already
//! decoded for the next step stay queued for the next stream and never trip
//! the stream's late-event check.
//!
//! The run never invents events. A transport failure ends the step with an
//! error and no `Usage` or `Stop`.
//!
//! This module has no clock. A run awaiting host tools, including a tool
//! waiting on an approval, stays open as long as the host takes; it ends only
//! by a terminal `Stop`, a failure, a release, or a drop. At most one step
//! stream is live and at most one result batch is outstanding, so the run
//! holds no queue; the transport supplies the one batch of room
//! [`RunTransport::send`] needs.
//!
//! Maintenance while awaiting: a run that ended its step in tool use keeps
//! its connection open but does no generation. [`RunEvents::poll_maintain`]
//! performs the family's upkeep (heartbeats, solicitation answers) without
//! yielding step events or adding a run queue, and
//! [`ProviderRun::keepalive`] drives it without spawning: the owner polls the
//! [`KeepAlive`] future through its own task scope. Upkeep runs only while no
//! step stream is live, so it never polls the source concurrently with step
//! delivery; the terminal and drop transitions are unchanged.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    task::{Context, Poll, Waker},
};

use dal_core::{CallId, Family, Part};
use futures::Stream;

use crate::{
    error::{ProviderError, capped_message_line},
    stream::{EventStream, StopReason, StreamEvent, ToolCall},
};

/// Why a provider run can no longer continue.
///
/// A lost run is final: nothing starts a new run in its place, and the
/// failure is [`ProviderError::RunLost`], which the loop never replays.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunLostReason {
    /// The server ended the run before its terminal event.
    ServerClosed,
    /// The connection or its task failed; the text is the transport's own.
    Transport(String),
    /// The results offered were not exactly the pending calls.
    ResultsMismatch,
    /// The server or its decoder broke the stream grammar.
    Protocol(String),
    /// A frame or unsent-result bound was exceeded. Nothing is dropped
    /// silently.
    BufferExceeded,
    /// The server acted, or asked to act, beyond what the host authorized;
    /// the text names the tool or query.
    Authority(Box<str>),
}

impl fmt::Display for RunLostReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerClosed => f.write_str("the server ended the run before its final response"),
            Self::Transport(reason) => {
                write!(f, "transport failure: {}", capped_message_line(reason))
            }
            Self::ResultsMismatch => {
                f.write_str("the tool results are not exactly the pending tool calls")
            }
            Self::Protocol(detail) => {
                write!(f, "invalid stream: {}", capped_message_line(detail))
            }
            Self::BufferExceeded => f.write_str("a buffer bound was exceeded"),
            Self::Authority(what) => write!(
                f,
                "the server acted beyond the host's authority: {}",
                capped_message_line(what)
            ),
        }
    }
}

/// One host tool result, exactly as journaled and lowered into context.
#[derive(Clone, Debug, PartialEq)]
pub struct HostToolResult {
    /// The call this result answers, as the step announced it.
    pub call: CallId,
    /// The tool name of that call.
    pub name: Box<str>,
    /// Whether the tool returned an error.
    pub is_error: bool,
    /// The result content.
    pub parts: Vec<Part>,
}

/// Where a run stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunState {
    /// A step is in flight on the server: its stream has been or may be taken.
    Streaming,
    /// The last step ended in tool use. The ids are its calls, in response
    /// order; [`ProviderRun::respond`] must answer exactly these.
    Awaiting(Vec<CallId>),
    /// A step ended with a terminal `Stop`. The transport is released.
    Closed,
    /// The run ended without a loss: its step stream was dropped before the
    /// terminal, its owner released it, or its first step failed before any
    /// tool call reached the host. The transport is aborted and a fresh run
    /// may replace it. Server-side state of a run released after tool use is
    /// gone.
    Released,
    /// The run cannot continue. The transport is aborted.
    Lost(RunLostReason),
}

/// The connection side of a run, supplied by a family adapter next to its
/// decoded events.
///
/// Both operations are synchronous and must not block: the run calls them
/// while it holds its state, from `respond`, from stream polling, from the
/// cancellation action and from `Drop`.
pub(crate) trait RunTransport: Send + 'static {
    /// Writes the results of the step that just ended to the server, in the
    /// order given.
    ///
    /// The run calls this at most once per step, only after checking that the
    /// results answer exactly the step's calls, and not again before the server
    /// has ended another step in tool use. A transport therefore needs room
    /// for one batch. When it cannot accept one, because the connection is
    /// gone or the previous batch is still unsent, it returns the reason and
    /// the run is lost; it never drops a batch silently.
    fn send(&mut self, results: Vec<HostToolResult>) -> Result<(), RunLostReason>;

    /// Tears down the connection and stops its task, without waiting.
    ///
    /// The run calls this exactly once, after it dropped the event source,
    /// when it leaves its live states for any reason, a terminal `Stop`
    /// included: nothing remains to read from a closed run.
    fn abort(&mut self);
}

/// Duplex upkeep of one run's event source while the run waits on the host.
///
/// Awaiting does no generation: the server holds the step until the host's
/// results arrive. Sources with an open connection during that wait (Cursor:
/// heartbeats, KV answers, exec-control replies) override
/// [`RunEvents::poll_maintain`] to perform that upkeep. Upkeep never yields
/// a [`StreamEvent`] and adds no queue to the run: step events decoded along
/// the way stay buffered in the source for the next step, and a terminal
/// failure surfaces as the returned error for the keeper to fold into the
/// run. The default is no upkeep; it stays parked until the run ends.
pub(crate) trait RunEvents:
    Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
{
    /// Drives one round of upkeep while the run awaits host results.
    ///
    /// The keeper calls this only while no step stream is live, so it never
    /// runs concurrently with step delivery. `Poll::Pending` parks until the
    /// source or the next phase change wakes the keeper;
    /// `Poll::Ready(Ok(()))` reports quiescence and parks until the next
    /// phase change; `Poll::Ready(Err(_))` ends the run through the usual
    /// failure mapping.
    fn poll_maintain(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), ProviderError>> {
        Poll::Pending
    }
}

type EventSource = Pin<Box<dyn RunEvents>>;

/// One server run kept open across host tool exchanges, owned by the caller.
///
/// Dropping it aborts the transport and its task and never blocks. Dropping a
/// step stream before its terminal does the same through the stream's
/// cancellation action. A step's clean `Stop` leaves the run alive, and
/// dropping that finished stream later changes nothing.
#[must_use = "dropping a ProviderRun aborts its transport"]
pub struct ProviderRun {
    shared: Arc<Mutex<Core>>,
}

impl ProviderRun {
    /// Builds a run over the decoded events and the connection of one opened
    /// server run.
    ///
    /// `events` carries every step back to back, each following the
    /// [`StreamEvent`] grammar and ending in `Stop`, with tool names already
    /// restored to internal names. The source also carries the run's upkeep
    /// for [`ProviderRun::keepalive`] through [`RunEvents`]. The first step
    /// is in flight.
    pub(crate) fn new(
        family: Family,
        events: impl RunEvents,
        transport: impl RunTransport,
    ) -> Self {
        let live = Live {
            events: Box::pin(events),
            transport: Box::new(transport),
        };
        Self {
            shared: Arc::new(Mutex::new(Core {
                family,
                phase: Phase::Streaming { taken: false },
                live: Some(live),
                handed_off: false,
                keeper: None,
            })),
        }
    }

    /// The next response as an ordinary [`EventStream`]: deltas, one
    /// `ToolCallsDone`, one `Usage`, one `Stop`.
    ///
    /// A step that ends in `Stop` with [`StopReason::ToolUse`] leaves the run
    /// alive; any other `Stop` closes it. Only one step stream may be live,
    /// and after tool use the results must be sent first.
    ///
    /// # Errors
    /// [`ProviderError::RunLost`] when the run was lost, and
    /// [`ProviderError::InvalidRequest`] when a step stream is live, the
    /// results are unanswered, or the run is closed or released. The run
    /// keeps its state in the second case.
    pub fn next_step(&mut self) -> Result<EventStream, ProviderError> {
        {
            let mut core = lock(&self.shared);
            if !matches!(core.phase, Phase::Streaming { taken: false }) {
                return Err(core.misuse("start a step"));
            }
            core.phase = Phase::Streaming { taken: true };
        }
        let source = StepSource {
            shared: Arc::clone(&self.shared),
            step: Step::default(),
        };
        let shared = Arc::clone(&self.shared);
        Ok(EventStream::new(source, move || {
            let mut core = lock(&shared);
            if matches!(core.phase, Phase::Streaming { taken: true }) {
                core.end(Phase::Released);
            }
        }))
    }

    /// Delivers the results for exactly the calls the last step ended with.
    ///
    /// Equality is checked on the whole set, call ids and tool names, before
    /// anything is sent: a missing, extra, duplicate or wrong result loses the
    /// run and sends nothing. The results reach the transport in the order
    /// given. The next step follows with [`ProviderRun::next_step`].
    ///
    /// # Errors
    /// [`ProviderError::RunLost`] with [`RunLostReason::ResultsMismatch`] on a
    /// mismatch, or with the transport's reason when the send fails; both end
    /// the run. [`ProviderError::RunLost`] again when the run was already
    /// lost, and [`ProviderError::InvalidRequest`], with the run unchanged,
    /// when no tool calls are pending.
    pub fn respond(&mut self, results: Vec<HostToolResult>) -> Result<(), ProviderError> {
        let mut guard = lock(&self.shared);
        let core = &mut *guard;
        let Phase::Awaiting(pending) = &core.phase else {
            return Err(core.misuse("respond"));
        };
        if !answers(pending, &results) {
            return Err(core.lose(RunLostReason::ResultsMismatch));
        }
        let Some(live) = core.live.as_mut() else {
            return Err(core.misuse("respond"));
        };
        match live.transport.send(results) {
            Ok(()) => {
                core.phase = Phase::Streaming { taken: false };
                Ok(())
            }
            Err(reason) => Err(core.lose(reason)),
        }
    }

    /// The maintenance driver for this run: upkeep while it awaits host
    /// results, resolved once it ends.
    ///
    /// The provider never spawns; the owner polls the returned future
    /// through its own task scope. Dropping it changes nothing: the run
    /// keeps its state and its transport.
    pub fn keepalive(&self) -> KeepAlive {
        KeepAlive {
            shared: Arc::clone(&self.shared),
        }
    }

    /// The calls the run is waiting on, in response order, or `None` unless
    /// the last step ended in tool use. The same ids as
    /// [`RunState::Awaiting`].
    #[must_use]
    pub fn awaiting(&self) -> Option<Vec<CallId>> {
        match &lock(&self.shared).phase {
            Phase::Awaiting(pending) => Some(pending.iter().map(|call| call.id.clone()).collect()),
            _ => None,
        }
    }

    /// Where the run stands.
    #[must_use]
    pub fn state(&self) -> RunState {
        match &lock(&self.shared).phase {
            Phase::Streaming { .. } => RunState::Streaming,
            Phase::Awaiting(pending) => {
                RunState::Awaiting(pending.iter().map(|call| call.id.clone()).collect())
            }
            Phase::Closed => RunState::Closed,
            Phase::Released => RunState::Released,
            Phase::Lost(reason) => RunState::Lost(reason.clone()),
        }
    }
}

impl Drop for ProviderRun {
    fn drop(&mut self) {
        let mut core = lock(&self.shared);
        if core.live.is_some() {
            core.end(Phase::Released);
        }
    }
}

/// The owner's maintenance driver for one [`ProviderRun`].
///
/// While the run awaits host results this drives the event source's
/// [`RunEvents::poll_maintain`]; while a step stream is live it stays parked
/// and never touches the source, so step delivery and maintenance never poll
/// the same stream. It resolves once the run leaves its live states
/// (closed, released, or lost, including the owner dropping the run), and
/// dropping it aborts nothing and releases nothing.
#[must_use = "a KeepAlive performs no upkeep unless polled"]
pub struct KeepAlive {
    shared: Arc<Mutex<Core>>,
}

impl Future for KeepAlive {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut core = lock(&self.shared);
        if !matches!(core.phase, Phase::Awaiting(_)) {
            if matches!(core.phase, Phase::Closed | Phase::Released | Phase::Lost(_)) {
                return Poll::Ready(());
            }
            core.keeper = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let outcome = core
            .live
            .as_mut()
            .map(|live| live.events.as_mut().poll_maintain(cx));
        match outcome {
            None => Poll::Ready(()),
            Some(Poll::Pending) => {
                core.keeper = Some(cx.waker().clone());
                Poll::Pending
            }
            Some(Poll::Ready(Ok(()))) => {
                // Upkeep is quiescent; park until the next phase change. The
                // source registered its own wakeups before reporting
                // readiness, so nothing is missed.
                core.keeper = Some(cx.waker().clone());
                Poll::Pending
            }
            Some(Poll::Ready(Err(error))) => {
                core.fail(error);
                Poll::Ready(())
            }
        }
    }
}

/// A call the last step ended with.
struct Pending {
    id: CallId,
    name: Box<str>,
}

enum Phase {
    /// A step is in flight; `taken` once its stream was handed out.
    Streaming {
        taken: bool,
    },
    Awaiting(Vec<Pending>),
    Closed,
    Released,
    Lost(RunLostReason),
}

struct Live {
    events: EventSource,
    transport: Box<dyn RunTransport>,
}

/// The state shared by the run and its step source.
struct Core {
    family: Family,
    phase: Phase,
    /// `Some` exactly while `phase` is `Streaming` or `Awaiting`.
    live: Option<Live>,
    /// A step ended in tool use: the server now holds state a fresh run
    /// would not.
    handed_off: bool,
    /// The parked maintenance driver, woken when the run starts awaiting
    /// and when it ends.
    keeper: Option<Waker>,
}

impl Core {
    /// Leaves the live states: drops the event source, then aborts the
    /// transport, once.
    fn end(&mut self, phase: Phase) {
        self.phase = phase;
        if let Some(Live {
            events,
            mut transport,
        }) = self.live.take()
        {
            drop(events);
            transport.abort();
        }
        self.wake_keeper();
    }

    fn lose(&mut self, reason: RunLostReason) -> ProviderError {
        self.end(Phase::Lost(reason.clone()));
        ProviderError::RunLost {
            family: self.family,
            reason,
        }
    }

    /// Wakes the parked maintenance driver, if any.
    fn wake_keeper(&mut self) {
        if let Some(waker) = self.keeper.take() {
            waker.wake();
        }
    }

    /// Maps a failure of the event source.
    ///
    /// A typed run loss from the adapter is kept. Before the first tool
    /// handoff nothing server-owned reached the host, so the error passes
    /// through with its own classification and the run is released. After
    /// it, the run is lost and the error is final.
    fn fail(&mut self, error: ProviderError) -> ProviderError {
        if let ProviderError::RunLost { reason, .. } = &error {
            self.end(Phase::Lost(reason.clone()));
            return error;
        }
        if !self.handed_off {
            self.end(Phase::Released);
            return error;
        }
        let reason = match &error {
            ProviderError::StreamCut | ProviderError::WsClosed { .. } => {
                RunLostReason::ServerClosed
            }
            ProviderError::Protocol { detail, .. } => RunLostReason::Protocol(detail.clone()),
            other => RunLostReason::Transport(other.to_string()),
        };
        self.lose(reason)
    }

    /// The error for an operation the current state does not allow.
    fn misuse(&self, action: &str) -> ProviderError {
        let state = match &self.phase {
            Phase::Lost(reason) => {
                return ProviderError::RunLost {
                    family: self.family,
                    reason: reason.clone(),
                };
            }
            Phase::Streaming { .. } => "a step is in flight",
            Phase::Awaiting(_) => "its tool calls are unanswered",
            Phase::Closed => "it is closed",
            Phase::Released => "it was released",
        };
        ProviderError::InvalidRequest {
            message: format!("a provider run cannot {action}: {state}"),
        }
    }
}

fn lock(core: &Mutex<Core>) -> MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether `results` answer exactly the `pending` calls: every call once,
/// under its own name, and nothing else.
fn answers(pending: &[Pending], results: &[HostToolResult]) -> bool {
    if pending.len() != results.len() {
        return false;
    }
    let mut open: HashMap<&CallId, &str> =
        pending.iter().map(|call| (&call.id, &*call.name)).collect();
    results
        .iter()
        .all(|result| open.remove(&result.call) == Some(&*result.name))
}

/// The calls of a step, or why they cannot be answered one by one.
fn pending_of(calls: &[ToolCall]) -> Result<Vec<Pending>, &'static str> {
    let mut seen = HashSet::with_capacity(calls.len());
    if !calls.iter().all(|call| seen.insert(call.id.as_str())) {
        return Err("a step reported one call id twice");
    }
    Ok(calls
        .iter()
        .map(|call| Pending {
            id: CallId::new(call.id.as_str()),
            name: call.name.as_str().into(),
        })
        .collect())
}

/// Polls the transport's events for the live step, or `None` when the run is
/// not streaming a taken step any more.
fn poll_source(
    core: &mut Core,
    cx: &mut Context<'_>,
) -> Option<Poll<Option<Result<StreamEvent, ProviderError>>>> {
    if !matches!(core.phase, Phase::Streaming { taken: true }) {
        return None;
    }
    core.live
        .as_mut()
        .map(|live| live.events.as_mut().poll_next(cx))
}

/// The source of one step stream: the run's events up to the step's `Stop`.
struct StepSource {
    shared: Arc<Mutex<Core>>,
    step: Step,
}

impl Stream for StepSource {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut core = lock(&this.shared);
        this.step.poll(&mut core, cx)
    }
}

#[derive(Default)]
struct Step {
    /// The calls of the step's `ToolCallsDone`, once seen.
    calls: Option<Vec<Pending>>,
    /// The step ended; the source yields nothing more.
    done: bool,
}

impl Step {
    fn poll(
        &mut self,
        core: &mut Core,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<StreamEvent, ProviderError>>> {
        if self.done {
            return Poll::Ready(None);
        }
        let Some(polled) = poll_source(core, cx) else {
            return Poll::Ready(Some(Err(self.stale(core))));
        };
        let item = match polled {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(item) => item,
        };
        Poll::Ready(Some(match item {
            Some(Ok(event)) => self.deliver(core, event),
            Some(Err(error)) => Err(self.stop_with(core.fail(error))),
            None => Err(self.stop_with(core.fail(ProviderError::StreamCut))),
        }))
    }

    /// The error for a poll after the run ended under this stream.
    fn stale(&mut self, core: &Core) -> ProviderError {
        self.done = true;
        let reason = match &core.phase {
            Phase::Lost(reason) => reason.clone(),
            _ => {
                RunLostReason::Transport(String::from("the run was released before the step ended"))
            }
        };
        ProviderError::RunLost {
            family: core.family,
            reason,
        }
    }

    fn stop_with(&mut self, error: ProviderError) -> ProviderError {
        self.done = true;
        error
    }

    fn violation(&mut self, core: &mut Core, detail: &str) -> ProviderError {
        self.done = true;
        core.lose(RunLostReason::Protocol(String::from(detail)))
    }

    /// Updates the run for one event, then hands the event on unchanged. The
    /// state changes before a `Stop` is yielded, so the consumer that reads
    /// it already sees the run awaiting or closed.
    fn deliver(
        &mut self,
        core: &mut Core,
        event: StreamEvent,
    ) -> Result<StreamEvent, ProviderError> {
        match &event {
            StreamEvent::ToolCallsDone { calls } => {
                if self.calls.is_some() {
                    return Err(self.violation(core, "a step reported its tool calls twice"));
                }
                match pending_of(calls) {
                    Ok(pending) => self.calls = Some(pending),
                    Err(detail) => return Err(self.violation(core, detail)),
                }
            }
            StreamEvent::Stop { reason } => {
                self.done = true;
                if *reason == StopReason::ToolUse {
                    let calls = self.calls.take().unwrap_or_default();
                    if calls.is_empty() {
                        return Err(self.violation(core, "a tool-use stop named no tool calls"));
                    }
                    core.phase = Phase::Awaiting(calls);
                    core.handed_off = true;
                    core.wake_keeper();
                } else {
                    core.end(Phase::Closed);
                }
            }
            _ => {}
        }
        Ok(event)
    }
}

#[cfg(test)]
mod tests;
