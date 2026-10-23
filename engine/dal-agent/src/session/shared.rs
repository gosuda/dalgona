//! Synchronous snapshot shared by one actor and its agents.
//!
//! The actor is the only writer: it publishes updates, refreshes fold
//! counters, and closes queues. Agents read snapshots and register
//! subscribers through the same lock without touching the actor channel,
//! which keeps `view` and `subscribe` synchronous.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::num::NonZeroU64;
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};

use dal_core::{
    ApprovalMode, CallId, EntryId, EntryKind, EntryView, Gen, Mode, Name, Notice, PrivateTerminal,
    Seq, Session, ThinkingLevel, Update, UpdateKind, View, ext::GateSeed,
};

use super::projection::{Projection, SnapshotArgs};
use super::ring::{Replay, ReplayRing, RingCaps};
use super::service_grants::ServiceGrants;
use super::subscriber::{Subscriber, SubscriberShared};
use crate::agent::Delivery;
use crate::ext::{ExtRecord, SessionInstalls, WatchFactory};

/// The extension records visible on the current leaf path, keyed by the
/// fold position that produced them.
struct ExtSnap {
    key: Option<(Option<EntryId>, usize)>,
    rows: Arc<[ExtRecord]>,
}

/// The published private terminal outcomes in End journal order.
///
/// Position `i` always corresponds to fold terminal index `i`: terminals are
/// appended once and never rewritten, so a read between advances observes
/// the same allocations.
struct PrivatePub {
    /// How many fold terminals have been published so far.
    published: usize,
    /// One shared payload per published terminal, in source order.
    terminal: Vec<Arc<PrivateTerminal>>,
}
/// Per-session setup products, keyed by validated extension owner name.
struct SetupState {
    owners: BTreeSet<Name>,
    states: BTreeMap<Name, Arc<dyn Any + Send + Sync>>,
    watches: Arc<[(Name, Arc<dyn WatchFactory>)]>,
    notices: Vec<Notice>,
}

/// The prompt customization one child session carries across turns and
/// reloads, journaled by its start and replayed with the journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChildPromptPolicy {
    /// Replaces the product-owned base sections of the system prompt;
    /// extension-contributed sections still render.
    System(Box<str>),
    /// Appends one prompt section after the assembled base prompt, so
    /// siblings on one model share the cached prefix.
    Role(Box<str>),
}

