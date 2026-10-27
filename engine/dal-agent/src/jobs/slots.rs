//! The session process-slot ledger: the per-session child cap and the FIFO of
//! calls waiting for a slot.
//!
//! One ledger lives in each session's [`JobTable`]. Both spawn doors, the
//! `ToolCx::authorize` ladder and the services `run` ladder, reach it through
//! `Admission::admit_process`, which takes a slot here before it asks the host
//! for its 256-process permit, so a queued call holds no host capacity. The
//! wait is one hop of that admission operation and shares its deadline and
//! cancellation. A [`SlotGuard`] rides in the child's `ProcessAdmission` and
//! then its `ProcessLease` from admission until the child is reaped, detached
//! children included. Coordinator-owned job rows back no process and never
//! hold a guard.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dal_core::CallId;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::JobTable;
use crate::admission::Deadline;
use crate::error::{AdmissionLimit, ToolError};
use crate::proc::ProgressPort;

#[derive(Debug, Default)]
struct Ledger {
    /// Children currently holding a slot.
    live: usize,
    /// Waiting tickets in arrival order.
    waiting: VecDeque<u64>,
    /// The next ticket to hand out.
    next: u64,
}

/// Where a call stands when it first looks at the ledger.
enum Place {
    /// A slot was free and nobody waited: the slot is already counted.
    Free,
    /// The waiting queue is at its bound: the call is shed.
    Full,
    /// The call joined the queue under this ticket.
    Waiting(u64),
}

/// What a waiting call finds each time it looks at the ledger.
enum Turn {
    /// The call was the head and a slot was free: the slot is already counted.
    Claimed,
    /// The call still waits, with this many calls in the queue, itself included.
    Waiting(usize),
}

/// One session's cap on live children and the bounded FIFO of calls waiting
/// for a slot.
#[derive(Debug)]
pub(crate) struct ProcessSlots {
    limit: usize,
    /// The most calls that may wait; a call past it is shed.
    queue_limit: usize,
    ledger: Mutex<Ledger>,
    /// Bumped on every join, leave, admission, and release.
    changed: watch::Sender<u64>,
}

