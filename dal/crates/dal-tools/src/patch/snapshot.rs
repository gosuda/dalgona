//! Snapshot references, deliveries, and revision continuity.
//!
//! One named invariant owner for reference bindings, deliveries, leases,
//! and revision continuity. New hashline profiles bind `[PATH@ReadRef]`
//! to an immutable captured buffer; legacy `[PATH#TAG]` stays in the
//! style parsers and `Seen`/`Chain` path.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use dal_core::{CallId, Consumer, SessionId, TurnId};

/// A read reference bound to one immutable captured buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReadRef {
    /// Host boot identity, 128 bits rendered as 32 lowercase hex.
    pub boot: [u8; 16],
    /// Host-wide nonzero base-36 counter, never reused after eviction.
    pub seq: u64,
}

impl ReadRef {
    /// Parses `r<32 hex>.<base36>` without leading zeroes.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix('r')?;
        let (boot_hex, seq_text) = rest.split_once('.')?;
        if boot_hex.len() != 32
            || !boot_hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase() && b != b'-')
        {
            // Accept lowercase hex only; uppercase is rejected.
            if !boot_hex
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            {
                return None;
            }
        }
        if boot_hex.len() != 32 {
            return None;
        }
        let mut boot = [0_u8; 16];
        for (index, chunk) in boot_hex.as_bytes().chunks(2).enumerate() {
            let text = std::str::from_utf8(chunk).ok()?;
            boot[index] = u8::from_str_radix(text, 16).ok()?;
        }
        if seq_text.is_empty() || seq_text.len() > 13 || seq_text.starts_with('0') {
            return None;
        }
        if !seq_text
            .bytes()
            .all(|b: u8| b.is_ascii_digit() || b.is_ascii_lowercase())
        {
            return None;
        }
        let mut seq = 0_u64;
        for byte in seq_text.bytes() {
            let digit = if byte.is_ascii_digit() {
                u64::from(byte - b'0')
            } else {
                u64::from(byte - b'a') + 10
            };
            seq = seq.checked_mul(36)?.checked_add(digit)?;
        }
        if seq == 0 {
            return None;
        }
        Some(Self { boot, seq })
    }

    /// Renders the canonical `r<boot>.<seq>` form.
    #[must_use]
    pub fn display(self) -> Box<str> {
        let mut boot_hex = String::with_capacity(32);
        for byte in self.boot {
            let _ = std::fmt::Write::write_fmt(&mut boot_hex, format_args!("{byte:02x}"));
        }
        let mut seq_text = String::new();
        let mut value = self.seq;
        while value > 0 {
            let digit = (value % 36) as u8;
            seq_text.push(if digit < 10 {
                (b'0' + digit) as char
            } else {
                (b'a' + digit - 10) as char
            });
            value /= 36;
        }
        let seq_text: String = seq_text.chars().rev().collect();
        format!("r{boot_hex}.{seq_text}").into()
    }
}

/// One immutable captured buffer with its revision lineage.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Session-open generation that minted the capture; resume/fork never inherits.
    pub generation: dal_core::GenerationId,
    /// Canonical workspace-relative path.
    pub path: PathBuf,
    /// Full BLAKE3 digest of the captured raw bytes.
    pub digest: [u8; 32],
    /// Captured raw bytes shared across deliveries.
    pub bytes: Arc<[u8]>,
    /// Revision of this capture in its path lineage.
    pub revision: u64,
    /// Epoch invalidated by opaque mutations.
    pub epoch: u64,
    /// Authenticated session that owns the capture.
    pub session: SessionId,
    /// Consumer the capture was first bound to.
    pub consumer: Consumer,
}

/// Delivery coverage for one consumer and reference.
///
/// One event per delivery call: the cutoff at delivery time plus the row
/// intervals that call delivered. Eligibility folds only events at or
/// before the call's frozen cutoff (INV-CUTOFF); later deliveries never
/// authorize pending calls, and earlier rows stay eligible when later
/// rows arrive.
#[derive(Clone, Debug, Default)]
struct Delivery {
    /// Per-delivery events: `(delivery cutoff, row intervals)`.
    events: Vec<(u64, Vec<(u64, u64)>)>,
}

