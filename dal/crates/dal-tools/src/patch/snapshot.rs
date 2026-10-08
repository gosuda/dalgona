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
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
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
            || !rows.iter().all(|&(first, last)| {
                first != 0 && first <= last && spans(entry.shown(from), first, last)
            })
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
    let rows: Vec<(u64, u64)> = rows.to_vec();
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use dal_core::GenerationId;

    use super::*;

    const BOOT: [u8; 16] = [0xab; 16];

    fn minted(seq: u64) -> ReadRef {
        ReadRef { boot: BOOT, seq }
    }

    fn invocation(number: u64) -> Consumer {
        Consumer::Invocation(NonZeroU64::new(number).expect("nonzero invocation"))
    }

    fn store() -> (SnapshotStore, SessionId, GenerationId) {
        (
            SnapshotStore::with_budget(BOOT, 1 << 20),
            SessionId::new_v7(),
            GenerationId::new(NonZeroU64::MIN),
        )
    }

    fn capture(store: &SnapshotStore, session: SessionId, generation: GenerationId) -> ReadRef {
        let (reference, _snapshot) = store
            .capture(
                session,
                generation,
                Consumer::Model,
                Path::new("a.txt"),
                b"one\ntwo\nthree\n",
            )
            .expect("capture");
        reference
    }

    /// The codec is a wire boundary: `display` output must re-parse to the
    /// identical reference across digit-range boundaries in both fields.
    #[test]
    fn read_ref_display_parse_round_trip() {
        let boots = [[0; 16], [0xff; 16], BOOT];
        let seqs = [
            1,
            9,
            10,
            35,
            36,
            37,
            1295, // 36^2 - 1: max two-digit seq.
            1296, // 36^2.
            u64::MAX,
        ];
        for boot in boots {
            for seq in seqs {
                let source = ReadRef { boot, seq };
                let text = source.display();
                let back = ReadRef::parse(&text).expect("display output must parse");
                assert_eq!(back, source, "round trip lost fidelity for {text}");
                // Display is canonical: parsing and re-rendering is stable.
                assert_eq!(back.display(), text);
            }
        }
    }

    /// Model-supplied `r<hex>.<base36>` tokens must reject every malformed
    /// shape without panic or silent reinterpretation.
    #[test]
    fn read_ref_parse_rejects_malformed() {
        let boot = "abababababababababababababababab";
        let valid = format!("r{boot}.1a");
        for text in [
            String::new(),
            String::from("x"),
            String::from("r"),
            format!("{boot}.1"),                   // missing r prefix
            format!("r{boot}"),                    // missing seq separator
            String::from("r.1"),                   // empty boot
            format!("r{boot}."),                   // empty seq
            format!("r{boot}.0"),                  // zero seq
            format!("r{boot}.01"),                 // leading zero
            format!("r{boot}.12345678901234"),     // seq longer than 13 chars
            format!("r{boot}.A"),                  // uppercase seq digit
            format!("r{boot}.~"),                  // non base-36 byte
            format!("r{boot}.z."),                 // trailing separator
            format!("r{}.1", &boot[..31]),         // short boot
            format!("r{boot}a.1"),                 // long boot
            format!("r{}.1", boot.to_uppercase()), // uppercase boot
            format!("r{boot}g.1"),                 // non-hex boot byte
            format!("r{boot}-.1"),                 // dash in boot
            format!("r{boot}.a-a"),                // embedded separator byte
        ] {
            assert_eq!(ReadRef::parse(&text), None, "accepted {text:?}");
        }
        // "1a" is a valid base-36 seq: parse it, never reject it.
        assert_eq!(
            ReadRef::parse(&valid),
            Some(ReadRef {
                boot: BOOT,
                seq: 46,
            })
        );
    }

    /// Max base-36 digit and saturated arithmetic must parse without wrap.
    #[test]
    fn read_ref_parse_accepts_boundaries() {
        let boot = "ffffffffffffffffffffffffffffffff";
        assert_eq!(
            ReadRef::parse(&format!("r{boot}.z")),
            Some(ReadRef {
                boot: [0xff; 16],
                seq: 35,
            })
        );
        assert_eq!(
            ReadRef::parse(&format!("r{boot}.zz")),
            Some(ReadRef {
                boot: [0xff; 16],
                seq: 1295,
            })
        );
        // 13-char base-36 max: "zzzzzzzzzzzzz" overflows u64 and must reject.
        assert_eq!(ReadRef::parse(&format!("r{boot}.zzzzzzzzzzzzz")), None);
    }

    /// Store tokens are the `display` form: every minted reference must
    /// round-trip through `parse` and resolve back to its buffer.
    #[test]
    fn capture_mints_monotone_parseable_references() {
        let (store, session, generation) = store();
        let first = capture(&store, session, generation);
        let second = capture(&store, session, generation);
        assert!(second.seq > first.seq);
        assert_eq!(first.boot, BOOT);
        for reference in [first, second] {
            let text = reference.display();
            assert_eq!(ReadRef::parse(&text), Some(reference));
            let snapshot = store.snapshot(reference).expect("live snapshot");
            assert_eq!(&*snapshot.bytes, b"one\ntwo\nthree\n");
        }
    }

    /// `new` must use the host budget, not a degenerate one: a second
    /// capture must not evict the first.
    #[test]
    fn default_budget_keeps_small_captures() {
        let store = SnapshotStore::new(BOOT);
        let session = SessionId::new_v7();
        let generation = GenerationId::new(NonZeroU64::MIN);
        let first = capture(&store, session, generation);
        let _second = capture(&store, session, generation);
        assert!(store.snapshot(first).is_some());
    }

    /// `with_budget` binds the given boot identity into minted references.
    #[test]
    fn with_budget_uses_given_boot() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        assert_eq!(reference.boot, BOOT);
    }

    /// Delivery only counts rows the claiming consumer was shown; the
    /// eligibility cutoff freezes at call time (INV-CUTOFF).
    #[test]
    fn covers_requires_shown_delivery_at_cutoff() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        // Nothing shown: a deliver call records nothing.
        store.deliver(session, Consumer::Model, reference, 1, 3, 5);
        assert!(!store.covers(session, Consumer::Model, reference, 1, 3, 5));
        // Now show rows 1..=3 and deliver at cutoff 5.
        store.show(reference, Consumer::Model, 1, 3);
        store.deliver(session, Consumer::Model, reference, 1, 3, 5);
        assert!(store.covers(session, Consumer::Model, reference, 1, 3, 5));
        // The same delivery does not authorize an earlier cutoff.
        assert!(!store.covers(session, Consumer::Model, reference, 1, 3, 4));
        // Rows outside the delivered span stay uncovered.
        assert!(!store.covers(session, Consumer::Model, reference, 1, 4, 5));
        assert!(!store.covers(session, Consumer::Model, reference, 4, 4, 5));
    }

    /// Zero and inverted ranges are rejected at every public entry point.
    #[test]
    fn zero_and_inverted_ranges_are_rejected() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        store.show(reference, Consumer::Model, 1, 5);
        store.deliver(session, Consumer::Model, reference, 0, 3, 5);
        store.deliver(session, Consumer::Model, reference, 4, 2, 5);
        assert!(
            store
                .delivered(session, Consumer::Model, reference)
                .is_empty()
        );
        assert!(!store.covers(session, Consumer::Model, reference, 0, 3, 5));
        assert!(!store.covers(session, Consumer::Model, reference, 4, 2, 5));
        store.show(reference, Consumer::Model, 0, 2);
        store.show(reference, Consumer::Model, 3, 1);
        // Rows a rejected `show` must never enter the shown set: they cannot
        // be delivered or handed off.
        store.deliver(session, Consumer::Model, reference, 0, 2, 5);
        store.deliver(session, Consumer::Model, reference, 3, 1, 5);
        assert!(!store.rebind(
            session,
            reference,
            Consumer::Model,
            invocation(9),
            &[(0, 2)],
            5
        ));
        assert!(!store.rebind(
            session,
            reference,
            Consumer::Model,
            invocation(9),
            &[(3, 1)],
            5
        ));
        store.deliver(session, Consumer::Model, reference, 1, 3, 5);
        assert_eq!(
            store.delivered(session, Consumer::Model, reference),
            vec![(1, 3)]
        );
        assert!(!store.covers(session, Consumer::Model, reference, 4, 5, 5));
        // Inverted ranges stay uncovered even once deliveries exist.
        assert!(!store.covers(session, Consumer::Model, reference, 4, 2, 5));
    }

    /// `covers` needs the snapshot live: an expired reference never covers.
    #[test]
    fn covers_rejects_expired_reference() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        store.show(reference, Consumer::Model, 1, 3);
        store.deliver(session, Consumer::Model, reference, 1, 3, 5);
        let stale = minted(99);
        assert!(!store.covers(session, Consumer::Model, stale, 1, 3, 5));
    }

    /// `rebind` moves shown rows to a recipient and fails otherwise:
    /// foreign session, unbound source, or rows never shown to `from`.
    #[test]
    fn rebind_transfers_only_shown_rows_to_bound_recipient() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        let child = invocation(1);
        store.show(reference, Consumer::Model, 1, 3);
        // Rows outside the shown span cannot be handed off.
        assert!(!store.rebind(session, reference, Consumer::Model, child, &[(1, 4)], 7));
        // A foreign session holds no binding.
        assert!(!store.rebind(
            SessionId::new_v7(),
            reference,
            Consumer::Model,
            child,
            &[(1, 3)],
            7
        ));
        // The source itself must be bound: `child` was never bound.
        assert!(!store.rebind(session, reference, child, Consumer::Model, &[(1, 3)], 7));
        // A valid handoff binds the child, shows it the rows, and records
        // the delivery at `at`.
        assert!(store.rebind(session, reference, Consumer::Model, child, &[(1, 3)], 7));
        assert!(store.covers(session, child, reference, 1, 3, 7));
        assert!(!store.covers(session, child, reference, 1, 3, 6));
        // The child can now hand its shown rows onward.
        let grandchild = invocation(2);
        assert!(store.rebind(session, reference, child, grandchild, &[(1, 2)], 9));
        assert!(store.covers(session, grandchild, reference, 1, 2, 9));
        assert!(!store.covers(session, grandchild, reference, 3, 3, 9));
    }

    /// `adopt` transfers only rows delivered to the parent at or before the
    /// parent cutoff; later parent deliveries do not leak into the child.
    #[test]
    fn adopt_transfers_only_parent_cutoff_coverage() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        let parent = Consumer::Model;
        let child = invocation(3);
        store.show(reference, parent, 1, 6);
        store.deliver(session, parent, reference, 1, 3, 4);
        store.deliver(session, parent, reference, 4, 6, 9);
        let (snapshot, rows) = store
            .adopt(session, reference, parent, 5, child)
            .expect("adopt before the second delivery");
        assert_eq!(rows, vec![(1, 3)]);
        assert_eq!(snapshot.path, Path::new("a.txt"));
        assert!(store.covers(session, child, reference, 1, 3, 5));
        // The post-cutoff delivery did not transfer.
        assert!(!store.covers(session, child, reference, 4, 6, 5));
        // Adopting again at a later cutoff picks up the rest.
        let (_snapshot, rows) = store
            .adopt(session, reference, parent, 10, child)
            .expect("second adopt");
        assert_eq!(rows, vec![(1, 6)]);
        assert!(store.covers(session, child, reference, 4, 6, 10));
        // A consumer with no deliveries cannot adopt.
        assert!(
            store
                .adopt(session, reference, invocation(4), 10, invocation(5))
                .is_none()
        );
    }

    /// `lookup` verifies session, consumer binding, generation, and path —
    /// any single mismatch must return `None`.
    #[test]
    fn lookup_binds_every_field() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        let path = Path::new("a.txt");
        assert!(
            store
                .lookup(session, generation, Consumer::Model, reference, path)
                .is_some()
        );
        for (s, g, c, p) in [
            (SessionId::new_v7(), generation, Consumer::Model, path),
            (
                session,
                GenerationId::new(NonZeroU64::new(7).expect("nonzero")),
                Consumer::Model,
                path,
            ),
            (session, generation, invocation(9), path),
            (session, generation, Consumer::Model, Path::new("b.txt")),
        ] {
            assert!(store.lookup(s, g, c, reference, p).is_none());
        }
        // A minted-but-expired token never resolves.
        assert!(
            store
                .lookup(session, generation, Consumer::Model, minted(88), path)
                .is_none()
        );
    }

    /// Adjacent `show` intervals must merge so a later `deliver` spanning
    /// the seam is authorized as one coverage event.
    #[test]
    fn shown_intervals_merge_across_seams() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        store.show(reference, Consumer::Model, 1, 3);
        store.show(reference, Consumer::Model, 5, 6);
        // The seam row alone merges both intervals into (1, 6).
        store.show(reference, Consumer::Model, 4, 4);
        store.deliver(session, Consumer::Model, reference, 1, 6, 5);
        assert!(store.covers(session, Consumer::Model, reference, 1, 6, 5));
        // Overlapping extension merges outward.
        store.show(reference, Consumer::Model, 6, 9);
        store.deliver(session, Consumer::Model, reference, 6, 9, 6);
        assert!(store.covers(session, Consumer::Model, reference, 5, 9, 6));
        // Disjoint intervals do not merge: the gap stays uncovered.
        store.show(reference, Consumer::Model, 12, 14);
        store.deliver(session, Consumer::Model, reference, 12, 14, 7);
        assert!(!store.covers(session, Consumer::Model, reference, 10, 12, 7));
    }

    /// Byte-budget eviction drops the oldest capture, wipes its deliveries,
    /// and never lets the sequence reuse or collide with a live token.
    #[test]
    fn byte_budget_evicts_oldest_and_preserves_seq_monotonicity() {
        let store = SnapshotStore::with_budget(BOOT, 8);
        let session = SessionId::new_v7();
        let generation = GenerationId::new(NonZeroU64::MIN);
        let first = capture(&store, session, generation); // 14 bytes > budget.
        // Capture evicts *before* inserting; an over-budget single buffer
        // still lands because the loop stops when order is empty.
        assert!(store.snapshot(first).is_some());
        store.show(first, Consumer::Model, 1, 3);
        store.deliver(session, Consumer::Model, first, 1, 3, 5);
        let second = capture(&store, session, generation);
        // The second capture forced the first out.
        assert!(store.snapshot(first).is_none());
        assert!(store.snapshot(second).is_some());
        assert!(store.delivered(session, Consumer::Model, first).is_empty());
        assert!(!store.covers(session, Consumer::Model, first, 1, 3, 5));
        // The sequence never reuses a minted token.
        assert!(second.seq > first.seq);
    }

    /// A capture that lands exactly on the byte budget is accepted; eviction
    /// triggers only when total bytes exceed the cap.
    #[test]
    fn byte_budget_accepts_exact_fit() {
        let store = SnapshotStore::with_budget(BOOT, 28);
        let session = SessionId::new_v7();
        let generation = GenerationId::new(NonZeroU64::MIN);
        let first = capture(&store, session, generation); // 14 bytes
        let second = capture(&store, session, generation); // 28 total: exact fit
        assert!(store.snapshot(first).is_some());
        assert!(store.snapshot(second).is_some());
        // One more byte over the cap evicts the oldest.
        let third = capture(&store, session, generation);
        assert!(store.snapshot(first).is_none());
        assert!(store.snapshot(third).is_some());
    }

    /// The count cap evicts oldest-first regardless of byte size.
    #[test]
    fn count_cap_evicts_oldest_first() {
        let store = SnapshotStore::with_budget(BOOT, usize::MAX);
        let session = SessionId::new_v7();
        let generation = GenerationId::new(NonZeroU64::MIN);
        let mut live = Vec::new();
        for _ in 0..MAX_SNAPSHOTS {
            live.push(capture(&store, session, generation));
        }
        let overflow = capture(&store, session, generation);
        assert!(store.snapshot(live[0]).is_none());
        assert!(store.snapshot(live[1]).is_some());
        assert!(store.snapshot(overflow).is_some());
    }

    /// `delivered` exposes the consumer-bound interval set unchanged.
    #[test]
    fn delivered_reports_recorded_intervals() {
        let (store, session, generation) = store();
        let reference = capture(&store, session, generation);
        assert!(
            store
                .delivered(session, Consumer::Model, reference)
                .is_empty()
        );
        store.show(reference, Consumer::Model, 1, 6);
        store.deliver(session, Consumer::Model, reference, 1, 3, 4);
        store.deliver(session, Consumer::Model, reference, 4, 6, 9);
        assert_eq!(
            store.delivered(session, Consumer::Model, reference),
            vec![(1, 6)]
        );
        // Another consumer sees nothing.
        assert!(
            store
                .delivered(session, invocation(1), reference)
                .is_empty()
        );
    }
}