impl ProcessSlots {
    /// Builds a ledger that admits at most `limit` live children and holds at
    /// most `queue_limit` waiting calls.
    pub(crate) fn new(limit: usize, queue_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            queue_limit,
            ledger: Mutex::new(Ledger::default()),
            changed: watch::channel(0).0,
        })
    }

    /// Returns the session's ledger without waiting out a table lock that
    /// another task holds across ledger IO: `cancel` abandons the wait and
    /// `deadline` bounds it.
    ///
    /// # Errors
    /// Returns [`ToolError::Cancelled`] when `cancel` fires first, or
    /// [`ToolError::Admission`] when `deadline` passes first.
    async fn of(
        table: &tokio::sync::Mutex<JobTable>,
        cancel: &CancellationToken,
        deadline: Deadline,
    ) -> Result<Arc<Self>, ToolError> {
        deadline
            .wait(AdmissionLimit::SessionProcesses, cancel, async {
                let table = table.lock().await;
                Ok::<_, ToolError>(table.process_slots())
            })
            .await
    }

    /// Takes a slot from the session's ledger: looks the ledger up, then
    /// queues for a slot, both against `deadline` and `cancel`.
    ///
    /// # Errors
    /// The errors of [`Self::acquire`], plus the lookup's.
    pub(crate) async fn reserve(
        table: &tokio::sync::Mutex<JobTable>,
        call: &CallId,
        port: &dyn ProgressPort,
        cancel: &CancellationToken,
        deadline: Deadline,
    ) -> Result<SlotGuard, ToolError> {
        let slots = Self::of(table, cancel, deadline).await?;
        slots.acquire(call, port, cancel, deadline).await
    }

    /// Counts one slot as live and returns its guard, as a claim would.
    #[cfg(test)]
    pub(crate) fn claim_for_test(self: &Arc<Self>) -> SlotGuard {
        self.ledger().live += 1;
        SlotGuard {
            slots: Arc::clone(self),
        }
    }

    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    /// The number of calls waiting for a slot.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.ledger().waiting.len()
    }

    /// The number of live children holding a slot.
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.ledger().live
    }

    /// Takes a free slot when nobody waits, joins the queue otherwise, and
    /// sheds the call when the queue is at its bound.
    fn enter(&self) -> Place {
        let mut ledger = self.ledger();
        if ledger.waiting.is_empty() && ledger.live < self.limit {
            ledger.live += 1;
            return Place::Free;
        }
        if ledger.waiting.len() >= self.queue_limit {
            return Place::Full;
        }
        let ticket = ledger.next;
        ledger.next += 1;
        ledger.waiting.push_back(ticket);
        Place::Waiting(ticket)
    }

    /// Claims the slot for `ticket` when it heads the queue and a slot is
    /// free; otherwise reports how many calls wait.
    fn turn(&self, ticket: u64) -> Turn {
        let mut ledger = self.ledger();
        if ledger.waiting.front() == Some(&ticket) && ledger.live < self.limit {
            ledger.waiting.pop_front();
            ledger.live += 1;
            return Turn::Claimed;
        }
        Turn::Waiting(ledger.waiting.len())
    }

    /// Wraps a slot just counted into `live`, then re-checks `cancel`.
    ///
    /// A cancellation can fire between the check before the claim and the
    /// claim itself. Dropping the guard here frees the slot and wakes the
    /// next waiter, so a claim never outlives a cancellation observed at
    /// the claim point.
    fn claimed(self: &Arc<Self>, cancel: &CancellationToken) -> Result<SlotGuard, ToolError> {
        let guard = SlotGuard {
            slots: Arc::clone(self),
        };
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        Ok(guard)
    }

    /// Waits in FIFO order for a free slot, publishing the live queue length
    /// for `call` while it waits.
    ///
    /// No row is published when a slot is free at once. A call that waited
    /// replaces its waiting row with an empty one when it takes a slot, so a
    /// child that prints nothing never keeps looking queued. Cancellation,
    /// the deadline, and dropping the future all leave the queue; none ever
    /// claims a slot, and a cancelled call publishes nothing more.
    ///
    /// # Errors
    /// Returns [`ToolError::Cancelled`] when `cancel` fires first,
    /// [`ToolError::Admission`] naming [`AdmissionLimit::SessionProcesses`]
    /// when `deadline` passes while the call waits, or
    /// [`ToolError::AdmissionQueueFull`] when the waiting queue is already
    /// full. A call that fails any way leaves the queue and holds no slot.
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        call: &CallId,
        port: &dyn ProgressPort,
        cancel: &CancellationToken,
        deadline: Deadline,
    ) -> Result<SlotGuard, ToolError> {
        // Subscribe before the first look so a change between the look and
        // the await is never missed.
        let mut changes = self.changed.subscribe();
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if deadline.is_past() {
            return Err(ToolError::Admission {
                limit: AdmissionLimit::SessionProcesses,
            });
        }
        let ticket = match self.enter() {
            Place::Free => return self.claimed(cancel),
            Place::Full => {
                return Err(ToolError::AdmissionQueueFull {
                    limit: AdmissionLimit::SessionProcesses,
                });
            }
            Place::Waiting(ticket) => ticket,
        };
        let _waiter = Waiter {
            slots: Arc::clone(self),
            ticket,
        };
        self.bump();
        let expired = deadline.elapsed();
        tokio::pin!(expired);
        let mut published = None;
        loop {
            if cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            if deadline.is_past() {
                return Err(ToolError::Admission {
                    limit: AdmissionLimit::SessionProcesses,
                });
            }
            match self.turn(ticket) {
                Turn::Claimed => {
                    self.bump();
                    let guard = self.claimed(cancel)?;
                    if published.is_some() {
                        // A cancel since the claim drops the guard here and
                        // publishes nothing more for a call that is leaving.
                        if cancel.is_cancelled() {
                            return Err(ToolError::Cancelled);
                        }
                        port.publish_progress(call, Box::from(""));
                    }
                    return Ok(guard);
                }
                Turn::Waiting(queued) => {
                    if published != Some(queued) {
                        if cancel.is_cancelled() {
                            return Err(ToolError::Cancelled);
                        }
                        port.publish_progress(
                            call,
                            format!("waiting for a process slot ({queued} queued)").into(),
                        );
                        published = Some(queued);
                    }
                }
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(ToolError::Cancelled),
                () = &mut expired => {
                    return Err(ToolError::Admission {
                        limit: AdmissionLimit::SessionProcesses,
                    });
                }
                _ = changes.changed() => {}
            }
        }
    }
}

