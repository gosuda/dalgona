//! Shared resource limits for a single provider streaming reply.
//!
//! Each decoder charges event bytes before parsing or extending retained
//! state and counts new calls, blocks, and pending backfill identifiers.
//! HTTPS/SSE and Responses/Codex `WebSockets` share these controls.

use crate::error::{LimitError, ProviderError};

/// Cumulative original JSON bytes admitted to one provider decoder.
pub(crate) const STREAM_BYTES_LIMIT: usize = 128 << 20;

/// Maximum dispatched events per provider reply, including ignored events.
pub(crate) const STREAM_EVENTS_LIMIT: usize = 65_536;

/// Maximum retained calls, blocks, and pending backfill identifiers per reply.
pub(crate) const STREAM_ITEMS_LIMIT: usize = 16_384;

#[derive(Debug, Default)]
pub(crate) struct StreamBudget {
    bytes: usize,
    events: usize,
    items: usize,
}

impl StreamBudget {
    /// Charge an event before parsing or growing retained state.
    pub(crate) fn admit_event(&mut self, bytes: usize) -> Result<(), ProviderError> {
        if self.events == STREAM_EVENTS_LIMIT {
            return Err(ProviderError::Limit(LimitError::StreamEvents));
        }
        if bytes > STREAM_BYTES_LIMIT - self.bytes {
            return Err(ProviderError::Limit(LimitError::StreamBytes));
        }
        self.events += 1;
        self.bytes += bytes;
        Ok(())
    }

    /// Charge a new persistent record before inserting it.
    pub(crate) fn admit_item(&mut self) -> Result<(), ProviderError> {
        if self.items == STREAM_ITEMS_LIMIT {
            return Err(ProviderError::Limit(LimitError::StreamItems));
        }
        self.items += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_for_cumulative_bytes_and_events_before_growth() {
        let mut budget = StreamBudget::default();
        for _ in 0..STREAM_BYTES_LIMIT / (8 << 10) {
            assert!(budget.admit_event(8 << 10).is_ok());
        }
        assert!(matches!(
            budget.admit_event(1),
            Err(ProviderError::Limit(LimitError::StreamBytes))
        ));
        let mut budget = StreamBudget::default();
        for _ in 0..STREAM_EVENTS_LIMIT {
            assert!(budget.admit_event(1).is_ok());
        }
        assert!(matches!(
            budget.admit_event(1),
            Err(ProviderError::Limit(LimitError::StreamEvents))
        ));
    }

    #[test]
    fn bounds_the_number_of_retained_items() {
        let mut budget = StreamBudget::default();
        for _ in 0..STREAM_ITEMS_LIMIT {
            assert!(budget.admit_item().is_ok());
        }
        assert!(matches!(
            budget.admit_item(),
            Err(ProviderError::Limit(LimitError::StreamItems))
        ));
    }
}
