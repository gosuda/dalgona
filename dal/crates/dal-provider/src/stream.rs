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

use crate::{error::ProviderError, tool_names::ToolNames};

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

    /// Maps the tool names of every event back to internal names.
    pub(crate) fn restoring(mut self, names: ToolNames) -> Self {
        if names.is_empty() {
            return self;
        }
        self.source = self.source.take().map(|source| -> Source {
            Box::pin(source.map(move |item| item.map(|event| names.restore_event(event))))
        });
        self
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
mod tests;
