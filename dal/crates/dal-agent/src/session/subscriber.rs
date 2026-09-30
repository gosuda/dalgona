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
    pub(crate) fn with_backlog(backlog: VecDeque<Delivery>, current: (Gen, Seq)) -> Self {
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
