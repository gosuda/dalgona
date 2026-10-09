//! Synchronous snapshot shared by one actor and its agents.
//!
//! The actor is the only writer: it publishes updates, refreshes fold
//! counters, and closes queues. Agents read snapshots and register
//! subscribers through the same lock without touching the actor channel,
//! which keeps `view` and `subscribe` synchronous.

use std::collections::{BTreeSet, VecDeque};
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use dal_core::{
    ApprovalMode, EntryId, EntryView, Gen, Mode, Name, Seq, Session, ThinkingLevel, Update,
    UpdateKind, View,
};

use super::projection::{Projection, SnapshotArgs};
use super::ring::{Replay, ReplayRing, RingCaps};
use super::service_grants::ServiceGrants;
use super::subscriber::{Subscriber, SubscriberShared};
use crate::agent::Delivery;
use crate::ext::ExtRecord;

/// The extension records visible on the current leaf path, keyed by the
/// fold position that produced them.
struct ExtSnap {
    key: Option<(Option<EntryId>, usize)>,
    rows: Arc<[ExtRecord]>,
}

/// Synchronous snapshot of one live session.
pub(crate) struct Shared {
    inner: Mutex<SharedInner>,
    ext: Mutex<ExtSnap>,
    promoted: Mutex<Arc<BTreeSet<Name>>>,
    tool_allowlist: OnceLock<Arc<BTreeSet<Name>>>,
    service_grants: ServiceGrants,
}

struct SharedInner {
    generation: Gen,
    projection: Projection,
    ring: ReplayRing,
    next_seq: u64,
    subscribers: Vec<Weak<SubscriberShared>>,
}

impl Shared {
    /// Returns the sequence of the most recently published update; zero
    /// before the first publish. Observation cutoffs and delivery marks
    /// share this cursor (R06 E01).
    pub(crate) fn cursor(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_seq
            .saturating_sub(1)
    }

    /// An empty snapshot at the given generation.
    pub(crate) fn new(
        child: bool,
        generation: Gen,
        thinking: ThinkingLevel,
        approval: ApprovalMode,
        mode: Mode,
    ) -> Self {
        let caps = if child {
            RingCaps::CHILD
        } else {
            RingCaps::TOP_LEVEL
        };
        Self {
            inner: Mutex::new(SharedInner {
                generation,
                projection: Projection::seed(thinking, approval, mode),
                ring: ReplayRing::new(caps, generation, None),
                next_seq: 1,
                subscribers: Vec::new(),
            }),
            ext: Mutex::new(ExtSnap {
                key: None,
                rows: Arc::from([]),
            }),
            promoted: Mutex::new(Arc::new(BTreeSet::new())),
            tool_allowlist: OnceLock::new(),
            service_grants: ServiceGrants::default(),
        }
    }

    /// Returns the deferred tools the fold has promoted, as of the last sync.
    pub(crate) fn promoted(&self) -> Arc<BTreeSet<Name>> {
        Arc::clone(
            &self
                .promoted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Restricts the session to `names`; the first restriction stands and a
    /// later call cannot widen it.
    pub(crate) fn restrict_tools(&self, names: &[Name]) {
        self.tool_allowlist
            .get_or_init(|| Arc::new(names.iter().cloned().collect()));
    }

    /// The tool names the session may use; `None` means no restriction.
    pub(crate) fn tool_allowlist(&self) -> Option<Arc<BTreeSet<Name>>> {
        self.tool_allowlist.get().map(Arc::clone)
    }

    /// The run grants approved calls lent their extensions in this session.
    pub(crate) fn service_grants(&self) -> &ServiceGrants {
        &self.service_grants
    }

    /// The approval mode the session runs under now.
    pub(crate) fn approval(&self) -> ApprovalMode {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .approval()
    }

    /// Refreshes the fold-owned promotion set on every call, then republishes
    /// current-leaf extension records when the fold's leaf or record count
    /// moved since the last publication.
    ///
    /// The actor is the only caller; readers get immutable snapshots and
    /// never wait on the actor, so a hook awaiting the actor cannot block a
    /// record read. The memo guard below applies only to extension records.
    pub(crate) fn sync_ext(&self, fold: &Session) {
        self.sync_promoted(fold);
        let key = (fold.leaf_entry(), fold.ext_len());
        let mut snap = self
            .ext
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if snap.key == Some(key) {
            return;
        }
        snap.rows = fold
            .leaf_ext()
            .filter_map(|row| {
                Some(ExtRecord {
                    ext: row.ext.parse::<Name>().ok()?,
                    kind: row.kind.into(),
                    body: row.body.clone(),
                })
            })
            .collect();
        snap.key = Some(key);
    }

    fn sync_promoted(&self, fold: &Session) {
        let mut promoted = self
            .promoted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if promoted.len() != fold.promoted().len() {
            *promoted = Arc::new(fold.promoted().clone());
        }
    }

    /// Returns the extension records on the current leaf path, all
    /// extensions, in journal order.
    pub(crate) fn ext_records(&self) -> Arc<[ExtRecord]> {
        Arc::clone(
            &self
                .ext
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .rows,
        )
    }

    /// Applies one update kind, publishes it, and offers it to subscribers.
    pub(crate) fn publish(&self, kind: UpdateKind) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.projection.apply(&kind);
        let seq = Seq::new(NonZeroU64::new(inner.next_seq.max(1)).unwrap_or(NonZeroU64::MIN));
        inner.next_seq = inner.next_seq.max(1).saturating_add(1);
        inner.projection.note_seq(seq);
        let update = Arc::new(Update {
            r#gen: inner.generation,
            seq,
            kind,
        });
        if inner.ring.push(Arc::clone(&update)).is_err() {
            return;
        }
        let current = (inner.generation, seq);
        inner.subscribers.retain(|slot| {
            Subscriber::upgrade(slot).is_some_and(|shared| {
                Subscriber::offer(&shared, Arc::clone(&update), current);
                true
            })
        });
    }

    /// Snapshots the materialized view with leaf paging applied.
    pub(crate) fn snapshot(&self, args: SnapshotArgs) -> View {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .snapshot(args)
    }

    /// The last terminal stop the projection recorded, when any.
    pub(crate) fn last_stop(&self) -> Option<dal_core::Stop> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .last_stop
    }

