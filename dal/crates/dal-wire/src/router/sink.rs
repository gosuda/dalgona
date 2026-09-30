//! Turn event sinks: discard for non-stream replies, SSE for live streams.
//!
//! A sink reports `false` once its client is gone; the harness then cancels
//! the turn and a relay drops its provider stream.

use tokio::sync::mpsc;

use super::harness::{HarnessEvent, UsageSum};
use super::stream::{ChatEncoder, MessagesEncoder, ResponsesEncoder};

/// Receives live harness turn events.
pub(crate) trait EventSink: Send {
    /// Delivers one event; returns `false` when the client has gone away.
    fn emit(&mut self, event: HarnessEvent) -> impl Future<Output = bool> + Send;
}

/// A sink for non-stream replies; the reply is built from the finished turn.
pub(crate) struct Discard;

impl EventSink for Discard {
    fn emit(&mut self, _event: HarnessEvent) -> impl Future<Output = bool> + Send {
        std::future::ready(true)
    }
}

/// One family's SSE encoder.
pub(crate) trait SseEncoder: Send + Sync {
    /// Encodes one event with the running usage totals.
    fn feed(&mut self, event: &HarnessEvent, usage: &UsageSum);
    /// Returns the encoded lines not yet sent.
    fn pending(&mut self) -> &mut Vec<String>;
}

impl SseEncoder for ChatEncoder {
    fn feed(&mut self, event: &HarnessEvent, usage: &UsageSum) {
        ChatEncoder::feed(self, event, usage);
    }

    fn pending(&mut self) -> &mut Vec<String> {
        &mut self.lines
    }
}

impl SseEncoder for ResponsesEncoder {
    fn feed(&mut self, event: &HarnessEvent, usage: &UsageSum) {
        ResponsesEncoder::feed(self, event, usage);
    }

    fn pending(&mut self) -> &mut Vec<String> {
        &mut self.lines
    }
}

impl SseEncoder for MessagesEncoder {
    fn feed(&mut self, event: &HarnessEvent, usage: &UsageSum) {
        MessagesEncoder::feed(self, event, usage);
    }

    fn pending(&mut self) -> &mut Vec<String> {
        &mut self.lines
    }
}

/// Encodes events as SSE lines into one live response channel.
pub(crate) struct StreamSink<E> {
    encoder: E,
    usage: UsageSum,
    tx: mpsc::Sender<Option<String>>,
}

impl<E: SseEncoder> StreamSink<E> {
    /// Creates a sink writing `encoder` output into `tx`.
    pub(crate) fn new(encoder: E, tx: mpsc::Sender<Option<String>>) -> Self {
        Self {
            encoder,
            usage: UsageSum::default(),
            tx,
        }
    }

    /// Resolves once the client side of the channel is gone.
    pub(crate) async fn closed(&self) {
        self.tx.closed().await;
    }

    /// Encodes one event and queues its lines without waiting; lines that do
    /// not fit are dropped.
    pub(crate) fn emit_now(&mut self, event: &HarnessEvent) {
        self.encode(event);
        for line in self.encoder.pending().drain(..) {
            if self.tx.try_send(Some(line)).is_err() {
                tracing::debug!("router stream had no room for its final event");
                return;
            }
        }
    }

    /// Feeds one event to the encoder, tracking usage totals.
    fn encode(&mut self, event: &HarnessEvent) {
        if let HarnessEvent::Usage(sum) = event {
            self.usage = sum.clone();
        }
        self.encoder.feed(event, &self.usage);
    }
}

impl<E: SseEncoder> EventSink for StreamSink<E> {
    async fn emit(&mut self, event: HarnessEvent) -> bool {
        self.encode(&event);
        for line in self.encoder.pending().drain(..) {
            if self.tx.send(Some(line)).await.is_err() {
                return false;
            }
        }
        true
    }
}
