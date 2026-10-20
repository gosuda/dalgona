//! The exactly-one-terminal invariant of a Devin turn.
//!
//! A turn ends exactly once: one `Stop` event on success, one error on
//! failure. [`Terminal`] is the latch that enforces it. The decoder consults
//! [`Terminal::is_finished`] before touching any input, records every emitted
//! event with [`Terminal::note_events`], and closes the turn with
//! [`Terminal::finish_ok`] or [`Terminal::finish_err`]. After the latch
//! closes, further frames and further `end` calls are silent no-ops, so a
//! server that keeps talking past its own trailer can never produce a second
//! terminal. The delivered count backs the retry table: a failure after the
//! first emitted event is final, never retried.

/// Latch and event counter of one turn's terminal state.
#[derive(Debug, Default)]
pub(crate) struct Terminal {
    /// The terminal was already emitted or failed.
    finished: bool,
    /// Stream events emitted before the terminal.
    delivered: u64,
}

impl Terminal {
    /// An open turn.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether the turn already ended; late input must be ignored.
    #[must_use]
    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    /// Records `count` newly emitted events for the retry table.
    pub(crate) fn note_events(&mut self, count: usize) {
        self.delivered = self.delivered.saturating_add(count as u64);
    }

    /// Whether any event reached the consumer before the terminal.
    #[must_use]
    pub(crate) fn delivered(&self) -> bool {
        self.delivered > 0
    }

    /// Closes the turn on success.
    pub(crate) fn finish_ok(&mut self) {
        self.finished = true;
    }

    /// Closes the turn on failure.
    pub(crate) fn finish_err(&mut self) {
        self.finished = true;
    }
}