/// Bounded reference and delivery store.
#[derive(Debug, Default)]
pub struct SnapshotStore {
    state: Mutex<StoreState>,
}

#[derive(Debug, Default)]
struct StoreState {
    snapshots: HashMap<Box<str>, Entry>,
    order: VecDeque<Box<str>>,
    deliveries: HashMap<(SessionId, Consumer, Box<str>), Delivery>,
    revisions: HashMap<PathBuf, u64>,
    epochs: HashMap<PathBuf, u64>,
    next_seq: u64,
    boot: [u8; 16],
    bytes: usize,
    max_bytes: usize,
}

/// One captured buffer plus the consumers its reference is bound to.
#[derive(Debug)]
struct Entry {
    snapshot: Snapshot,
    bound: Vec<Consumer>,
    /// Rows each consumer may claim: displayed to it, delivered to it, or
    /// granted to it by adoption. No view may claim more.
    shown: HashMap<Consumer, Vec<(u64, u64)>>,
}

impl Entry {
    fn binds(&self, session: SessionId, consumer: Consumer) -> bool {
        self.snapshot.session == session && self.bound.contains(&consumer)
    }

    fn bind(&mut self, consumer: Consumer) {
        if !self.bound.contains(&consumer) {
            self.bound.push(consumer);
        }
    }

    fn shown(&self, consumer: Consumer) -> &[(u64, u64)] {
        self.shown.get(&consumer).map_or(&[][..], Vec::as_slice)
    }

    fn show(&mut self, consumer: Consumer, first: u64, last: u64) {
        insert_interval(self.shown.entry(consumer).or_default(), first, last);
    }
}

const MAX_SNAPSHOTS: usize = 512;
/// Retained capture bytes across the store; eviction is oldest-first.
const MAX_STORE_BYTES: usize = 64 << 20;

impl SnapshotStore {
    /// Creates an empty store bound to the host's boot identity.
    #[must_use]
    pub fn new(boot: [u8; 16]) -> Self {
        Self::with_budget(boot, MAX_STORE_BYTES)
    }