    pub(crate) fn leaf_entries(&self) -> Vec<EntryView> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .leaf_entries()
    }

    pub(crate) fn restore_fold(&self, fold: &Session) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .restore_fold(fold);
        self.sync_ext(fold);
    }

    /// Captures a view and its full leaf path from one projection state.
    pub(crate) fn snapshot_with_leaf_entries(&self, args: SnapshotArgs) -> (View, Vec<EntryView>) {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = inner.projection.leaf_entries();
        let view = inner.projection.snapshot(args);
        (view, entries)
    }
    /// Reports whether a live approval-answering subscriber watches this
    /// session. Listen-only subscribers observe updates but cannot resolve
    /// approval requests, so they never count here.
    pub(crate) fn attached_approval(&self) -> bool {
        self.attached(Subscriber::answers_approval)
    }

    /// Reports whether a live ask-answering subscriber watches this session.
    /// Listen-only subscribers observe updates but cannot answer extension
    /// questions, so they never count here.
    pub(crate) fn attached_ask(&self) -> bool {
        self.attached(Subscriber::answers_ask)
    }

    fn attached(&self, answers: fn(&Arc<SubscriberShared>) -> bool) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .subscribers
            .iter()
            .any(|slot| Subscriber::upgrade(slot).is_some_and(|shared| answers(&shared)))
    }

    pub(crate) fn subscribe(
        self: &Arc<Self>,
        after: Option<(Gen, Seq)>,
        approval: bool,
        ask: bool,
    ) -> Arc<SubscriberShared> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .subscribers
            .retain(|slot| Subscriber::upgrade(slot).is_some());
        let mut backlog = VecDeque::new();
        match inner.ring.replay(after) {
            Replay::Live => {}
            Replay::Suffix(suffix) => {
                backlog.extend(suffix.map(|update| Delivery::Update(Arc::clone(update))));
            }
            Replay::Resync(position) => {
                if let Some(seq) = position.seq {
                    backlog.push_back(Delivery::Resync {
                        generation: position.r#gen,
                        seq,
                    });
                }
            }
        }
        let position = inner.ring.position();
        let current = (
            position.r#gen,
            position.seq.unwrap_or(Seq::new(NonZeroU64::MIN)),
        );
        let subscriber = Subscriber::with_backlog(backlog, current, approval, ask);
        let port = subscriber.port();
        inner.subscribers.push(subscriber.downgrade());
        port
    }

    /// Returns the current status of every extension that is not quiet with
    /// no text, keyed by extension name.
    pub(crate) fn ext_statuses(&self) -> std::collections::BTreeMap<Box<str>, dal_core::ExtStatus> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .ext_statuses()
            .clone()
    }

    /// Refreshes fold-owned counters after a step.
    pub(crate) fn set_fold_stats(&self, steers: u32, follow_ups: u32, auto: bool) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .set_fold_stats(steers, follow_ups, auto);
    }

    /// Marks a manual compaction job as the active turn state.
    pub(crate) fn set_compacting(&self, job: dal_core::JobId) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .set_compacting(job);
    }

    /// Closes every subscriber queue; queued items drain first.
    pub(crate) fn close_all(&self) {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for slot in &inner.subscribers {
            if let Some(shared) = Subscriber::upgrade(slot) {
                Subscriber::close(&shared);
            }
        }
    }
}
