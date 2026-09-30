//! Bounded replay window of published session updates.
//!
//! The session actor owns one [`ReplayRing`] per session and pushes every
//! update after its journal receipt. The ring keeps the newest gap-free
//! sequence suffix of the current generation within an update-count cap
//! and a byte cap, where bytes are the update's sonic-rs JSON length. The
//! newest update always stays, even when it alone exceeds the byte cap.
//! Subscriber queues live elsewhere; the ring only answers which retained
//! updates follow a client cursor, or that the client must resync.

use std::cmp::Ordering;
use std::collections::{VecDeque, vec_deque};
use std::fmt;
use std::sync::Arc;

use dal_core::{Gen, Seq, Update};

/// Update-count and byte limits for one [`ReplayRing`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RingCaps {
    /// The most updates the ring retains beyond a lone newest update.
    pub(crate) updates: usize,
    /// The most serialized bytes the ring retains beyond a lone newest update.
    pub(crate) bytes: usize,
}

impl RingCaps {
    /// Limits for a top-level session: 4096 updates or 1 MiB.
    pub(crate) const TOP_LEVEL: Self = Self {
        updates: 4096,
        bytes: 1 << 20,
    };
    /// Limits for a child session: 256 updates or 64 KiB.
    pub(crate) const CHILD: Self = Self {
        updates: 256,
        bytes: 64 << 10,
    };
}

/// The ring's current generation and the last sequence published in it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Position {
    /// The current session generation.
    pub(crate) r#gen: Gen,
    /// The last published sequence, or `None` before any is known.
    pub(crate) seq: Option<Seq>,
}

/// How a subscriber starting at a cursor continues.
#[derive(Debug)]
pub(crate) enum Replay<'a> {
    /// Nothing to replay: the subscriber continues with the next push.
    Live,
    /// A non-empty retained suffix, oldest first, strictly after the cursor
    /// and ending at the current head; live pushes follow it.
    Suffix(Suffix<'a>),
    /// The cursor names another generation, runs ahead of the head, or
    /// precedes the retained window; the subscriber must refetch state.
    Resync(Position),
}

/// Borrowed retained updates, oldest first.
#[derive(Clone, Debug)]
pub(crate) struct Suffix<'a>(vec_deque::Iter<'a, Entry>);

impl<'a> Iterator for Suffix<'a> {
    type Item = &'a Arc<Update>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|entry| &entry.update)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ExactSizeIterator for Suffix<'_> {}

/// Why [`ReplayRing::push`] refused an update. The ring is unchanged.
#[derive(Debug)]
pub(crate) enum PushError {
    /// The update belongs to a generation older than the ring's.
    StaleGeneration {
        /// The ring's generation.
        current: Gen,
        /// The refused update's generation.
        got: Gen,
    },
    /// The update does not directly follow the last published sequence.
    Gap {
        /// The last published sequence.
        after: Seq,
        /// The refused update's sequence.
        got: Seq,
    },
    /// The update could not be serialized to measure its size.
    Encode(sonic_rs::Error),
}

impl fmt::Display for PushError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration { current, got } => write!(
                formatter,
                "replay ring is at generation {current} but got generation {got}"
            ),
            Self::Gap { after, got } => write!(
                formatter,
                "replay ring expected the sequence after {after} but got {got}"
            ),
            Self::Encode(error) => write!(formatter, "replay ring could not size update: {error}"),
        }
    }
}

impl std::error::Error for PushError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encode(error) => Some(error),
            Self::StaleGeneration { .. } | Self::Gap { .. } => None,
        }
    }
}

#[derive(Debug)]
struct Entry {
    update: Arc<Update>,
    bytes: usize,
}

/// FIFO of the newest published updates of one generation.
#[derive(Debug)]
pub(crate) struct ReplayRing {
    caps: RingCaps,
    position: Position,
    entries: VecDeque<Entry>,
    bytes: usize,
}

impl ReplayRing {
    /// Creates an empty ring at `head`, the last sequence already published
    /// in `generation` before this ring existed, if any.
    #[must_use]
    pub(crate) const fn new(caps: RingCaps, generation: Gen, head: Option<Seq>) -> Self {
        Self {
            caps,
            position: Position {
                r#gen: generation,
                seq: head,
            },
            entries: VecDeque::new(),
            bytes: 0,
        }
    }

    /// Returns the current generation and last published sequence.
    #[must_use]
    pub(crate) const fn position(&self) -> Position {
        self.position
    }

