//! Rust stream watcher contract and per-request delivery.
//!
//! One [`WatchFactory`] starts at most one synchronous [`StreamWatch`] per
//! model request. Deltas visit watchers inline in start order; the first
//! interrupt verdict wins. The loop owns retrying, journaling, and the
//! [`MAX_STREAM_INTERRUPTS`] cap; watchers expose fires and durable record
//! payloads through this module's contracts.

use std::{marker::PhantomData, sync::Arc};

use dal_core::{EntryId, RawJson, SessionId, Timestamp, TurnId};

pub use dal_core::ext::{GateSeed, StreamFire, StreamFireAction, WatchBudget};
pub use dal_core::{Channel, StreamVerdict};

/// An opaque record emitted for a stream-rule fire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamWatchRecord {
    /// The extension-defined record kind.
    pub kind: Box<str>,
    /// The validated JSON record body.
    pub body: RawJson,
}

/// Loop-owned interrupt cap per turn; no config key reads this value.
pub const MAX_STREAM_INTERRUPTS: usize = 3;

/// Starts at most one synchronous stream watcher per model request.
pub trait WatchFactory: Send + Sync + 'static {
    /// Returns the single watcher for this request, if this factory
    /// watches the turn.
    fn start(&self, turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>>;
}

/// Synchronous per-request stream watcher; linear in `delta` by contract.
pub trait StreamWatch: Send {
    /// Observes one stream delta and decides whether to interrupt.
    fn feed(&mut self, channel: Channel, delta: &str) -> StreamVerdict;
    /// Observes end of stream and decides whether to interrupt.
    fn finish(&mut self) -> StreamVerdict;
    /// Returns each newly observed rule fire once, in delivery order.
    fn take_fires(&mut self) -> Vec<StreamFire> {
        Vec::new()
    }
    /// Builds the extension record for one fire and its associated entry.
    fn record_body(
        &self,
        _fire: usize,
        _at: Timestamp,
        _entry: Option<EntryId>,
    ) -> Option<StreamWatchRecord> {
        None
    }
    /// Commits a fire to the watch's repeat gate after its record is durable.
    fn gate_record(&mut self, _fire: usize, _entry: Option<EntryId>) {}
}

/// Watch context for one model request; extended by consumers, never here.
/// The lifetime is the plan-pinned `TurnInfo<'_>` contract form; today the
/// context carries the turn alone and the marker holds the parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TurnInfo<'a> {
    /// The session that owns the request.
    pub session: SessionId,
    /// The turn that owns the request.
    pub turn: TurnId,
    /// Holds the contract lifetime; read by the derived impls.
    marker: PhantomData<&'a ()>,
}

impl TurnInfo<'_> {
    /// Starts a watch context for `session_id` and `turn`.
    #[must_use]
    pub const fn new(session_id: SessionId, turn: TurnId) -> Self {
        Self {
            session: session_id,
            turn,
            marker: PhantomData,
        }
    }
}

/// Starts at most one watcher per factory for one model request, in order.
#[must_use]
pub fn start_watchers(
    factories: &[Arc<dyn WatchFactory>],
    turn: &TurnInfo<'_>,
) -> Vec<Box<dyn StreamWatch>> {
    let mut watchers = Vec::with_capacity(factories.len());
    for factory in factories {
        if let Some(watcher) = factory.start(turn) {
            watchers.push(watcher);
        }
    }
    watchers
}

/// Feeds one delta to every watcher in start order; the first interrupt wins.
pub fn feed_watchers(
    watchers: &mut [Box<dyn StreamWatch>],
    channel: &Channel,
    delta: &str,
) -> StreamVerdict {
    for watcher in watchers.iter_mut() {
        let verdict = watcher.feed(channel.clone(), delta);
        if matches!(verdict, StreamVerdict::Interrupt { .. }) {
            return verdict;
        }
    }
    StreamVerdict::Continue
}

/// Finishes every watcher in start order; the first interrupt wins.
pub fn finish_watchers(watchers: &mut [Box<dyn StreamWatch>]) -> StreamVerdict {
    for watcher in watchers.iter_mut() {
        let verdict = watcher.finish();
        if matches!(verdict, StreamVerdict::Interrupt { .. }) {
            return verdict;
        }
    }
    StreamVerdict::Continue
}

/// Whether the loop must stop opening streams after this many interrupts.
#[must_use]
pub const fn interrupts_capped(interrupts: usize) -> bool {
    interrupts >= MAX_STREAM_INTERRUPTS
}