/// Synchronous snapshot of one live session.
pub(crate) struct Shared {
    inner: Mutex<SharedInner>,
    ext: Mutex<ExtSnap>,
    private: Mutex<PrivatePub>,
    setup: Mutex<SetupState>,
    rule_seeds: Mutex<Arc<[GateSeed]>>,
    can_interrupt_stream: AtomicBool,
    thinking_explicit: AtomicBool,
    promoted: Mutex<Arc<BTreeSet<Name>>>,
    allow_always: Mutex<Arc<BTreeSet<Name>>>,
    tool_allowlist: OnceLock<Arc<BTreeSet<Name>>>,
    child_prompt_policy: OnceLock<ChildPromptPolicy>,
    /// The tool-round bound the next turn takes; the turn consumes it.
    next_turn_step_cap: Mutex<Option<std::num::NonZeroU32>>,
    scoped: Mutex<Option<Vec<Box<str>>>>,
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
    /// Returns the sequence of the most recently published update; one
    /// before the first publish (the empty-snapshot sentinel; no update
    /// carries it). Observation cutoffs and delivery marks share this
    /// cursor.
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
                next_seq: 2,
                subscribers: Vec::new(),
            }),
            ext: Mutex::new(ExtSnap {
                key: None,
                rows: Arc::from([]),
            }),
            private: Mutex::new(PrivatePub {
                published: 0,
                terminal: Vec::new(),
            }),
            setup: Mutex::new(SetupState {
                owners: BTreeSet::new(),
                states: BTreeMap::new(),
                watches: Arc::from([]),
                notices: Vec::new(),
            }),
            rule_seeds: Mutex::new(Arc::from([])),
            can_interrupt_stream: AtomicBool::new(true),
            thinking_explicit: AtomicBool::new(false),
            promoted: Mutex::new(Arc::new(BTreeSet::new())),
            allow_always: Mutex::new(Arc::new(BTreeSet::new())),
            tool_allowlist: OnceLock::new(),
            child_prompt_policy: OnceLock::new(),
            next_turn_step_cap: Mutex::new(None),
            scoped: Mutex::new(None),
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

    /// Returns the session-wide always-allowed tools, as of the last sync.
    pub(crate) fn allow_always(&self) -> Arc<BTreeSet<Name>> {
        Arc::clone(
            &self
                .allow_always
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

    /// Sets the tool-round bound the next turn takes, or clears it.
    pub(crate) fn set_next_turn_step_cap(&self, cap: Option<std::num::NonZeroU32>) {
        *self
            .next_turn_step_cap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = cap;
    }

    /// Takes the tool-round bound for the turn that is starting: a bound
    /// applies to one turn and never to a later one.
    pub(crate) fn take_next_turn_step_cap(&self) -> Option<std::num::NonZeroU32> {
        self.next_turn_step_cap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// The tool names the session may use; `None` means no restriction.
    pub(crate) fn tool_allowlist(&self) -> Option<Arc<BTreeSet<Name>>> {
        self.tool_allowlist.get().map(Arc::clone)
    }

    /// Sets the child prompt policy; the first policy stands and a later
    /// call cannot replace it.
    pub(crate) fn set_child_prompt_policy(&self, policy: ChildPromptPolicy) {
        let _ = self.child_prompt_policy.set(policy);
    }

    /// The child prompt policy, when the session carries one.
    pub(crate) fn child_prompt_policy(&self) -> Option<&ChildPromptPolicy> {
        self.child_prompt_policy.get()
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

    /// The execution mode the session runs under now.
    pub(crate) fn mode(&self) -> Mode {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .effective_settings()
            .mode
    }

    pub(crate) fn set_scoped(&self, scoped: Option<Vec<Box<str>>>) {
        *self
            .scoped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = scoped;
    }

    pub(crate) fn scoped_models(&self) -> Vec<Box<str>> {
        self.scoped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_default()
    }

    /// Refreshes fold-owned promotion and always-allowed sets on every call,
    /// publishes newly settled private terminals, then republishes
    /// current-leaf extension records when the fold's leaf or record count
    /// moved since the last publication.
    ///
    /// The private publication runs before the extension-record memo guard:
    /// a private-End-only advance moves neither leaf nor ext length, so any
    /// placement after that early return would serve stale live reads.
    ///
    /// The actor is the only caller; readers get immutable snapshots and
    /// never wait on the actor, so a hook awaiting the actor cannot block a
    /// record read. The memo guard below applies only to extension records.
    pub(crate) fn sync_ext(&self, fold: &Session) {
        self.sync_allow_always(fold);
        self.sync_promoted(fold);
        self.sync_rule_seeds(fold);
        self.can_interrupt_stream
            .store(fold.can_interrupt_stream(), Ordering::Release);
        self.sync_private(fold);
        let key = (fold.leaf_entry(), fold.ext_len());
        {
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
        self.thinking_explicit.store(
            fold.leaf_entries()
                .iter()
                .any(|entry| matches!(entry.kind, EntryKind::Thinking { .. })),
            Ordering::Release,
        );
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

    fn sync_allow_always(&self, fold: &Session) {
        let mut allow_always = self
            .allow_always
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if allow_always.as_ref() != fold.allow_always() {
            *allow_always = Arc::new(fold.allow_always().clone());
        }
    }
    fn sync_rule_seeds(&self, fold: &Session) {
        let seeds = fold.rule_seeds();
        let mut current = self
            .rule_seeds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.len() != seeds.len() {
            *current = seeds.to_vec().into();
        }
    }
    /// Returns all durable gate facts, including facts hidden from the current leaf.
    pub(crate) fn rule_seeds(&self) -> Arc<[GateSeed]> {
        Arc::clone(
            &self
                .rule_seeds
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Returns whether the legacy watcher may interrupt the current stream.
    pub(crate) fn can_interrupt_stream(&self) -> bool {
        self.can_interrupt_stream.load(Ordering::Acquire)
    }

    /// Stores one extension's setup products under its validated owner name.
    pub(crate) fn install_setup(&self, owner: Name, installs: SessionInstalls) -> Result<(), ()> {
        let mut setup = self
            .setup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !setup.owners.insert(owner.clone()) {
            return Err(());
        }
        let mut watches = setup.watches.to_vec();
        watches.extend(
            installs
                .watches
                .into_iter()
                .map(|watcher| (owner.clone(), watcher)),
        );
        setup.watches = watches.into();
        if let Some(state) = installs.state {
            setup.states.insert(owner, state);
        }
        setup.notices.extend(installs.notices);
        Ok(())
    }

    /// Returns the retained state for one extension owner.
    pub(crate) fn owner_state(&self, owner: &Name) -> Option<Arc<dyn Any + Send + Sync>> {
        self.setup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .states
            .get(owner)
            .cloned()
    }

    pub(crate) fn setup_watches(&self) -> Arc<[(Name, Arc<dyn WatchFactory>)]> {
        Arc::clone(
            &self
                .setup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .watches,
        )
    }

    pub(crate) fn take_setup_notices(&self) -> Vec<Notice> {
        let mut setup = self
            .setup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut setup.notices)
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

    /// Publishes every fold terminal not yet shared, oldest first.
    ///
    /// Relies on the append-only invariant: the published cursor never
    /// exceeds the fold terminal length. The live fold only grows, and
    /// `restore_fold` replays the same session, so the cursor stays valid.
    ///
    /// Each terminal payload is cloned into its `Arc` exactly once per
    /// session lifetime, on first publication (including repair terminals at
    /// open). Unchanged state costs one lock plus a length compare with no
    /// allocation.
    pub(crate) fn sync_private(&self, fold: &Session) {
        let mut published = self
            .private
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = fold.private_terminals_since(published.published);
        if fresh.is_empty() {
            return;
        }
        published
            .terminal
            .extend(fresh.iter().map(|terminal| Arc::new(terminal.clone())));
        published.published += fresh.len();
    }

    /// Returns the shared payloads matching `pred`, oldest first.
    ///
    /// Only matching `Arc`s are cloned (pointer copies, no payload bytes);
    /// the lock is released before the caller clones any payload. The
    /// predicate is supplied by the backend call site, so filtering stays
    /// backend-owned.
    pub(crate) fn private_terminal_select(
        &self,
        pred: impl Fn(&PrivateTerminal) -> bool,
    ) -> Vec<Arc<PrivateTerminal>> {
        self.private
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .terminal
            .iter()
            .filter(|terminal| pred(terminal.as_ref()))
            .cloned()
            .collect()
    }

    /// Applies one update kind, publishes it, and offers it to subscribers.
    pub(crate) fn publish(&self, kind: UpdateKind) {
        let scoped = self.scoped_models();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.projection.apply(&kind);
        let kind = match kind {
            UpdateKind::Settings(_) => {
                let mut settings = inner.projection.effective_settings();
                settings.scoped_models = scoped;
                UpdateKind::Settings(settings)
            }
            other => other,
        };
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
        let scoped = self.scoped_models();
        let mut view = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projection
            .snapshot(args);
        view.settings.scoped_models = scoped;
        view
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

    /// Whether a `SetThinking` command chose the session's thinking level on
    /// the current leaf path, as of the last sync.
    ///
    /// The fold journals the choice as an `EntryKind::Thinking` entry that
    /// the published projection does not carry, so the flag is read off the
    /// fold path.
    pub(crate) fn has_explicit_thinking(&self) -> bool {
        self.thinking_explicit.load(Ordering::Acquire)
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
        let scoped = self.scoped_models();
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = inner.projection.leaf_entries();
        let mut view = inner.projection.snapshot(args);
        view.settings.scoped_models = scoped;
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

/// Process progress rides the keyed publication: each subscriber queue
/// replaces its unsent row for the same call, so a running child leaves
/// one transient card per watcher.
impl crate::proc::ProgressPort for Shared {
    fn publish_progress(&self, call: &CallId, tail: Box<str>) {
        self.publish(UpdateKind::ToolProgress {
            call: call.clone(),
            tail,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_ext_publishes_fold_allow_always() {
        let (fold, _) = Session::replay(
            [dal_core::Record::AllowAlways {
                at: jiff::Timestamp::UNIX_EPOCH,
                tool: "run".into(),
                by: dal_core::ClientId::new("tui"),
            }],
            jiff::Timestamp::UNIX_EPOCH,
        )
        .expect("allow-always record replays");
        let shared = Shared::new(
            false,
            Gen::new(std::num::NonZeroU64::MIN),
            ThinkingLevel::Medium,
            ApprovalMode::Ask,
            Mode::Normal,
        );

        shared.sync_ext(&fold);

        assert!(
            shared
                .allow_always()
                .contains(&Name::parse("run").expect("run name parses"))
        );
    }
}
