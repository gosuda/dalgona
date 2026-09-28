//! The neutral provider event stream every family decoder emits.
//!
//! [`StreamEvent`] is the decoding-level vocabulary the agent loop consumes
//! and maps onto the recorded core protocol events. Its grammar is: any
//! number of text, reasoning, call-start, and argument-fragment events
//! interleaved with [`StreamEvent::Replay`] events, then exactly one
//! [`StreamEvent::ToolCallsDone`] (possibly empty), exactly one
//! [`StreamEvent::Usage`], and exactly one [`StreamEvent::Stop`]. A
//! [`ProviderError`] ends the stream instead of `Stop`. Decoders own that
//! order by construction.
//!
//! [`EventStream`] owns one decoder source and one cancellation action and
//! guards the terminal: `Stop` or an error is delivered once, nothing is
//! delivered after it, and a source that ends without a terminal yields
//! [`ProviderError::StreamCut`]. The cancellation action runs at most once:
//! when the stream ends in an error or a cut, or when the consumer drops the
//! stream before its terminal. A clean `Stop` drops the action uncalled, so a
//! transport may keep a healthy connection for reuse.
//!
//! This module has no clock and no socket. The transport adapter that builds
//! the source and the action owns the real-time bound on closing the
//! connection after cancellation; nothing here enforces or tests it.
//!
//! The stream carries no journal, no session, and no host reference.

use std::{
    fmt,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use dal_core::{Family, RawJson, Usage};
use futures::{FutureExt, Stream, StreamExt, future::poll_fn};

use crate::error::ProviderError;

/// One decoded provider stream event.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    /// Visible assistant text.
    TextDelta {
        /// The next text slice.
        text: String,
    },
    /// Reasoning or thinking text.
    ReasoningDelta {
        /// The next reasoning slice.
        text: String,
    },
    /// A tool call began.
    ToolCallStarted {
        /// The provider call id.
        id: String,
        /// The tool name.
        name: String,
    },
    /// Raw argument bytes of a started tool call.
    ToolArgsDelta {
        /// The provider call id.
        id: String,
        /// Argument bytes exactly as received; a fragment may split a UTF-8
        /// character or a JSON token.
        fragment: Vec<u8>,
    },
    /// An opaque item the same family and model must receive back verbatim.
    Replay {
        /// The item and the route it is bound to.
        payload: ReplayPayload,
    },
    /// Every tool call of the response, complete; may be empty.
    ToolCallsDone {
        /// The calls in response order.
        calls: Vec<ToolCall>,
    },
    /// The normalized token usage of the response.
    Usage {
        /// The counters; zero when the server reported none.
        usage: Usage,
    },
    /// The response completed; the terminal success event.
    Stop {
        /// Why generation stopped.
        reason: StopReason,
    },
}

impl StreamEvent {
    const fn is_terminal(&self) -> bool {
        matches!(self, Self::Stop { .. })
    }
}

/// One complete tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// The provider call id.
    pub id: String,
    /// The tool name.
    pub name: String,
    /// The assembled arguments.
    pub args: ToolArgs,
}

/// The arguments of one complete tool call.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolArgs {
    /// One valid JSON value, kept byte for byte.
    Parsed(RawJson),
    /// The assembled bytes are not one JSON value.
    Invalid {
        /// Why the bytes were rejected.
        message: String,
    },
    /// The response ended before the arguments were complete.
    Truncated,
}

impl ToolArgs {
    /// Validates the concatenated argument fragments of a completed call.
    ///
    /// The bytes are never re-encoded: a valid value keeps its number
    /// spellings, member order, and interior whitespace. Only whitespace
    /// around the value is dropped. Empty input, invalid UTF-8, and anything
    /// other than exactly one JSON value give [`ToolArgs::Invalid`]; the
    /// decoder chooses [`ToolArgs::Truncated`] itself when the response was
    /// cut, since truncation is a property of the stream, not of the bytes.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(text) => match RawJson::parse(text) {
                Ok(raw) => Self::Parsed(raw),
                Err(error) => Self::Invalid {
                    message: error.to_string(),
                },
            },
            Err(error) => Self::Invalid {
                message: format!("arguments are not valid UTF-8: {error}"),
            },
        }
    }
}

/// Why generation stopped.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum StopReason {
    /// The model finished its turn.
    #[default]
    EndTurn,
    /// The model requested tool calls.
    ToolUse,
    /// The output token limit was reached.
    MaxTokens,
    /// The model refused.
    Refusal,
    /// The server paused the turn.
    Paused,
    /// A stop reason this crate has no name for, verbatim.
    Other(String),
}

/// An opaque provider item bound to the family and model that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayPayload {
    /// The family whose wire format the item uses.
    pub family: Family,
    /// The model id that produced the item.
    pub model: Box<str>,
    /// The item exactly as received.
    pub item: RawJson,
}

