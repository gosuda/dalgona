//! Shared resource limits for a single provider streaming reply.
//!
//! Each decoder charges event bytes before parsing or extending retained
//! state and counts new calls, blocks, and pending backfill identifiers.
//! HTTPS/SSE and Responses/Codex `WebSockets` share these controls.

use std::{pin::Pin, time::Duration};

use futures::{Stream, StreamExt, stream};
use tokio::time::{Instant, timeout_at};

use crate::{
    error::{LimitError, ProviderError},
    stream::StreamEvent,
};

/// Cumulative original JSON bytes admitted to one provider decoder.
pub(crate) const STREAM_BYTES_LIMIT: usize = 32 << 20;

/// Maximum dispatched events per provider reply, including ignored events.
pub(crate) const STREAM_EVENTS_LIMIT: usize = 8192;

/// Maximum retained calls, blocks, and pending backfill identifiers per reply.
pub(crate) const STREAM_ITEMS_LIMIT: usize = 4096;

/// Maximum wall-clock lifetime of one streaming reply.
pub(crate) const STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(600);

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

/// Bound the response lifetime even when the peer sends keepalives.
/// An error drops the source, releasing its socket and admission guard.
pub(crate) fn timeboxed<S>(
    source: S,
    deadline: Instant,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static,
{
    stream::unfold(
        Some(Box::pin(source)),
        move |source: Option<Pin<Box<S>>>| async move {
            let mut source = source?;
            match timeout_at(deadline, source.next()).await {
                Ok(Some(Ok(event))) => {
                    let next = if matches!(&event, StreamEvent::Stop { .. }) {
                        None
                    } else {
                        Some(source)
                    };
                    Some((Ok(event), next))
                }
                Ok(Some(Err(error))) => Some((Err(error), None)),
                Ok(None) => None,
                Err(_) => Some((Err(ProviderError::Limit(LimitError::StreamDuration)), None)),
            }
        },
    )
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

    #[tokio::test]
    async fn deadline_interrupts_a_stalled_stream() {
        let stalled = stream::pending::<Result<StreamEvent, ProviderError>>();
        let mut source = Box::pin(timeboxed(stalled, Instant::now()));
        assert!(matches!(
            source.next().await,
            Some(Err(ProviderError::Limit(LimitError::StreamDuration)))
        ));
        assert!(source.next().await.is_none());
    }
}
