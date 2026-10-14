use std::num::NonZeroU64;

use crate::TurnId;

/// Allocation and execution order for one session's durable turn identities.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TurnSequence {
    allocated: u64,
    started: u64,
}

/// A journal turn identity contradicts its durable sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TurnSequenceError {
    /// Allocation skipped or reused an identity, or exhausted the counter.
    #[error("turn {turn} is not the next allocation after {allocated}")]
    Allocation {
        /// The invalid identity.
        turn: TurnId,
        /// The last allocated identity, or zero before the first allocation.
        allocated: u64,
    },
    /// Execution reused an identity or moved backwards.
    #[error("turn {turn} does not follow started turn {started}")]
    Start {
        /// The invalid identity.
        turn: TurnId,
        /// The last started or expired identity.
        started: u64,
    },
}

impl TurnSequence {
    /// Returns the next unused identity, or none when the counter is exhausted.
    #[must_use]
    pub fn next(&self) -> Option<TurnId> {
        self.allocated
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(TurnId::new)
    }

    /// Records a consecutive allocation without starting execution.
    ///
    /// # Errors
    /// Rejects a gap, a repeated identity, or counter exhaustion.
    pub fn reserve(&mut self, turn: TurnId) -> Result<(), TurnSequenceError> {
        if self.next() != Some(turn) {
            return Err(TurnSequenceError::Allocation {
                turn,
                allocated: self.allocated,
            });
        }
        self.allocated = turn.get();
        Ok(())
    }

    /// Starts a reserved identity in increasing order.
    ///
    /// An exact-next start also allocates its identity for journals written
    /// before explicit reservations. No unrecorded gap is accepted.
    ///
    /// # Errors
    /// Rejects repeated or decreasing starts and unallocated gaps.
    pub fn start(&mut self, turn: TurnId) -> Result<(), TurnSequenceError> {
        if turn.get() <= self.started {
            return Err(TurnSequenceError::Start {
                turn,
                started: self.started,
            });
        }
        if turn.get() > self.allocated {
            self.reserve(turn)?;
        }
        self.started = turn.get();
        Ok(())
    }

    /// Prevents an unstarted reservation from a previous boot from executing.
    /// The allocation counter remains unchanged.
    pub const fn expire_pending(&mut self) {
        self.started = self.allocated;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(value: u64) -> Result<TurnId, Box<dyn std::error::Error>> {
        Ok(TurnId::new(NonZeroU64::new(value).ok_or("zero turn")?))
    }

    #[test]
    fn reservations_account_for_skipped_starts_without_allowing_gaps_or_reuse()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut sequence = TurnSequence::default();
        sequence.start(turn(1)?)?;
        sequence.reserve(turn(2)?)?;
        sequence.reserve(turn(3)?)?;
        sequence.start(turn(3)?)?;
        let saved = sequence;
        assert!(sequence.start(turn(2)?).is_err());
        assert!(sequence.start(turn(3)?).is_err());
        assert!(sequence.reserve(turn(5)?).is_err());
        assert!(sequence.start(turn(5)?).is_err());
        assert_eq!(sequence, saved);
        sequence.reserve(turn(4)?)?;
        sequence.expire_pending();
        assert!(sequence.start(turn(4)?).is_err());
        assert_eq!(sequence.next(), Some(turn(5)?));
        Ok(())
    }

    #[test]
    fn exhausted_allocation_still_allows_its_reserved_start()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut sequence = TurnSequence {
            allocated: u64::MAX - 1,
            started: u64::MAX - 1,
        };
        sequence.reserve(turn(u64::MAX)?)?;
        assert_eq!(sequence.next(), None);
        sequence.start(turn(u64::MAX)?)?;
        assert!(sequence.reserve(turn(1)?).is_err());
        assert!(sequence.start(turn(u64::MAX)?).is_err());
        Ok(())
    }
}