/// The per-turn sink for clamp, temperature, retry, and fallback notices.
///
/// Notices never travel as stream events.
pub type NoticeSink = Arc<dyn Fn(String) + Send + Sync>;

type Source = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;
type Cancel = Box<dyn FnOnce() + Send>;

/// An owned, boxed provider event source with a terminal guard and a
/// cancellation action.
///
/// [`EventStream::next`] yields events in source order and delivers exactly
/// one terminal: `Stop`, an error, or [`ProviderError::StreamCut`] when the
/// source ends without either. After the terminal it yields `None`. An error
/// or a cut drops the source at once, so nothing can follow it. After `Stop`
/// the source is kept until the next call, which checks it without waiting:
/// an event already produced after `Stop` violates the grammar, and debug
/// builds panic while release builds drop the late event.
///
/// The cancellation action runs at most once, always after the source is
/// dropped: when the stream ends in an error or a cut, or when the stream is
/// dropped before its terminal. After `Stop` the action is dropped without
/// being called.
#[must_use = "dropping an EventStream before its terminal cancels the transport"]
pub struct EventStream {
    /// `None` once the stream yields nothing more.
    source: Option<Source>,
    cancel: Option<Cancel>,
    /// `Stop` was delivered and the kept source awaits the late-event check.
    stopped: bool,
}

impl EventStream {
    /// Wraps a decoder source and the action that cancels its transport.
    ///
    /// `cancel` should mark any reusable connection closed and abort what the
    /// source drop does not; it runs after the source is dropped and must not
    /// block.
    pub fn new(
        source: impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static,
        cancel: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            source: Some(Box::pin(source)),
            cancel: Some(Box::new(cancel)),
            stopped: false,
        }
    }

    /// Yields the next event, or `None` once the terminal was delivered.
    ///
    /// # Panics
    /// In debug builds, when the source produced an event after `Stop`.
    pub async fn next(&mut self) -> Option<Result<StreamEvent, ProviderError>> {
        poll_fn(|cx| self.poll_event(cx)).await
    }

    fn poll_event(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<StreamEvent, ProviderError>>> {
        let Some(source) = self.source.as_mut() else {
            return Poll::Ready(None);
        };
        if self.stopped {
            // Only an already-ready item counts; a source that stays pending
            // after `Stop` is never awaited.
            let late = source.next().now_or_never().flatten();
            debug_assert!(
                late.is_none(),
                "provider stream yielded {late:?} after its terminal event"
            );
            self.source = None;
            return Poll::Ready(None);
        }
        let item = match source.as_mut().poll_next(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(item) => item,
        };
        match item {
            Some(Ok(event)) if event.is_terminal() => {
                self.stopped = true;
                self.cancel = None;
                Poll::Ready(Some(Ok(event)))
            }
            Some(Ok(event)) => Poll::Ready(Some(Ok(event))),
            Some(Err(error)) => {
                self.abort();
                Poll::Ready(Some(Err(error)))
            }
            None => {
                self.abort();
                Poll::Ready(Some(Err(ProviderError::StreamCut)))
            }
        }
    }

    /// Drops the source, then runs the cancellation action if still armed.
    fn abort(&mut self) {
        self.source = None;
        if let Some(cancel) = self.cancel.take() {
            cancel();
        }
    }
}

