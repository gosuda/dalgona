//! Per-subscriber queues that shed load without blocking the actor.
//!
//! Tool progress replaces in place per call; a full queue drops queued
//! deltas, queues exactly one resync, and stops accepting until resubscribe.
//! The actor holds a weak handle and prunes dead subscribers on publish.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use dal_core::{CallId, Gen, Seq, Update, UpdateKind};
use tokio::sync::Notify;

use crate::agent::Delivery;

/// Maximum queued deliveries per subscriber.
const QUEUE_CAP: usize = 1024;

/// Actor-side subscriber handle; the client shares ownership.
pub(crate) struct Subscriber {
    shared: Arc<SubscriberShared>,
}

pub(crate) struct SubscriberShared {
    queue: Mutex<QueueState>,
    notify: Notify,
    approval: bool,
    ask: bool,
}

struct QueueState {
    items: VecDeque<Delivery>,
    progress_at: HashMap<CallId, u64>,
    base: u64,
    lagged: bool,
    pending_resync: Option<(Gen, Seq)>,
    closed: bool,
    current: (Gen, Seq),
}

impl Subscriber {
    /// A subscriber preloaded with replay backlog at the given cursor.
    /// `approval` marks subscribers that may resolve approval requests;
    /// `ask` marks subscribers that may answer extension questions.
    /// Listen-only subscribers declare neither and never count as an
    /// attached answerer.
    pub(crate) fn with_backlog(
        backlog: VecDeque<Delivery>,
        current: (Gen, Seq),
        approval: bool,
        ask: bool,
    ) -> Self {
        Self {
            shared: Arc::new(SubscriberShared {
                queue: Mutex::new(QueueState {
                    items: backlog,
                    progress_at: HashMap::new(),
                    base: 0,
                    lagged: false,
                    pending_resync: None,
                    closed: false,
                    current,
                }),
                notify: Notify::new(),
                approval,
                ask,
            }),
        }
    }

    /// A weak handle for the actor's subscriber table.
    pub(crate) fn downgrade(&self) -> Weak<SubscriberShared> {
        Arc::downgrade(&self.shared)
    }

    /// The client-owned strong handle to the shared queue.
    pub(crate) fn port(&self) -> Arc<SubscriberShared> {
        Arc::clone(&self.shared)
    }

    /// Rebuilds a strong handle from the actor table, if the client lives.
    pub(crate) fn upgrade(slot: &Weak<SubscriberShared>) -> Option<Arc<SubscriberShared>> {
        slot.upgrade()
    }

    /// Whether this subscriber may answer approval requests.
    pub(crate) fn answers_approval(slot: &Arc<SubscriberShared>) -> bool {
        slot.approval && Self::deliverable(slot)
    }

    /// Whether this subscriber may answer extension questions.
    pub(crate) fn answers_ask(slot: &Arc<SubscriberShared>) -> bool {
        slot.ask && Self::deliverable(slot)
    }

    /// Whether `offer` would still accept a delivery for `slot`. A lagged or
    /// closed queue never takes a new request, so a role flag on such a
    /// subscriber must not count it as an attached answerer.
    fn deliverable(slot: &Arc<SubscriberShared>) -> bool {
        let queue = slot
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !(queue.lagged || queue.closed)
    }

    /// Offers one published update; shed or dropped offers return false.
    pub(crate) fn offer(
        slot: &Arc<SubscriberShared>,
        update: Arc<Update>,
        current: (Gen, Seq),
    ) -> bool {
        let mut queue = slot
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.current = current;
        if queue.closed || queue.lagged {
            return false;
        }
        if matches!(update.kind, UpdateKind::ToolProgress { .. }) {
            if let UpdateKind::ToolProgress { call, .. } = &update.kind {
                if let Some(&at) = queue.progress_at.get(call) {
                    if let Some(slot) = at.checked_sub(queue.base).and_then(|index| {
                        usize::try_from(index)
                            .ok()
                            .and_then(|offset| queue.items.get_mut(offset))
                    }) {
                        *slot = Delivery::Update(update);
                        return true;
                    }
                    queue.progress_at.remove(call);
                }
                if queue.items.len() >= QUEUE_CAP {
                    shed(&mut queue, current);
                    return false;
                }
                let pos = queue.base + queue.items.len() as u64;
                queue.progress_at.insert(call.clone(), pos);
            }
        } else if queue.items.len() >= QUEUE_CAP {
            shed(&mut queue, current);
            return false;
        }
        queue.items.push_back(Delivery::Update(update));
        slot.notify.notify_one();
        true
    }

