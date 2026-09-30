//! Observer queues and observe-only dispatch.
//!
//! The loop enqueues event payloads without waiting; an observer task
//! drains the queues and drives the typed dispatch fns below. Lossy queues
//! shed the oldest event under flood and count the drops; lossless queues
//! apply cancellable backpressure. Observer failures are counted in
//! [`ObserverReport`], never raised.

use std::sync::Arc;

use dal_core::ext::TurnEnd;
use dal_core::{SessionEnd, SessionStart, Settled, ToolResultEvent};
use tokio_util::sync::CancellationToken;

use crate::ext::ObserveHook;

use super::{DispatchCx, hook_block_message, settle};
use super::{LOSSLESS_OBSERVER_QUEUE_CAPACITY, OBSERVER_QUEUE_CAPACITY};

/// Counts and retains the latest observer failure; the actor logs the text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ObserverReport {
    /// How many observer invocations have failed so far.
    pub failed: u64,
    /// The latest failure text, if any observer has failed.
    pub last_error: Option<Box<str>>,
}

impl ObserverReport {
    /// Counts one failure of `ext` without ever failing the turn.
    pub fn note(&mut self, ext: &str, err: &crate::ext::HookError) {
        self.failed = self.failed.saturating_add(1);
        self.last_error = Some(hook_block_message(ext, err));
    }
}

/// Caller-owned lossy observer queue: bounded, drop-oldest, counted.
#[derive(Clone, Debug, Default)]
pub struct LossyQueue<T> {
    buf: std::collections::VecDeque<T>,
    dropped: u64,
}

impl<T> LossyQueue<T> {
    /// An empty queue with capacity [`super::OBSERVER_QUEUE_CAPACITY`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: std::collections::VecDeque::new(),
            dropped: 0,
        }
    }

    /// Enqueues `item`, shedding the oldest queued event when full.
    pub fn push(&mut self, item: T) {
        if self.buf.len() >= OBSERVER_QUEUE_CAPACITY {
            self.buf.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.buf.push_back(item);
    }

    /// Removes and returns the oldest queued event, if any.
    pub fn pop(&mut self) -> Option<T> {
        self.buf.pop_front()
    }

    /// The number of queued events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the queue holds no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// How many events flood pressure has shed so far.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Caller-owned lossless observer queue: bounded, cancellable backpressure.
/// The interior lock is never held across an await, so a waiter never
/// blocks the popper; a poisoned lock recovers its inner queue.
#[derive(Debug, Default)]
pub struct LosslessQueue<T> {
    state: std::sync::Mutex<std::collections::VecDeque<T>>,
    space: tokio::sync::Notify,
}

impl<T> LosslessQueue<T> {
    /// An empty queue with capacity [`super::LOSSLESS_OBSERVER_QUEUE_CAPACITY`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(std::collections::VecDeque::new()),
            space: tokio::sync::Notify::new(),
        }
    }

    /// Enqueues `item`, waiting for space when full. Returns `false` when
    /// `cancel` fires first; the item is then refused, never stored.
    pub async fn push(&self, item: T, cancel: &CancellationToken) -> bool {
        loop {
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.len() < LOSSLESS_OBSERVER_QUEUE_CAPACITY {
                    state.push_back(item);
                    return true;
                }
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return false,
                () = self.space.notified() => {}
            }
        }
    }

    /// Removes and returns the oldest queued event, waking one waiter.
    pub fn pop(&self) -> Option<T> {
        let item = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();
        if item.is_some() {
            self.space.notify_one();
        }
        item
    }

    /// The number of queued events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether the queue holds no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Drives one extension's observer hooks without ever failing the turn.
/// Each failure is counted in `report`; the actor logs the text.
async fn drive_observe<I: Clone + 'static>(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn ObserveHook<I>>],
    event: &I,
    report: &mut ObserverReport,
) {
    let deadline = cx.deadline();
    for hook in hooks {
        if let Err(err) = settle(
            hook.call(event.clone(), cx.mint(deadline)),
            cx.cancel,
            deadline,
        )
        .await
        {
            report.note(ext, &err);
        }
    }
}

/// Observes session start; failures are counted, never raised.
pub async fn dispatch_session_start(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn ObserveHook<SessionStart>>],
    event: &SessionStart,
    report: &mut ObserverReport,
) {
    drive_observe(ext, cx, hooks, event, report).await;
}

/// Observes session end; failures are counted, never raised.
pub async fn dispatch_session_end(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn ObserveHook<SessionEnd>>],
    event: &SessionEnd,
    report: &mut ObserverReport,
) {
    drive_observe(ext, cx, hooks, event, report).await;
}

/// Observes a tool result; failures are counted, never raised.
pub async fn dispatch_tool_result(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn ObserveHook<ToolResultEvent>>],
    event: &ToolResultEvent,
    report: &mut ObserverReport,
) {
    drive_observe(ext, cx, hooks, event, report).await;
}

/// Observes turn end; failures are counted, never raised.
pub async fn dispatch_turn_end(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn ObserveHook<TurnEnd>>],
    event: &TurnEnd,
    report: &mut ObserverReport,
) {
    drive_observe(ext, cx, hooks, event, report).await;
}

/// Observes turn settlement; failures are counted, never raised.
pub async fn dispatch_settled(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn ObserveHook<Settled>>],
    event: &Settled,
    report: &mut ObserverReport,
) {
    drive_observe(ext, cx, hooks, event, report).await;
}
