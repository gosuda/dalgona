//! Per-turn limits for untrusted provider event streams.
//!
//! Every decoder charges the full event before parsing it. The cumulative
//! payload-byte and event limits bound all text, arguments, replay data, and
//! collection overhead, even when every individual frame is valid. The item
//! limit separately bounds persistent entries opened by provider-supplied ids.
//! Time is enforced at the transport boundary and does not reset on activity.

use std::{pin::Pin, time::Duration};

use futures::{Stream, StreamExt, stream};
use tokio::time::{Instant, timeout_at};

use crate::{
    error::{LimitError, ProviderError},
    stream::StreamEvent,
};

pub(crate) const STREAM_BYTES_LIMIT: usize = 32 << 20;
pub(crate) const STREAM_EVENTS_LIMIT: usize = 65_536;
pub(crate) const STREAM_ITEMS_LIMIT: usize = 4_096;
pub(crate) const STREAM_TOTAL_TIMEOUT: Duration = Duration::from_mins(15);

#[derive(Debug)]
pub(crate) struct StreamBudget {
    bytes_left: usize,
    events_left: usize,
    items_left: usize,
}

impl Default for StreamBudget {
    fn default() -> Self {
        Self {
            bytes_left: STREAM_BYTES_LIMIT,
            events_left: STREAM_EVENTS_LIMIT,
            items_left: STREAM_ITEMS_LIMIT,
        }
    }
}

impl StreamBudget {
    /// Charges before a decoder parses or retains the event.
    pub(crate) fn event(&mut self, bytes: usize) -> Result<(), ProviderError> {
        if bytes > self.bytes_left {
            return Err(ProviderError::Limit(LimitError::StreamBytes));
        }
        if self.events_left == 0 {
            return Err(ProviderError::Limit(LimitError::StreamEvents));
        }
        self.bytes_left -= bytes;
        self.events_left -= 1;
        Ok(())
    }

    /// Charges before creating persistent calls, blocks, or backfill entries.
    pub(crate) fn items(&mut self, count: usize) -> Result<(), ProviderError> {
        if count > self.items_left {
            return Err(ProviderError::Limit(LimitError::StreamItems));
        }
        self.items_left -= count;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) const fn with_limits(bytes: usize, events: usize, items: usize) -> Self {
        Self {
            bytes_left: bytes,
            events_left: events,
            items_left: items,
        }
    }
}

type Source = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

/// Ends an SSE decoding stream at a fixed deadline, even if it stays active.
/// A failure drops the source before the next poll.
pub(crate) fn with_deadline(
    source: Source,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send {
    with_timeout(source, STREAM_TOTAL_TIMEOUT)
}

fn with_timeout(
    source: Source,
    total: Duration,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send {
    let deadline = Instant::now() + total;
    stream::unfold((Some(source), deadline), |(mut source, deadline)| async move {
        let input = source.as_mut()?;
        let result = timeout_at(deadline, input.next()).await;
        match result {
            Ok(Some(Ok(event))) => Some((Ok(event), (source, deadline))),
            Ok(Some(Err(error))) => Some((Err(error), (None, deadline))),
            Ok(None) => None,
            Err(_) => Some((
                Err(ProviderError::Limit(LimitError::StreamDuration)),
                (None, deadline),
            )),
        }
    })
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;

    #[test]
    fn cumulative_budgets_accept_boundaries_and_reject_next_units() {
        let mut bytes = StreamBudget::with_limits(4, 3, 2);
        bytes.event(2).unwrap();
        bytes.event(2).unwrap();
        assert!(matches!(
            bytes.event(1),
            Err(ProviderError::Limit(LimitError::StreamBytes))
        ));

        let mut events = StreamBudget::with_limits(10, 2, 2);
        events.event(0).unwrap();
        events.event(0).unwrap();
        assert!(matches!(
            events.event(0),
            Err(ProviderError::Limit(LimitError::StreamEvents))
        ));

        let mut items = StreamBudget::with_limits(10, 2, 2);
        items.items(2).unwrap();
        assert!(matches!(
            items.items(1),
            Err(ProviderError::Limit(LimitError::StreamItems))
        ));
    }

    #[tokio::test]
    async fn a_busy_or_stalled_source_expires_at_its_total_deadline() {
        let source: Source = Box::pin(futures::stream::pending());
        let mut events = Box::pin(with_timeout(source, Duration::from_millis(5)));
        assert!(matches!(
            events.next().await,
            Some(Err(ProviderError::Limit(LimitError::StreamDuration)))
        ));
        assert!(events.next().await.is_none());
    }
}