    /// Appends a published update, then evicts the oldest while either cap
    /// is exceeded and more than one update remains.
    ///
    /// A newer generation clears the retained updates and starts at the
    /// pushed sequence. Within a generation the sequence must follow the
    /// last published one by exactly one.
    ///
    /// # Errors
    /// Returns [`PushError`] for an older generation, a sequence gap, or an
    /// update sonic-rs cannot serialize; the ring is then unchanged.
    pub(crate) fn push(&mut self, update: Arc<Update>) -> Result<(), PushError> {
        let current = self.position.r#gen;
        let switch = match update.r#gen.cmp(&current) {
            Ordering::Less => {
                return Err(PushError::StaleGeneration {
                    current,
                    got: update.r#gen,
                });
            }
            Ordering::Greater => true,
            Ordering::Equal => {
                if let Some(after) = self.position.seq
                    && update.seq.get().checked_sub(1) != Some(after.get())
                {
                    return Err(PushError::Gap {
                        after,
                        got: update.seq,
                    });
                }
                false
            }
        };
        let bytes = sonic_rs::to_vec(&*update).map_err(PushError::Encode)?.len();
        if switch {
            self.entries.clear();
            self.bytes = 0;
        }
        self.position = Position {
            r#gen: update.r#gen,
            seq: Some(update.seq),
        };
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.push_back(Entry { update, bytes });
        while self.entries.len() > 1
            && (self.entries.len() > self.caps.updates || self.bytes > self.caps.bytes)
        {
            if let Some(evicted) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(evicted.bytes);
            }
        }
        Ok(())
    }

    /// Answers where a subscriber that has seen everything through `after`
    /// continues; `None` means a fresh subscriber that starts live.
    ///
    /// The cursor bound is exclusive: the suffix starts at `after.seq + 1`.
    #[must_use]
    pub(crate) fn replay(&self, after: Option<(Gen, Seq)>) -> Replay<'_> {
        let Some((generation, seen)) = after else {
            return Replay::Live;
        };
        let resync = Replay::Resync(self.position);
        if generation != self.position.r#gen {
            return resync;
        }
        let Some(head) = self.position.seq else {
            return resync;
        };
        if seen == head {
            return Replay::Live;
        }
        if seen > head {
            return resync;
        }
        let Some(first) = self.entries.front().map(|entry| entry.update.seq.get()) else {
            return resync;
        };
        // `seen < head` and the entries run gap-free up to `head`, so any
        // `seen + 1 >= first` names a retained entry.
        let Some(skip) = (seen.get() + 1)
            .checked_sub(first)
            .and_then(|skip| usize::try_from(skip).ok())
        else {
            return resync;
        };
        if skip >= self.entries.len() {
            return resync;
        }
        Replay::Suffix(Suffix(self.entries.range(skip..)))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::Arc;

    use dal_core::{Gen, Seq, StreamChannel, TurnId, Update, UpdateKind};

    use super::{Position, PushError, Replay, ReplayRing, RingCaps};

    fn g(value: u64) -> Gen {
        Gen::new(NonZeroU64::new(value).unwrap())
    }

    fn s(value: u64) -> Seq {
        Seq::new(NonZeroU64::new(value).unwrap())
    }

    fn delta(generation: u64, seq: u64, text_len: usize) -> Arc<Update> {
        Arc::new(Update {
            r#gen: g(generation),
            seq: s(seq),
            kind: UpdateKind::Delta {
                turn: TurnId::new(NonZeroU64::MIN),
                channel: StreamChannel::Text,
                text: "x".repeat(text_len).into(),
            },
        })
    }

    fn size(update: &Update) -> usize {
        sonic_rs::to_vec(update).unwrap().len()
    }

    fn at(generation: u64, seq: Option<u64>) -> Position {
        Position {
            r#gen: g(generation),
            seq: seq.map(s),
        }
    }

    /// Replayed sequence numbers, `None` for live, or the resync position.
    fn outcome(replay: Replay<'_>) -> Result<Option<Vec<u64>>, Position> {
        match replay {
            Replay::Live => Ok(None),
            Replay::Suffix(suffix) => Ok(Some(suffix.map(|update| update.seq.get()).collect())),
            Replay::Resync(position) => Err(position),
        }
    }

    fn cursor(generation: u64, seq: u64) -> (Gen, Seq) {
        (g(generation), s(seq))
    }

    #[test]
    fn exact_update_cap_keeps_first_retained_and_exclusive_cursor() {
        let mut ring = ReplayRing::new(RingCaps::TOP_LEVEL, g(1), None);
        for seq in 1..=4096 {
            ring.push(delta(1, seq, 0)).unwrap();
        }
        assert_eq!(ring.entries.len(), 4096);
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 1)))),
            Ok(Some((2..=4096).collect()))
        );

        ring.push(delta(1, 4097, 0)).unwrap();
        ring.push(delta(1, 4098, 0)).unwrap();
        assert_eq!(ring.entries.len(), 4096);
        // Seq 2 was evicted, so a client that saw only seq 1 lost data.
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 1)))),
            Err(at(1, Some(4098)))
        );
        // A client that saw seq 2 gets the whole ring, first entry included.
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 2)))),
            Ok(Some((3..=4098).collect()))
        );
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 4097)))),
            Ok(Some(vec![4098]))
        );
        assert_eq!(outcome(ring.replay(Some(cursor(1, 4098)))), Ok(None));
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 4099)))),
            Err(at(1, Some(4098)))
        );
        assert_eq!(outcome(ring.replay(None)), Ok(None));
    }

    #[test]
    fn byte_cap_evicts_oldest_until_serialized_total_fits() {
        let updates = [
            delta(1, 1, 10),
            delta(1, 2, 20),
            delta(1, 3, 30),
            delta(1, 4, 40),
        ];
        let sizes: Vec<usize> = updates.iter().map(|update| size(update)).collect();
        let cap = sizes[0] + sizes[1] + sizes[2];
        let mut ring = ReplayRing::new(
            RingCaps {
                updates: 100,
                bytes: cap,
            },
            g(1),
            None,
        );
        for update in &updates[..3] {
            ring.push(Arc::clone(update)).unwrap();
        }
        // Exactly at the cap retains everything.
        assert_eq!(ring.bytes, cap);
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 1)))),
            Ok(Some(vec![2, 3]))
        );

        ring.push(Arc::clone(&updates[3])).unwrap();
        // Dropping seq 1 alone leaves 2+3+4 over the cap, so seq 2 goes too.
        assert!(sizes[1] + sizes[2] + sizes[3] > cap);
        assert_eq!(ring.bytes, sizes[2] + sizes[3]);
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 2)))),
            Ok(Some(vec![3, 4]))
        );
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 1)))),
            Err(at(1, Some(4)))
        );
        let retained: Vec<_> = ring.entries.iter().map(|entry| &entry.update).collect();
        assert!(Arc::ptr_eq(retained[0], &updates[2]));
        assert!(Arc::ptr_eq(retained[1], &updates[3]));
    }

    #[test]
    fn top_level_byte_cap_is_one_mebibyte() {
        let mut ring = ReplayRing::new(RingCaps::TOP_LEVEL, g(1), None);
        ring.push(delta(1, 1, 600 << 10)).unwrap();
        ring.push(delta(1, 2, 400 << 10)).unwrap();
        assert!(ring.bytes <= 1 << 20);
        assert_eq!(ring.entries.len(), 2);
        // 400 KiB + 700 KiB exceeds 1 MiB, so both older updates go.
        ring.push(delta(1, 3, 700 << 10)).unwrap();
        assert_eq!(ring.entries.len(), 1);
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 1)))),
            Err(at(1, Some(3)))
        );
        assert_eq!(outcome(ring.replay(Some(cursor(1, 2)))), Ok(Some(vec![3])));
    }

    #[test]
    fn oversize_singleton_is_retained_and_laggards_resync() {
        let mut ring = ReplayRing::new(RingCaps::TOP_LEVEL, g(1), Some(s(10)));
        ring.push(delta(1, 11, 16)).unwrap();
        ring.push(delta(1, 12, 2 << 20)).unwrap();
        assert_eq!(ring.entries.len(), 1);
        assert!(ring.bytes > RingCaps::TOP_LEVEL.bytes);
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 11)))),
            Ok(Some(vec![12]))
        );
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 10)))),
            Err(at(1, Some(12)))
        );

        ring.push(delta(1, 13, 16)).unwrap();
        assert_eq!(ring.entries.len(), 1);
        assert_eq!(ring.bytes, size(&delta(1, 13, 16)));
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 11)))),
            Err(at(1, Some(13)))
        );
    }

    #[test]
    fn open_head_replays_first_pushed_update_and_resyncs_lost_history() {
        let mut ring = ReplayRing::new(RingCaps::CHILD, g(3), Some(s(5)));
        assert_eq!(outcome(ring.replay(Some(cursor(3, 5)))), Ok(None));
        assert_eq!(
            outcome(ring.replay(Some(cursor(3, 4)))),
            Err(at(3, Some(5)))
        );
        ring.push(delta(3, 6, 1)).unwrap();
        ring.push(delta(3, 7, 1)).unwrap();
        assert_eq!(
            outcome(ring.replay(Some(cursor(3, 5)))),
            Ok(Some(vec![6, 7]))
        );
        assert_eq!(
            outcome(ring.replay(Some(cursor(3, 4)))),
            Err(at(3, Some(7)))
        );

        let empty = ReplayRing::new(RingCaps::CHILD, g(1), None);
        assert_eq!(outcome(empty.replay(None)), Ok(None));
        assert_eq!(outcome(empty.replay(Some(cursor(1, 1)))), Err(at(1, None)));
    }

    #[test]
    fn gaps_and_stale_generations_leave_ring_unchanged() {
        let mut ring = ReplayRing::new(RingCaps::CHILD, g(2), None);
        ring.push(delta(2, 1, 1)).unwrap();
        let before = ring.bytes;
        assert!(matches!(
            ring.push(delta(2, 3, 1)),
            Err(PushError::Gap { after, got }) if after == s(1) && got == s(3)
        ));
        assert!(matches!(
            ring.push(delta(2, 1, 1)),
            Err(PushError::Gap { .. })
        ));
        assert!(matches!(
            ring.push(delta(1, 2, 1)),
            Err(PushError::StaleGeneration { current, got }) if current == g(2) && got == g(1)
        ));
        assert_eq!(ring.position(), at(2, Some(1)));
        assert_eq!(ring.entries.len(), 1);
        assert_eq!(ring.bytes, before);
        ring.push(delta(2, 2, 1)).unwrap();
        assert_eq!(outcome(ring.replay(Some(cursor(2, 1)))), Ok(Some(vec![2])));
    }

    #[test]
    fn generation_rollover_clears_window_and_resyncs_old_cursors() {
        let mut ring = ReplayRing::new(RingCaps::TOP_LEVEL, g(1), None);
        for seq in 1..=3 {
            ring.push(delta(1, seq, 1)).unwrap();
        }
        ring.push(delta(2, 1, 1)).unwrap();
        assert_eq!(ring.entries.len(), 1);
        assert_eq!(ring.bytes, size(&delta(2, 1, 1)));
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 3)))),
            Err(at(2, Some(1)))
        );
        assert_eq!(
            outcome(ring.replay(Some(cursor(1, 1)))),
            Err(at(2, Some(1)))
        );
        assert_eq!(
            outcome(ring.replay(Some(cursor(3, 1)))),
            Err(at(2, Some(1)))
        );
        assert_eq!(outcome(ring.replay(Some(cursor(2, 1)))), Ok(None));
        assert_eq!(
            outcome(ring.replay(Some(cursor(2, 5)))),
            Err(at(2, Some(1)))
        );
        ring.push(delta(2, 2, 1)).unwrap();
        assert_eq!(outcome(ring.replay(Some(cursor(2, 1)))), Ok(Some(vec![2])));
        assert!(matches!(
            ring.push(delta(2, 2, 1)),
            Err(PushError::Gap { .. })
        ));
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }

        fn below_usize(&mut self, bound: usize) -> usize {
            usize::try_from(self.below(u64::try_from(bound).unwrap())).unwrap()
        }
    }

    /// The oracle's view of one generation: every update pushed in it.
    struct Oracle {
        generation: u64,
        start: Option<u64>,
        pushed: Vec<(Arc<Update>, usize)>,
    }

    impl Oracle {
        fn head(&self) -> Option<u64> {
            self.pushed
                .last()
                .map(|(update, _)| update.seq.get())
                .or(self.start)
        }

        /// Index of the first update in the longest suffix within the caps,
        /// never excluding the newest update.
        fn window(&self, caps: RingCaps) -> usize {
            let mut first = self.pushed.len();
            let mut bytes = 0;
            for (index, (_, size)) in self.pushed.iter().enumerate().rev() {
                let count = self.pushed.len() - index;
                if count > 1 && (count > caps.updates || bytes + size > caps.bytes) {
                    break;
                }
                bytes += size;
                first = index;
            }
            first
        }

        fn position(&self) -> Position {
            at(self.generation, self.head())
        }

        fn expected(
            &self,
            caps: RingCaps,
            after: Option<(u64, u64)>,
        ) -> Result<Option<Vec<u64>>, Position> {
            let Some((generation, seen)) = after else {
                return Ok(None);
            };
            let head = self.head();
            if generation != self.generation || head.is_none_or(|head| seen > head) {
                return Err(self.position());
            }
            if Some(seen) == head {
                return Ok(None);
            }
            let window = &self.pushed[self.window(caps)..];
            match window.first() {
                Some((first, _)) if seen + 1 >= first.seq.get() => Ok(Some(
                    window
                        .iter()
                        .map(|(entry, _)| entry.seq.get())
                        .filter(|seq| *seq > seen)
                        .collect(),
                )),
                _ => Err(self.position()),
            }
        }
    }

    fn check(ring: &ReplayRing, oracle: &Oracle, random: &mut Rng) {
        let caps = ring.caps;
        let window = &oracle.pushed[oracle.window(caps)..];
        assert_eq!(ring.entries.len(), window.len());
        assert_eq!(
            ring.bytes,
            window.iter().map(|(_, size)| size).sum::<usize>()
        );
        for (entry, (update, _)) in ring.entries.iter().zip(window) {
            assert!(Arc::ptr_eq(&entry.update, update));
        }
        if ring.entries.len() > 1 {
            assert!(ring.entries.len() <= caps.updates && ring.bytes <= caps.bytes);
        }
        assert_eq!(ring.position(), oracle.position());

        let head = oracle.head().unwrap_or(0);
        for _ in 0..8 {
            let after = match random.below(6) {
                0 => None,
                1 => Some((oracle.generation + 1, 1 + random.below(head + 2))),
                2 if oracle.generation > 1 => {
                    Some((oracle.generation - 1, 1 + random.below(head + 2)))
                }
                3 => Some((oracle.generation, head.max(1))),
                _ => Some((oracle.generation, 1 + random.below(head + 2))),
            };
            let replay = ring.replay(after.map(|(generation, seq)| (g(generation), s(seq))));
            let expected = oracle.expected(caps, after);
            if let Replay::Suffix(suffix) = &replay {
                let seen = after.map_or(0, |(_, seq)| seq);
                let expected_first = oracle
                    .pushed
                    .iter()
                    .find(|(update, _)| update.seq.get() == seen + 1);
                let first = suffix.clone().next();
                assert!(matches!(
                    (first, expected_first),
                    (Some(got), Some((want, _))) if Arc::ptr_eq(got, want)
                ));
            }
            assert_eq!(outcome(replay), expected, "cursor {after:?}");
        }
    }

    #[test]
    fn replay_ring_property() {
        let mut random = Rng(0x9e37_79b9_7f4a_7c15);
        for case in 0..12 {
            let caps = match case % 3 {
                0 => RingCaps::TOP_LEVEL,
                1 => RingCaps::CHILD,
                _ => RingCaps {
                    updates: 1 + random.below_usize(64),
                    bytes: 200 + random.below_usize(4000),
                },
            };
            let length = random.below(10_001);
            let start = if random.below(3) == 0 {
                None
            } else {
                Some(1 + random.below(1000))
            };
            let mut oracle = Oracle {
                generation: 1 + random.below(3),
                start,
                pushed: Vec::new(),
            };
            let mut ring = ReplayRing::new(caps, g(oracle.generation), start.map(s));
            check(&ring, &oracle, &mut random);
            for step in 0..length {
                if random.below(2000) == 0 {
                    let restart = oracle.head();
                    oracle = Oracle {
                        generation: oracle.generation + 1,
                        start: None,
                        pushed: Vec::new(),
                    };
                    // A new generation may restart or continue the counter.
                    if random.below(2) == 0 {
                        oracle.start = restart;
                    }
                }
                let seq = oracle.head().map_or(1 + random.below(5), |head| head + 1);
                let text_len = if random.below(500) == 0 {
                    (64 << 10) + random.below_usize(1 << 20)
                } else {
                    random.below_usize(700)
                };
                let update = delta(oracle.generation, seq, text_len);
                let bytes = size(&update);
                ring.push(Arc::clone(&update)).unwrap();
                if oracle.pushed.is_empty() {
                    // The first push of a generation fixes its window start.
                    oracle.start = Some(seq - 1).filter(|start| *start > 0);
                }
                oracle.pushed.push((update, bytes));
                if step % 97 == 0 || step + 1 == length {
                    check(&ring, &oracle, &mut random);
                }
            }
        }
    }
}