    /// Closes the subscriber; queued items drain before `next` ends.
    pub(crate) fn close(slot: &Arc<SubscriberShared>) {
        let mut queue = slot
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.closed = true;
        slot.notify.notify_waiters();
    }

    /// Returns the next delivery, or `None` after drain on close or lag.
    pub(crate) async fn next(slot: &Arc<SubscriberShared>) -> Option<Delivery> {
        loop {
            {
                let mut queue = slot
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(delivery) = queue.items.pop_front() {
                    let at = queue.base;
                    queue.base += 1;
                    queue.progress_at.retain(|_, pos| *pos != at);
                    return Some(delivery);
                }
                if let Some((generation, seq)) = queue.pending_resync.take() {
                    return Some(Delivery::Resync { generation, seq });
                }
                if queue.lagged || queue.closed {
                    return None;
                }
            }
            slot.notify.notified().await;
        }
    }
}
/// Drops queued deltas, queues one resync when there is room, and latches lag.
fn shed(queue: &mut QueueState, current: (Gen, Seq)) {
    queue.items.retain(|delivery| {
        !matches!(
            delivery,
            Delivery::Update(update) if matches!(update.kind, UpdateKind::Delta { .. })
        )
    });
    queue.progress_at.clear();
    for (offset, delivery) in queue.items.iter().enumerate() {
        let call = match delivery {
            Delivery::Update(update) => match &update.kind {
                UpdateKind::ToolProgress { call, .. } => call.clone(),
                _ => continue,
            },
            Delivery::Resync { .. } => continue,
        };
        queue.progress_at.insert(call, queue.base + offset as u64);
    }
    if queue.items.len() < QUEUE_CAP {
        queue.items.push_back(Delivery::Resync {
            generation: current.0,
            seq: current.1,
        });
    } else {
        queue.pending_resync = Some(current);
    }
    queue.lagged = true;
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    fn cursor() -> (Gen, Seq) {
        (Gen::new(NonZeroU64::MIN), Seq::new(NonZeroU64::MIN))
    }

    fn update() -> Arc<Update> {
        let (r#gen, seq) = cursor();
        Arc::new(Update {
            r#gen,
            seq,
            kind: UpdateKind::Unknown,
        })
    }

    #[test]
    fn a_lagged_subscriber_answers_nothing() {
        let subscriber = Subscriber::with_backlog(VecDeque::new(), cursor(), true, true);
        let slot = subscriber.port();
        assert!(Subscriber::answers_approval(&slot));
        assert!(Subscriber::answers_ask(&slot));
        let update = update();
        for _ in 0..=QUEUE_CAP {
            if !Subscriber::offer(&slot, Arc::clone(&update), cursor()) {
                break;
            }
        }
        assert!(!Subscriber::answers_approval(&slot));
        assert!(!Subscriber::answers_ask(&slot));
    }

    #[test]
    fn a_closed_subscriber_answers_nothing() {
        let subscriber = Subscriber::with_backlog(VecDeque::new(), cursor(), true, true);
        let slot = subscriber.port();
        Subscriber::close(&slot);
        assert!(!Subscriber::answers_approval(&slot));
        assert!(!Subscriber::answers_ask(&slot));
    }

    fn delta(text: &str) -> Arc<Update> {
        let (r#gen, seq) = cursor();
        Arc::new(Update {
            r#gen,
            seq,
            kind: UpdateKind::Delta {
                turn: dal_core::TurnId::new(NonZeroU64::MIN),
                channel: dal_core::StreamChannel::Text,
                text: text.into(),
            },
        })
    }

    fn progress(call: &str, tail: &str) -> Arc<Update> {
        let (r#gen, seq) = cursor();
        Arc::new(Update {
            r#gen,
            seq,
            kind: UpdateKind::ToolProgress {
                call: CallId::new(call),
                tail: tail.into(),
            },
        })
    }

    #[tokio::test]
    async fn slow_subscriber_lag_resync() {
        // Spec workload: a never-reading subscriber takes 5000 deltas plus
        // 50 progress updates for one call. A healthy sibling drains as it
        // goes and must see every delivery with no resync.
        let lagging = Subscriber::with_backlog(VecDeque::new(), cursor(), false, false);
        let slow = lagging.port();
        let healthy = Subscriber::with_backlog(VecDeque::new(), cursor(), false, false);
        let well = healthy.port();

        let mut healthy_delivered = 0usize;
        for index in 0..5000 {
            let update = if index % 100 == 0 {
                progress("call-9", &format!("p{}", index / 100))
            } else {
                delta("d")
            };
            let _ = Subscriber::offer(&slow, Arc::clone(&update), cursor());
            assert!(
                Subscriber::offer(&well, Arc::clone(&update), cursor()),
                "the draining sibling keeps room for every delivery"
            );
            if index % 100 == 99 {
                healthy_delivered += drain_available(&well).await;
            }
        }
        // The 50 progress updates replace in place; the shed keeps at most
        // one unsent tool_progress per call.
        for index in 0..50 {
            let update = progress("call-9", &format!("q{index}"));
            let _ = Subscriber::offer(&slow, Arc::clone(&update), cursor());
            assert!(
                Subscriber::offer(&well, Arc::clone(&update), cursor()),
                "the draining sibling keeps room for every delivery"
            );
        }
        healthy_delivered += drain_available(&well).await;
        assert_eq!(
            healthy_delivered, 5001,
            "the sibling takes the whole workload: 5050 pushes minus the \
             49 same-call progress replacements, with nothing shed"
        );
        // Every sibling drain above matched Update only and never saw a
        // Resync: full delivery elsewhere needs no resync.
        // A lagged queue refuses new work until the client resubscribes.
        assert!(
            !Subscriber::offer(&slow, delta("late"), cursor()),
            "a lagged subscriber sheds every later offer"
        );
        {
            let queue = slow
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                queue.items.len() <= QUEUE_CAP,
                "the shed queue never exceeds QUEUE_CAP"
            );
            let deltas = queue
                .items
                .iter()
                .filter(|delivery| {
                    matches!(
                        delivery,
                        Delivery::Update(update)
                            if matches!(update.kind, UpdateKind::Delta { .. })
                    )
                })
                .count();
            assert_eq!(deltas, 0, "shed drops every queued delta");
            let progresses = queue
                .items
                .iter()
                .filter(|delivery| {
                    matches!(
                        delivery,
                        Delivery::Update(update)
                            if matches!(update.kind, UpdateKind::ToolProgress { .. })
                    )
                })
                .count();
            assert!(
                progresses <= 1,
                "at most one unsent tool_progress per call survives, got {progresses}"
            );
            let resyncs = queue
                .items
                .iter()
                .filter(|delivery| matches!(delivery, Delivery::Resync { .. }))
                .count();
            assert_eq!(resyncs, 1, "shed queues exactly one resync");
        }
        // Draining delivers the kept progress, then the single resync,
        // then ends: full delivery resumes after the catch-up resubscribe.
        let mut saw_progress = false;
        loop {
            match Subscriber::next(&slow).await {
                Some(Delivery::Update(update)) => {
                    if matches!(update.kind, UpdateKind::ToolProgress { .. }) {
                        saw_progress = true;
                    }
                }
                Some(Delivery::Resync { .. }) => break,
                None => panic!("the lagged queue ends in a resync, not silence"),
            }
        }
        assert!(saw_progress, "the kept progress delivers before the resync");
        assert!(
            Subscriber::next(&slow).await.is_none(),
            "the lagged queue ends after its single resync"
        );
    }

    async fn drain_available(slot: &Arc<SubscriberShared>) -> usize {
        // Progress replacement keeps one slot per call, so pushed
        // items overcount queued items: read the queue length first.
        // A fixed push-count drain would wait forever on an empty
        // queue and hang the proof.
        let queued = {
            slot.queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .items
                .len()
        };
        for _ in 0..queued {
            match Subscriber::next(slot).await {
                Some(Delivery::Update(_)) => {}
                other => panic!("healthy sibling drains updates only, got {other:?}"),
            }
        }
        queued
    }
}