/// Removes a waiter that leaves the queue without a slot: cancelled, timed
/// out, or dropped. A waiter that was admitted is already gone, so this is a
/// no-op.
struct Waiter {
    slots: Arc<ProcessSlots>,
    ticket: u64,
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let removed = {
            let mut ledger = self.slots.ledger();
            let before = ledger.waiting.len();
            ledger.waiting.retain(|ticket| *ticket != self.ticket);
            ledger.waiting.len() != before
        };
        if removed {
            self.slots.bump();
        }
    }
}

/// One live child's claim on the session cap; dropping it frees the slot and
/// wakes the head waiter.
#[derive(Debug)]
#[must_use = "dropping a slot guard frees its session slot"]
pub(crate) struct SlotGuard {
    slots: Arc<ProcessSlots>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        {
            let mut ledger = self.slots.ledger();
            ledger.live = ledger.live.saturating_sub(1);
        }
        self.slots.bump();
    }
}

#[cfg(test)]
mod tests {
    use std::task::Poll;
    use std::time::Duration;

    use futures::poll;

    use super::*;
    use crate::admission::{Admission, Limits};
    use crate::proc::SESSION_CHILD_LIMIT;

    /// Records every row published, in order, per call.
    #[derive(Default)]
    struct Rows(Mutex<Vec<(String, String)>>);

    impl Rows {
        fn last(&self, call: &str) -> Option<String> {
            let rows = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            rows.iter()
                .rev()
                .find(|(key, _)| key == call)
                .map(|(_, text)| text.clone())
        }

        fn count(&self, call: &str) -> usize {
            let rows = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            rows.iter().filter(|(key, _)| key == call).count()
        }
    }

