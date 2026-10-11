//! Hand-written Server-Sent Events framing for provider streams.
//!
//! [`decode_stream`] is the single SSE parser for this crate: it reassembles
//! network chunks split at any byte boundary and emits one [`SseEvent`] per
//! dispatch. [`encode`] writes the same framing and serves the replay
//! surfaces. Event names are metadata only; family decoders dispatch on the
//! JSON `type` member of [`SseEvent::data`], so this module never reads JSON
//! and never consumes a `[DONE]` payload.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;

use crate::error::{LimitError, ProviderError};

/// The maximum raw byte length of one SSE line, inclusive.
pub const SSE_LINE_LIMIT: usize = 1 << 20;

/// The maximum byte length of one dispatched event's joined data, inclusive.
///
/// The limit counts [`SseEvent::data`] as it accumulates, including the LF
/// separators placed between `data` lines.
pub const SSE_EVENT_LIMIT: usize = 8 << 20;

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// One dispatched Server-Sent Events frame.
///
/// `name` is the value of the `event` field when the event carried one;
/// `data` is the LF-joined value of its `data` lines. Dispatch only happens
/// for non-empty data, so an event whose data is empty never reaches the
/// consumer.
#[derive(Clone, Debug, PartialEq)]
pub struct SseEvent {
    /// The `event` field value, when present.
    pub name: Option<String>,
    /// The `data` field values joined with LF.
    pub data: String,
}

/// Writes events in the framing [`decode_stream`] accepts.
///
/// Each event is one optional `event: <name>` line, one `data: <segment>` line
/// per LF-separated segment of `data`, and a blank line. One ASCII space
/// separates each colon from its value; decoding removes exactly that space,
/// so values keep every other byte.
///
/// The unconstrained [`SseEvent`] fields are wider than one wire framing: `name`
/// must not contain CR or LF, `data` must not contain CR (LF is representable
/// as multiple `data` lines), and empty `data` cannot survive the dispatch
/// rule, which requires non-empty data. Values outside that domain are written
/// byte for byte without escaping; round-tripping is guaranteed only inside it.
#[must_use]
pub fn encode(events: &[SseEvent]) -> Vec<u8> {
    let mut wire = Vec::new();
    for event in events {
        if let Some(name) = &event.name {
            wire.extend_from_slice(b"event: ");
            wire.extend_from_slice(name.as_bytes());
            wire.push(b'\n');
        }
        for segment in event.data.split('\n') {
            wire.extend_from_slice(b"data: ");
            wire.extend_from_slice(segment.as_bytes());
            wire.push(b'\n');
        }
        wire.push(b'\n');
    }
    wire
}

/// Decodes a byte stream of Server-Sent Events incrementally.
///
/// Framing rules: a line ends at LF, CR, or CRLF; one leading byte-order mark
/// is stripped; a line starting with `:` is a comment; a line splits at its
/// first colon and one optional ASCII space after the colon is removed;
/// `data` values join with LF; `event` sets the name; `id`, `retry`, and
/// unknown field names are ignored. A blank line dispatches the event when
/// its joined data is non-empty, and a non-empty residual event is dispatched
/// at end of input even without its blank line.
///
/// A line over [`SSE_LINE_LIMIT`] yields [`LimitError::SseLine`], and an event
/// whose joined data would exceed [`SSE_EVENT_LIMIT`] yields
/// [`LimitError::SseEvent`]. Either limit is enforced as bytes arrive, before
/// the oversized buffer is allocated; the stream yields that one failure and
/// then ends. Invalid UTF-8 renders as replacement characters, and chunk
/// boundaries may fall inside any character, terminator, or byte-order mark.
/// Dropping the returned stream drops the byte source, which cancels the
/// underlying transfer.
pub fn decode_stream(
    bytes: impl Stream<Item = Vec<u8>>,
) -> impl Stream<Item = Result<SseEvent, ProviderError>> {
    Decoder::new(bytes)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Source,
    Eof,
    Done,
}

fn append_data(
    data: &mut String,
    data_lines: &mut usize,
    value: &str,
) -> Result<(), ProviderError> {
    let separator = usize::from(*data_lines > 0);
    if data.len() + separator + value.len() > SSE_EVENT_LIMIT {
        return Err(ProviderError::Limit(LimitError::SseEvent));
    }
    if separator > 0 {
        data.push('\n');
    }
    data.push_str(value);
    *data_lines += 1;
    Ok(())
}

struct Decoder<S> {
    source: Pin<Box<S>>,
    chunk: Vec<u8>,
    cursor: usize,
    line: Vec<u8>,
    name: Option<String>,
    data: String,
    data_lines: usize,
    phase: Phase,
    head: [u8; 3],
    head_len: usize,
    head_checked: bool,
    line_feed_pending: bool,
}