    /// Creates an empty store with its own retained-byte budget.
    #[must_use]
    pub fn with_budget(boot: [u8; 16], max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(StoreState {
                boot,
                max_bytes,
                ..StoreState::default()
            }),
        }
    }

    /// Captures a buffer and mints a fresh reference token.
    ///
    /// Returns `None` when the host-wide sequence is exhausted; the counter
    /// never wraps or reuses a token after eviction.
    pub fn capture(
        &self,
        session: SessionId,
        generation: dal_core::GenerationId,
        consumer: Consumer,
        path: &Path,
        bytes: &[u8],
    ) -> Option<(ReadRef, Snapshot)> {
        let digest = *blake3::hash(bytes).as_bytes();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let seq = state.next_seq.checked_add(1)?;
        if seq == 0 {
            return None;
        }
        state.next_seq = seq;
        let reference = ReadRef {
            boot: state.boot,
            seq,
        };
        let revision = state
            .revisions
            .get(path)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        state.revisions.insert(path.to_path_buf(), revision);
        let epoch = state.epochs.get(path).copied().unwrap_or(0);
        let snapshot = Snapshot {
            generation,
            path: path.to_path_buf(),
            digest,
            bytes: Arc::from(bytes),
            revision,
            epoch,
            session,
            consumer,
        };
        let token: Box<str> = reference.display();
        while (state.snapshots.len() >= MAX_SNAPSHOTS
            || state.bytes + bytes.len() > state.max_bytes)
            && !state.order.is_empty()
        {
            evict_oldest(&mut state);
        }
        state.order.push_back(token.clone());
        state.bytes += bytes.len();
        state.snapshots.insert(
            token,
            Entry {
                snapshot: snapshot.clone(),
                bound: vec![consumer],
                shown: HashMap::new(),
            },
        );
        Some((reference, snapshot))
    }

    /// Records complete delivered rows for one consumer reference.
    pub fn deliver(
        &self,
        session: SessionId,
        consumer: Consumer,
        reference: ReadRef,
        first: u64,
        last: u64,
        cutoff: u64,
    ) {
        if first == 0 || first > last {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shown = state
            .snapshots
            .get(&reference.display())
            .is_some_and(|entry| spans(entry.shown(consumer), first, last));
        if !shown {
            return;
        }
        record(
            &mut state,
            (session, consumer, reference.display()),
            &[(first, last)],
            cutoff,
        );
    }

    /// Records rows `first..=last` as displayed to `consumer` by the read
    /// that captured `reference`; later deliveries may claim only rows the
    /// claiming consumer has seen.
    pub fn show(&self, reference: ReadRef, consumer: Consumer, first: u64, last: u64) {
        if first == 0 || first > last {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = state.snapshots.get_mut(&reference.display()) {
            entry.show(consumer, first, last);
        }
    }

    /// Returns the captured buffer behind `reference` while it is live.
    #[must_use]
    pub fn snapshot(&self, reference: ReadRef) -> Option<Snapshot> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshots
            .get(&reference.display())
            .map(|entry| entry.snapshot.clone())
    }

    /// Rebinds a live reference held by `from` to the recipient `to` and
    /// records `rows` as delivered to `to` at `at`.
    ///
    /// Returns `false` and records nothing when the reference is expired,
    /// belongs to another session, or is not bound to `from`.
    pub fn rebind(
        &self,
        session: SessionId,
        reference: ReadRef,
        from: Consumer,
        to: Consumer,
        rows: &[(u64, u64)],
        at: u64,
    ) -> bool {
        let token = reference.display();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = state.snapshots.get_mut(&token) else {
            return false;
        };
        if !entry.binds(session, from)
            || !rows
                .iter()
                .all(|&(first, last)| spans(entry.shown(from), first, last))
        {
            return false;
        }
        entry.bind(to);
        for &(first, last) in rows {
            entry.show(to, first, last);
        }
        record(&mut state, (session, to, token), rows, at);
        true
    }

    /// Adopts `parent`'s coverage of `reference` for `child`.
    ///
    /// Only rows delivered to `parent` at or before `parent_cutoff` transfer;
    /// the child receives them as one event at `parent_cutoff`. No source
    /// bytes are read. Returns `None` for an expired, foreign-session,
    /// unbound, or not yet eligible reference.
    pub fn adopt(
        &self,
        session: SessionId,
        reference: ReadRef,
        parent: Consumer,
        parent_cutoff: u64,
        child: Consumer,
    ) -> Option<(Snapshot, Vec<(u64, u64)>)> {
        let token = reference.display();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rows = eligible(
            state.deliveries.get(&(session, parent, token.clone()))?,
            parent_cutoff,
        );
        let entry = state.snapshots.get_mut(&token)?;
        if !entry.binds(session, parent) {
            return None;
        }
        let rows: Vec<(u64, u64)> = rows
            .into_iter()
            .filter(|&(first, last)| spans(entry.shown(parent), first, last))
            .collect();
        if rows.is_empty() {
            return None;
        }
        entry.bind(child);
        for &(first, last) in &rows {
            entry.show(child, first, last);
        }
        let snapshot = entry.snapshot.clone();
        record(&mut state, (session, child, token), &rows, parent_cutoff);
        Some((snapshot, rows))
    }

    /// Looks up a reference and verifies session, consumer, and path binding.
    #[must_use]
    pub fn lookup(
        &self,
        session: SessionId,
        generation: dal_core::GenerationId,
        consumer: Consumer,
        reference: ReadRef,
        path: &Path,
    ) -> Option<Snapshot> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state.snapshots.get(&reference.display())?;
        if !entry.binds(session, consumer)
            || entry.snapshot.generation != generation
            || entry.snapshot.path != path
        {
            return None;
        }
        Some(entry.snapshot.clone())
    }

    /// Returns delivered intervals for one consumer reference.
    #[must_use]
    pub fn delivered(
        &self,
        session: SessionId,
        consumer: Consumer,
        reference: ReadRef,
    ) -> Vec<(u64, u64)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .deliveries
            .get(&(session, consumer, reference.display()))
            .map(|delivery| eligible(delivery, u64::MAX))
            .unwrap_or_default()
    }

    /// Returns whether every line in `first..=last` was fully delivered
    /// at or before `cutoff`. Later deliveries never authorize a pending call.
    #[must_use]
    pub fn covers(
        &self,
        session: SessionId,
        consumer: Consumer,
        reference: ReadRef,
        first: u64,
        last: u64,
        cutoff: u64,
    ) -> bool {
        if first == 0 || first > last {
            return false;
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.snapshots.contains_key(&reference.display()) {
            return false;
        }
        let Some(delivery) = state
            .deliveries
            .get(&(session, consumer, reference.display()))
        else {
            return false;
        };
        spans(&eligible(delivery, cutoff), first, last)
    }
}