    impl ProgressPort for Rows {
        fn publish_progress(&self, call: &CallId, tail: Box<str>) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((call.as_str().to_owned(), tail.into()));
        }
    }

    fn row(queued: usize) -> String {
        format!("waiting for a process slot ({queued} queued)")
    }

    /// A deadline `limits.admission_wait` (30 s) from now: far enough that
    /// the tests that do not test it never meet it.
    fn soon() -> Deadline {
        Admission::new(Limits::default(), 1000).begin()
    }

    #[tokio::test]
    async fn a_free_slot_is_taken_without_a_row() {
        let slots = ProcessSlots::new(2, 8);
        let rows = Rows::default();
        let cancel = CancellationToken::new();
        let first = slots
            .acquire(&CallId::new("a"), &rows, &cancel, soon())
            .await;
        assert!(first.is_ok());
        assert_eq!(slots.live(), 1);
        assert_eq!(rows.count("a"), 0, "no waiting row when a slot is free");
    }

    #[tokio::test]
    async fn waiters_show_the_live_count_and_start_in_arrival_order() {
        let slots = ProcessSlots::new(1, 8);
        let rows = Rows::default();
        let cancel = CancellationToken::new();
        let (b_call, c_call, d_call) = (CallId::new("b"), CallId::new("c"), CallId::new("d"));
        let a = slots
            .acquire(&CallId::new("a"), &rows, &cancel, soon())
            .await
            .expect("the first call takes the only slot");
        let mut b = Box::pin(slots.acquire(&b_call, &rows, &cancel, soon()));
        let mut c = Box::pin(slots.acquire(&c_call, &rows, &cancel, soon()));
        let mut d = Box::pin(slots.acquire(&d_call, &rows, &cancel, soon()));
        assert!(poll!(b.as_mut()).is_pending());
        assert!(poll!(c.as_mut()).is_pending());
        assert!(poll!(d.as_mut()).is_pending());
        assert_eq!(rows.last("b"), Some(row(1)));
        assert_eq!(rows.last("c"), Some(row(2)));
        assert_eq!(rows.last("d"), Some(row(3)));
        assert!(poll!(b.as_mut()).is_pending());
        assert_eq!(
            rows.last("b"),
            Some(row(3)),
            "a join updates earlier waiters"
        );

        drop(a);
        assert!(
            poll!(d.as_mut()).is_pending(),
            "a later waiter never overtakes the head"
        );
        assert!(
            poll!(c.as_mut()).is_pending(),
            "a later waiter never overtakes the head"
        );
        let Poll::Ready(Ok(b_guard)) = poll!(b.as_mut()) else {
            panic!("the head waiter takes the freed slot");
        };
        assert_eq!(slots.live(), 1);
        assert_eq!(
            rows.last("b"),
            Some(String::new()),
            "an admitted call no longer looks queued"
        );
        assert!(poll!(c.as_mut()).is_pending());
        assert_eq!(
            rows.last("c"),
            Some(row(2)),
            "an admission updates the rest"
        );

        drop(b_guard);
        let Poll::Ready(Ok(_c_guard)) = poll!(c.as_mut()) else {
            panic!("the second waiter is next");
        };
        assert!(poll!(d.as_mut()).is_pending());
        assert_eq!(rows.last("d"), Some(row(1)));
        assert_eq!(rows.last("c"), Some(String::new()));
    }

    #[tokio::test]
    async fn a_cancelled_or_dropped_waiter_leaves_without_a_slot() {
        let slots = ProcessSlots::new(1, 8);
        let rows = Rows::default();
        let live = CancellationToken::new();
        let stop = CancellationToken::new();
        let (b_call, c_call) = (CallId::new("b"), CallId::new("c"));
        let a = slots
            .acquire(&CallId::new("a"), &rows, &live, soon())
            .await
            .expect("the first call takes the only slot");
        let mut b = Box::pin(slots.acquire(&b_call, &rows, &stop, soon()));
        let mut c = Box::pin(slots.acquire(&c_call, &rows, &live, soon()));
        assert!(poll!(b.as_mut()).is_pending());
        assert!(poll!(c.as_mut()).is_pending());
        assert_eq!(rows.last("c"), Some(row(2)));

        stop.cancel();
        let Poll::Ready(Err(ToolError::Cancelled)) = poll!(b.as_mut()) else {
            panic!("a cancelled waiter fails promptly");
        };
        assert_eq!(slots.queued(), 1);
        assert!(poll!(c.as_mut()).is_pending());
        assert_eq!(rows.last("c"), Some(row(1)), "a leaver updates the rest");
        assert_eq!(
            rows.last("b"),
            Some(row(1)),
            "a cancelled waiter publishes nothing more"
        );

        drop(c);
        assert_eq!(slots.queued(), 0, "dropping the future leaves the queue");
        drop(a);
        assert_eq!(slots.live(), 0, "a leaver never held or consumed a slot");
    }

    #[tokio::test]
    async fn a_waiter_cancelled_as_a_slot_frees_never_claims_it() {
        let slots = ProcessSlots::new(1, 8);
        let rows = Rows::default();
        let live = CancellationToken::new();
        let stop = CancellationToken::new();
        let b_call = CallId::new("b");
        let a = slots
            .acquire(&CallId::new("a"), &rows, &live, soon())
            .await
            .expect("the first call takes the only slot");
        let mut b = Box::pin(slots.acquire(&b_call, &rows, &stop, soon()));
        assert!(poll!(b.as_mut()).is_pending());
        stop.cancel();
        drop(a);
        let Poll::Ready(Err(ToolError::Cancelled)) = poll!(b.as_mut()) else {
            panic!("the head waiter sees its cancel before it claims the freed slot");
        };
        assert_eq!(slots.live(), 0);
        assert_eq!(slots.queued(), 0);
    }

    #[test]
    fn a_claim_that_races_a_cancel_is_released_at_once() {
        let slots = ProcessSlots::new(1, 8);
        let cancel = CancellationToken::new();
        let changes = slots.changed.subscribe();
        // The slot is counted, then the cancel lands before the guard is
        // handed out: the claim must give the slot back.
        slots.ledger().live += 1;
        cancel.cancel();
        assert!(matches!(slots.claimed(&cancel), Err(ToolError::Cancelled)));
        assert_eq!(slots.live(), 0, "the claimed slot was released");
        assert!(
            changes.has_changed().unwrap_or(false),
            "the release woke the waiters"
        );
        // Without the cancel the same claim hands the guard out.
        slots.ledger().live += 1;
        let guard = slots.claimed(&CancellationToken::new());
        assert!(guard.is_ok());
        assert_eq!(slots.live(), 1);
    }

    #[tokio::test]
    async fn a_cancelled_call_never_claims_a_free_slot() {
        let slots = ProcessSlots::new(1, 8);
        let rows = Rows::default();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = slots
            .acquire(&CallId::new("a"), &rows, &cancel, soon())
            .await;
        assert!(matches!(result, Err(ToolError::Cancelled)));
        assert_eq!(slots.live(), 0);
        assert_eq!(rows.count("a"), 0);
    }

    #[tokio::test]
    async fn a_full_waiting_queue_sheds_a_new_call_without_growing() {
        let slots = ProcessSlots::new(1, 2);
        let rows = Rows::default();
        let cancel = CancellationToken::new();
        let (b_call, c_call, d_call, e_call) = (
            CallId::new("b"),
            CallId::new("c"),
            CallId::new("d"),
            CallId::new("e"),
        );
        let _a = slots
            .acquire(&CallId::new("a"), &rows, &cancel, soon())
            .await
            .expect("the first call takes the only slot");
        let mut b = Box::pin(slots.acquire(&b_call, &rows, &cancel, soon()));
        let mut c = Box::pin(slots.acquire(&c_call, &rows, &cancel, soon()));
        assert!(poll!(b.as_mut()).is_pending());
        assert!(poll!(c.as_mut()).is_pending());
        let mut d = Box::pin(slots.acquire(&d_call, &rows, &cancel, soon()));
        let Poll::Ready(Err(ToolError::AdmissionQueueFull { limit })) = poll!(d.as_mut()) else {
            panic!("a call past the waiting bound is shed at once");
        };
        assert_eq!(limit, AdmissionLimit::SessionProcesses);
        assert_eq!(slots.queued(), 2, "the queue did not grow");
        assert_eq!(rows.count("d"), 0, "a shed call publishes no row");
        drop(b);
        let mut e = Box::pin(slots.acquire(&e_call, &rows, &cancel, soon()));
        assert!(
            poll!(e.as_mut()).is_pending(),
            "a freed place admits a new waiter"
        );
        assert_eq!(slots.queued(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiter_gives_up_at_the_admission_deadline_and_leaves_the_queue() {
        let slots = ProcessSlots::new(1, 8);
        let rows = Rows::default();
        let cancel = CancellationToken::new();
        let deadline = soon();
        let b_call = CallId::new("b");
        let a = slots
            .acquire(&CallId::new("a"), &rows, &cancel, deadline)
            .await
            .expect("the first call takes the only slot");
        let mut b = Box::pin(slots.acquire(&b_call, &rows, &cancel, deadline));
        assert!(poll!(b.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(
            poll!(b.as_mut()).is_pending(),
            "the waiter holds on until the deadline"
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        let Poll::Ready(Err(ToolError::Admission { limit })) = poll!(b.as_mut()) else {
            panic!("the waiter gives up when the deadline passes");
        };
        assert_eq!(limit, AdmissionLimit::SessionProcesses);
        assert_eq!(slots.queued(), 0, "a timed-out waiter left the queue");
        assert_eq!(slots.live(), 1, "the held slot is untouched");
        assert_eq!(
            rows.last("b"),
            Some(row(1)),
            "a timed-out waiter never clears a row it did not earn"
        );
        drop(a);
        assert_eq!(slots.live(), 0);
    }

    #[tokio::test]
    async fn the_lookup_returns_the_session_ledger() {
        let table = tokio::sync::Mutex::new(JobTable::new());
        let cancel = CancellationToken::new();
        let found = ProcessSlots::of(&table, &cancel, soon())
            .await
            .expect("an idle table hands out its ledger");
        assert!(Arc::ptr_eq(&found, &table.lock().await.process_slots()));
    }

    #[tokio::test]
    async fn cancel_abandons_a_table_lock_held_across_io() {
        let table = tokio::sync::Mutex::new(JobTable::new());
        let held = table.lock().await;
        let cancel = CancellationToken::new();
        let mut lookup = Box::pin(ProcessSlots::of(&table, &cancel, soon()));
        assert!(poll!(lookup.as_mut()).is_pending());
        cancel.cancel();
        let Poll::Ready(Err(ToolError::Cancelled)) = poll!(lookup.as_mut()) else {
            panic!("a cancelled lookup does not wait out the table lock");
        };
        drop(held);
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_bounds_a_table_lock_held_across_io() {
        let table = tokio::sync::Mutex::new(JobTable::new());
        let held = table.lock().await;
        let cancel = CancellationToken::new();
        let mut lookup = Box::pin(ProcessSlots::of(&table, &cancel, soon()));
        assert!(poll!(lookup.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(31)).await;
        let Poll::Ready(Err(ToolError::Admission { limit })) = poll!(lookup.as_mut()) else {
            panic!("a lookup that outlasts the deadline fails");
        };
        assert_eq!(limit, AdmissionLimit::SessionProcesses);
        drop(held);
    }

    #[tokio::test]
    async fn the_real_cap_queues_exactly_the_excess_in_arrival_order() {
        let slots = ProcessSlots::new(SESSION_CHILD_LIMIT, SESSION_CHILD_LIMIT);
        let rows = Rows::default();
        let cancel = CancellationToken::new();
        let mut held: Vec<SlotGuard> = Vec::with_capacity(SESSION_CHILD_LIMIT);
        for index in 0..SESSION_CHILD_LIMIT {
            let call = CallId::new(format!("held-{index}"));
            held.push(
                slots
                    .acquire(&call, &rows, &cancel, soon())
                    .await
                    .expect("a call under the cap takes a slot"),
            );
        }
        let calls: Vec<CallId> = (0..10).map(|n| CallId::new(format!("wait-{n}"))).collect();
        let mut waiters: Vec<_> = calls
            .iter()
            .map(|call| Box::pin(slots.acquire(call, &rows, &cancel, soon())))
            .collect();
        for _ in 0..2 {
            for waiter in &mut waiters {
                assert!(poll!(waiter.as_mut()).is_pending());
            }
        }
        assert_eq!(slots.live(), SESSION_CHILD_LIMIT);
        assert_eq!(slots.queued(), 10);
        for n in 0..10 {
            assert_eq!(rows.last(&format!("wait-{n}")), Some(row(10)));
        }

        drop(held.pop());
        for waiter in waiters.iter_mut().skip(1).rev() {
            assert!(poll!(waiter.as_mut()).is_pending());
        }
        let Poll::Ready(Ok(_first)) = poll!(waiters[0].as_mut()) else {
            panic!("the first waiter takes the freed slot");
        };
        assert_eq!(slots.live(), SESSION_CHILD_LIMIT);
        assert_eq!(slots.queued(), 9);
        assert_eq!(rows.last("wait-0"), Some(String::new()));
    }
}