impl fmt::Debug for EventStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventStream")
            .field("open", &(self.source.is_some() && !self.stopped))
            .field("cancel_armed", &self.cancel.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use futures::{channel::mpsc, executor::block_on, stream};

    use super::*;

    fn text(text: &str) -> StreamEvent {
        StreamEvent::TextDelta { text: text.into() }
    }

    fn stop() -> StreamEvent {
        StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }
    }

    fn zero_usage() -> StreamEvent {
        StreamEvent::Usage {
            usage: Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
            },
        }
    }

    fn counter() -> (Arc<AtomicUsize>, impl FnOnce() + Send + 'static) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        (count, move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn from_items(
        items: Vec<Result<StreamEvent, ProviderError>>,
    ) -> (EventStream, Arc<AtomicUsize>) {
        let (count, cancel) = counter();
        (EventStream::new(stream::iter(items), cancel), count)
    }

    #[test]
    fn async_producer_is_delivered_in_order_and_stop_keeps_the_transport() {
        let (sender, receiver) = mpsc::unbounded();
        let (count, cancel) = counter();
        let mut events = EventStream::new(receiver, cancel);
        let script = vec![
            text("Hel"),
            text("lo"),
            StreamEvent::ToolCallsDone { calls: Vec::new() },
            zero_usage(),
            stop(),
        ];
        let producer = {
            let script = script.clone();
            async move {
                for event in script {
                    // Yield between sends so the consumer observes Pending;
                    // the self-wake keeps the single-threaded executor going.
                    let mut yielded = false;
                    poll_fn(|cx| {
                        if yielded {
                            return Poll::Ready(());
                        }
                        yielded = true;
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    })
                    .await;
                    sender.unbounded_send(Ok(event)).unwrap();
                }
                // The sender stays alive: a healthy connection after Stop.
                sender
            }
        };
        let consumer = async {
            let mut seen = Vec::new();
            while let Some(item) = events.next().await {
                seen.push(item.unwrap());
            }
            seen
        };
        let (_sender, seen) = block_on(async { futures::join!(producer, consumer) });
        assert_eq!(seen, script);
        drop(events);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "after its terminal event"))]
    fn second_stop_is_never_delivered() {
        let (mut events, _count) = from_items(vec![Ok(stop()), Ok(stop())]);
        block_on(async {
            assert_eq!(events.next().await.unwrap().unwrap(), stop());
            assert!(events.next().await.is_none());
            assert!(events.next().await.is_none());
        });
    }

    #[test]
    fn stop_after_error_is_unreachable() {
        // An error drops the source at once, so a second terminal queued
        // behind it is never polled, in debug and release alike.
        let (mut events, count) = from_items(vec![Err(ProviderError::Overloaded), Ok(stop())]);
        block_on(async {
            assert!(matches!(events.next().await, Some(Err(ProviderError::Overloaded))));
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert!(events.next().await.is_none());
            assert!(events.next().await.is_none());
        });
        drop(events);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn error_is_the_terminal_and_cancels_once() {
        let (mut events, count) = from_items(vec![
            Ok(text("a")),
            Err(ProviderError::Protocol {
                family: Family::Anthropic,
                detail: "content block index went backwards".into(),
            }),
        ]);
        block_on(async {
            assert_eq!(events.next().await.unwrap().unwrap(), text("a"));
            assert_eq!(count.load(Ordering::SeqCst), 0);
            let error = events.next().await.unwrap().unwrap_err();
            assert!(matches!(error, ProviderError::Protocol { .. }));
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert!(events.next().await.is_none());
            assert!(events.next().await.is_none());
        });
        drop(events);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn source_end_without_terminal_is_a_stream_cut() {
        let (mut events, count) = from_items(vec![Ok(text("a"))]);
        block_on(async {
            assert_eq!(events.next().await.unwrap().unwrap(), text("a"));
            assert!(matches!(events.next().await, Some(Err(ProviderError::StreamCut))));
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert!(events.next().await.is_none());
        });
        drop(events);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dropping_before_the_terminal_drops_the_source_then_cancels_once() {
        struct Socket(Arc<AtomicBool>);
        impl Drop for Socket {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let closed = Arc::new(AtomicBool::new(false));
        let socket = Socket(Arc::clone(&closed));
        let stalled = stream::poll_fn(move |_| {
            let _open = &socket;
            Poll::<Option<Result<StreamEvent, ProviderError>>>::Pending
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let closed_first = Arc::new(AtomicBool::new(false));
        let cancel = {
            let calls = Arc::clone(&calls);
            let closed = Arc::clone(&closed);
            let closed_first = Arc::clone(&closed_first);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                closed_first.store(closed.load(Ordering::SeqCst), Ordering::SeqCst);
            }
        };
        let mut events = EventStream::new(stream::iter([Ok(text("first"))]).chain(stalled), cancel);
        assert_eq!(block_on(events.next()).unwrap().unwrap(), text("first"));
        assert!(events.next().now_or_never().is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(events);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(closed_first.load(Ordering::SeqCst));
    }

    #[test]
    fn argument_fragments_reassemble_byte_for_byte() {
        let args = "{ \"b\" : 1.50E+2, \"a\":[ ], \"s\":\"caf\u{e9}\" }";
        let bytes = args.as_bytes();
        // Split inside the two-byte UTF-8 sequence of the accented letter.
        let split = args.find('\u{e9}').unwrap() + 1;
        let fragments = [&bytes[..7], &bytes[7..split], &bytes[split..]];
        let mut items: Vec<_> = fragments
            .iter()
            .map(|fragment| {
                Ok(StreamEvent::ToolArgsDelta {
                    id: "call_1".into(),
                    fragment: fragment.to_vec(),
                })
            })
            .collect();
        items.push(Ok(stop()));
        let (mut events, _count) = from_items(items);
        let mut assembled = Vec::new();
        block_on(async {
            while let Some(item) = events.next().await {
                if let StreamEvent::ToolArgsDelta { fragment, .. } = item.unwrap() {
                    assembled.extend_from_slice(&fragment);
                }
            }
        });
        assert_eq!(assembled, bytes);
        match ToolArgs::from_bytes(&assembled) {
            ToolArgs::Parsed(raw) => assert_eq!(raw.as_str(), args),
            other => panic!("expected parsed arguments, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_bytes_are_reported_not_repaired() {
        for bytes in [&b""[..], b"{\"a\":", b"{} {}", b"\"\xff\""] {
            assert!(
                matches!(ToolArgs::from_bytes(bytes), ToolArgs::Invalid { .. }),
                "{bytes:?} must be invalid"
            );
        }
    }
}