fn record(
    state: &mut StoreState,
    key: (SessionId, Consumer, Box<str>),
    rows: &[(u64, u64)],
    cutoff: u64,
) {
    let rows: Vec<(u64, u64)> = rows
        .iter()
        .copied()
        .filter(|&(first, last)| first != 0 && first <= last)
        .collect();
    if rows.is_empty() {
        return;
    }
    let delivery = state.deliveries.entry(key).or_default();
    let event = if let Some(at) = delivery
        .events
        .iter()
        .position(|(event_cutoff, _)| *event_cutoff == cutoff)
    {
        &mut delivery.events[at].1
    } else {
        delivery.events.push((cutoff, Vec::new()));
        let last = delivery.events.len() - 1;
        &mut delivery.events[last].1
    };
    for (first, last) in rows {
        insert_interval(event, first, last);
    }
}

/// Drops the oldest capture and its deliveries, accounting its bytes.
fn evict_oldest(state: &mut StoreState) {
    let Some(oldest) = state.order.pop_front() else {
        return;
    };
    if let Some(entry) = state.snapshots.remove(&oldest) {
        state.bytes = state.bytes.saturating_sub(entry.snapshot.bytes.len());
    }
    state
        .deliveries
        .retain(|(_, _, reference), _| reference != &oldest);
}

/// Whether sorted, merged `intervals` contain every line of `first..=last`.
fn spans(intervals: &[(u64, u64)], first: u64, last: u64) -> bool {
    intervals
        .iter()
        .any(|&(from, to)| from <= first && last <= to)
}

/// Folds the delivery events at or before `cutoff` (INV-CUTOFF).
fn eligible(delivery: &Delivery, cutoff: u64) -> Vec<(u64, u64)> {
    let mut rows = Vec::new();
    for (event_cutoff, event_rows) in &delivery.events {
        if *event_cutoff <= cutoff {
            for &(first, last) in event_rows {
                insert_interval(&mut rows, first, last);
            }
        }
    }
    rows
}

/// Call identity for invocation dedup: session, generation, turn, call.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct InvocationId {
    /// Owning session.
    pub session: SessionId,
    /// Originating open generation.
    pub generation: dal_core::GenerationId,
    /// Owning turn.
    pub turn: TurnId,
    /// Provider call identifier hash.
    pub call: CallId,
}

fn insert_interval(intervals: &mut Vec<(u64, u64)>, first: u64, last: u64) {
    let insert_at =
        intervals.partition_point(|&(_, current_last)| current_last.saturating_add(1) < first);
    let mut merged_first = first;
    let mut merged_last = last;
    let mut merge_end = insert_at;
    while let Some(&(current_first, current_last)) = intervals.get(merge_end) {
        if merged_last.saturating_add(1) < current_first {
            break;
        }
        merged_first = merged_first.min(current_first);
        merged_last = merged_last.max(current_last);
        merge_end += 1;
    }
    if insert_at == merge_end {
        intervals.insert(insert_at, (first, last));
        return;
    }
    intervals[insert_at] = (merged_first, merged_last);
    intervals.drain(insert_at + 1..merge_end);
}