impl<S: Stream<Item = Vec<u8>>> Decoder<S> {
    fn new(source: S) -> Self {
        Self {
            source: Box::pin(source),
            chunk: Vec::new(),
            cursor: 0,
            line: Vec::new(),
            name: None,
            data: String::new(),
            data_lines: 0,
            phase: Phase::Source,
            head: [0; 3],
            head_len: 0,
            head_checked: false,
            line_feed_pending: false,
        }
    }

    fn process_input(&mut self) -> Result<Option<SseEvent>, ProviderError> {
        if !self.head_checked {
            while self.head_len < BOM.len() && self.cursor < self.chunk.len() {
                self.head[self.head_len] = self.chunk[self.cursor];
                self.head_len += 1;
                self.cursor += 1;
            }
            if self.head_len < BOM.len() {
                return Ok(None);
            }
            self.head_checked = true;
            let head = self.head;
            if head == BOM {
                return Ok(None);
            }
            for byte in head {
                if let Some(event) = self.feed_byte(byte)? {
                    return Ok(Some(event));
                }
            }
            return Ok(None);
        }
        let byte = self.chunk[self.cursor];
        self.cursor += 1;
        self.feed_byte(byte)
    }

    fn feed_byte(&mut self, byte: u8) -> Result<Option<SseEvent>, ProviderError> {
        if self.line_feed_pending {
            self.line_feed_pending = false;
            if byte == b'\n' {
                return Ok(None);
            }
        }
        match byte {
            b'\n' => self.end_line(),
            b'\r' => {
                self.line_feed_pending = true;
                self.end_line()
            }
            _ => {
                if self.line.len() == SSE_LINE_LIMIT {
                    return Err(ProviderError::Limit(LimitError::SseLine));
                }
                self.line.push(byte);
                Ok(None)
            }
        }
    }

    fn end_line(&mut self) -> Result<Option<SseEvent>, ProviderError> {
        let outcome = self.apply_line();
        self.line.clear();
        outcome
    }

    fn apply_line(&mut self) -> Result<Option<SseEvent>, ProviderError> {
        if self.line.is_empty() {
            return Ok(self.take_event());
        }
        if self.line[0] == b':' {
            return Ok(None);
        }
        let (field, value) = match self.line.iter().position(|byte| *byte == b':') {
            Some(index) => (&self.line[..index], &self.line[index + 1..]),
            None => (&self.line[..], &self.line[self.line.len()..]),
        };
        let value = match value {
            [b' ', rest @ ..] => rest,
            _ => value,
        };
        if field == b"data" {
            let value = String::from_utf8_lossy(value);
            append_data(&mut self.data, &mut self.data_lines, &value)?;
            return Ok(None);
        }
        if field == b"event" {
            self.name = Some(String::from_utf8_lossy(value).into_owned());
        }
        Ok(None)
    }

    fn take_event(&mut self) -> Option<SseEvent> {
        let name = self.name.take();
        let data = std::mem::take(&mut self.data);
        self.data_lines = 0;
        if data.is_empty() {
            None
        } else {
            Some(SseEvent { name, data })
        }
    }

    fn finish(&mut self) -> Result<Option<SseEvent>, ProviderError> {
        if !self.head_checked {
            self.head_checked = true;
            let head = self.head;
            for byte in &head[..self.head_len] {
                if let Some(event) = self.feed_byte(*byte)? {
                    return Ok(Some(event));
                }
            }
        }
        if !self.line.is_empty()
            && let Some(event) = self.end_line()?
        {
            return Ok(Some(event));
        }
        Ok(self.take_event())
    }
}

impl<S: Stream<Item = Vec<u8>>> Stream for Decoder<S> {
    type Item = Result<SseEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if this.phase == Phase::Done {
                return Poll::Ready(None);
            }
            if this.cursor < this.chunk.len() {
                match this.process_input() {
                    Ok(Some(event)) => return Poll::Ready(Some(Ok(event))),
                    Ok(None) => continue,
                    Err(error) => {
                        this.phase = Phase::Done;
                        return Poll::Ready(Some(Err(error)));
                    }
                }
            }
            if this.phase == Phase::Eof {
                let outcome = this.finish();
                this.phase = Phase::Done;
                return Poll::Ready(match outcome {
                    Ok(event) => event.map(Ok),
                    Err(error) => Some(Err(error)),
                });
            }
            match this.source.as_mut().poll_next(cx) {
                Poll::Ready(Some(bytes)) => {
                    this.chunk = bytes;
                    this.cursor = 0;
                }
                Poll::Ready(None) => this.phase = Phase::Eof,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests;
