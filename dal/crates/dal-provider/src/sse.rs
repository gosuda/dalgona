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
mod tests {
    use futures::{StreamExt, executor::block_on, stream};
    use proptest::{arbitrary::any, collection::vec, strategy::Strategy};

    use super::*;

    const BASIC_TEXT_STREAM: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_\u{2026}\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-opus-5-5\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "\n",
        "event: ping\n",
        "data: {\"type\":\"ping\"}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"!\"}}\n",
        "\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        "\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":15}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
    );

    fn decode_texts(chunks: Vec<Vec<u8>>) -> Vec<Result<SseEvent, String>> {
        block_on(decode_stream(stream::iter(chunks)).collect::<Vec<_>>())
            .into_iter()
            .map(|item| match item {
                Ok(event) => Ok(event),
                Err(error) => Err(error.to_string()),
            })
            .collect()
    }

    fn decode_frames(chunks: Vec<Vec<u8>>) -> Vec<SseEvent> {
        decode_texts(chunks)
            .into_iter()
            .map(Result::unwrap)
            .collect()
    }

    fn frame(name: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            name: name.map(String::from),
            data: String::from(data),
        }
    }

    fn wire_type(data: &str) -> &str {
        let start = data.find("\"type\":\"").expect("type member") + 8;
        let rest = &data[start..];
        &rest[..rest.find('"').expect("type value end")]
    }

    fn comment_line(length: usize) -> Vec<u8> {
        let mut line = vec![b':'];
        line.extend(std::iter::repeat_n(b'x', length - 1));
        line.push(b'\n');
        line
    }

    fn short_line_event(joined_len: usize) -> Vec<u8> {
        let mut wire = Vec::new();
        let mut joined = 0;
        while joined < joined_len {
            let separator = usize::from(joined > 0);
            let value = (joined_len - joined - separator).min(9);
            wire.extend_from_slice(b"data: ");
            wire.extend(std::iter::repeat_n(b'x', value));
            wire.push(b'\n');
            joined += value + separator;
        }
        wire
    }

    fn chunked(bytes: &[u8], cuts: &[usize]) -> Vec<Vec<u8>> {
        let mut points: Vec<usize> = cuts.iter().map(|cut| cut % (bytes.len() + 1)).collect();
        points.sort_unstable();
        let mut chunks = Vec::new();
        let mut previous = 0;
        for point in points {
            chunks.push(bytes[previous..point].to_vec());
            previous = point;
        }
        chunks.push(bytes[previous..].to_vec());
        chunks
    }

    fn name_strategy() -> impl Strategy<Value = String> {
        any::<String>().prop_filter("name has no line break", |name| {
            !name.contains('\r') && !name.contains('\n')
        })
    }

    fn event_strategy() -> impl Strategy<Value = SseEvent> {
        let data = any::<String>()
            .prop_filter("data has no carriage return", |data| !data.contains('\r'))
            .prop_map(|data| {
                if data.is_empty() {
                    String::from("x")
                } else {
                    data
                }
            });
        (proptest::option::of(name_strategy()), data)
            .prop_map(|(name, data)| SseEvent { name, data })
    }

    #[test]
    fn sse_parser() {
        let frames = decode_frames(vec![BASIC_TEXT_STREAM.as_bytes().to_vec()]);
        assert_eq!(frames.len(), 8);
        let names: Vec<Option<&str>> = frames.iter().map(|event| event.name.as_deref()).collect();
        assert_eq!(
            names,
            vec![
                Some("message_start"),
                Some("content_block_start"),
                Some("ping"),
                Some("content_block_delta"),
                Some("content_block_delta"),
                Some("content_block_stop"),
                Some("message_delta"),
                Some("message_stop"),
            ]
        );
        let types: Vec<&str> = frames.iter().map(|event| wire_type(&event.data)).collect();
        assert_eq!(
            types,
            vec![
                "message_start",
                "content_block_start",
                "ping",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
    }

    #[test]
    fn sse_line_endings() {
        let expected = decode_frames(vec![BASIC_TEXT_STREAM.as_bytes().to_vec()]);
        let streams = [
            BASIC_TEXT_STREAM.replace('\n', "\r\n"),
            BASIC_TEXT_STREAM.replace('\n', "\r"),
            format!("\u{feff}{BASIC_TEXT_STREAM}"),
            format!("\u{feff}{}", BASIC_TEXT_STREAM.replace('\n', "\r\n")),
        ];
        for stream in streams {
            assert_eq!(
                decode_frames(vec![stream.as_bytes().to_vec()]),
                expected,
                "{stream:?}"
            );
        }
    }

    #[test]
    fn sse_end_of_input() {
        assert_eq!(
            decode_frames(vec![
                b"event: message_stop\ndata: {\"type\":\"message_stop\"}".to_vec()
            ]),
            vec![frame(Some("message_stop"), "{\"type\":\"message_stop\"}")]
        );
        assert_eq!(
            decode_frames(vec![b"data: x".to_vec()]),
            vec![frame(None, "x")]
        );
        assert_eq!(
            decode_frames(vec![b"data: x\r".to_vec()]),
            vec![frame(None, "x")]
        );
        assert!(decode_frames(vec![b"event: ping".to_vec()]).is_empty());
    }

    #[test]
    fn sse_size_limits() {
        let mut over_line = comment_line(SSE_LINE_LIMIT + 1);
        over_line.extend_from_slice(b"data: later\n\n");
        assert_eq!(
            decode_texts(vec![over_line]),
            vec![Err(String::from("SSE line exceeds 1 MiB."))]
        );
        let mut over_event = short_line_event(SSE_EVENT_LIMIT + 1);
        over_event.extend_from_slice(b"data: later\n\n");
        assert_eq!(
            decode_texts(vec![over_event]),
            vec![Err(String::from("SSE event exceeds 8 MiB."))]
        );
    }

    #[test]
    fn size_limits_are_inclusive() {
        let mut at_line_limit = comment_line(SSE_LINE_LIMIT);
        at_line_limit.extend_from_slice(b"data: x\n\n");
        assert_eq!(decode_frames(vec![at_line_limit]), vec![frame(None, "x")]);
        let mut at_event_limit = short_line_event(SSE_EVENT_LIMIT);
        at_event_limit.push(b'\n');
        let frames = decode_frames(vec![at_event_limit]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data.len(), SSE_EVENT_LIMIT);
    }

    #[test]
    fn limit_failure_ends_the_stream_after_one_error() {
        let mut wire = comment_line(SSE_LINE_LIMIT + 1);
        wire.extend_from_slice(b"data: later\n\n");
        let chunks: Vec<Vec<u8>> = wire.chunks(64).map(<[u8]>::to_vec).collect();
        assert_eq!(
            decode_texts(chunks),
            vec![Err(String::from("SSE line exceeds 1 MiB."))]
        );
    }

    #[test]
    fn framing_fields_and_comments_keep_exact_values() {
        let wire = concat!(
            ": keep-alive\n",
            ":no space\n",
            "id: 7\n",
            "retry: 100\n",
            "unknown: value\n",
            "data: a:b\n",
            "data:  two spaces kept\n",
            "data:\ttab kept\n",
            "data:\n",
            "data: x\n",
            "\n",
        );
        assert_eq!(
            decode_frames(vec![wire.as_bytes().to_vec()]),
            vec![frame(None, "a:b\n two spaces kept\n\ttab kept\n\nx")]
        );
    }

    #[test]
    fn empty_data_semantics() {
        assert!(decode_frames(vec![b"data:\n\n".to_vec()]).is_empty());
        assert!(decode_frames(vec![b"data: \n\n".to_vec()]).is_empty());
        assert!(decode_frames(vec![b"event: ping\n\n".to_vec()]).is_empty());
        assert!(decode_frames(vec![b": comment\n\n".to_vec()]).is_empty());
        assert_eq!(
            decode_frames(vec![b"data:  \n\n".to_vec()]),
            vec![frame(None, " ")]
        );
        assert_eq!(
            decode_frames(vec![b"data:\ndata: x\n\n".to_vec()]),
            vec![frame(None, "\nx")]
        );
        assert_eq!(
            decode_frames(vec![b"data: x\ndata:\n\n".to_vec()]),
            vec![frame(None, "x\n")]
        );
        assert_eq!(
            decode_frames(vec![b"data:\ndata:\n\n".to_vec()]),
            vec![frame(None, "\n")]
        );
        assert_eq!(
            decode_frames(vec![b"event: a\n\ndata: x\n\n".to_vec()]),
            vec![frame(None, "x")]
        );
    }

    #[test]
    fn event_names_are_set_and_reset() {
        let wire = concat!(
            "event: a\ndata: 1\n\n",
            "data: 2\n\n",
            "event:\ndata: 3\n\n",
            "event: d\ndata: 4\ndata: 5\n\n",
        );
        assert_eq!(
            decode_frames(vec![wire.as_bytes().to_vec()]),
            vec![
                frame(Some("a"), "1"),
                frame(None, "2"),
                frame(Some(""), "3"),
                frame(Some("d"), "4\n5"),
            ]
        );
    }

    #[test]
    fn one_leading_byte_order_mark_is_stripped() {
        let marked = "\u{feff}data: \u{feff}x\n\ndata: y\u{feff}\n\n".to_owned();
        assert_eq!(
            decode_frames(vec![marked.as_bytes().to_vec()]),
            vec![frame(None, "\u{feff}x"), frame(None, "y\u{feff}")]
        );
        let prefixed = "data: x\n\n\u{feff}data: y\n\n".to_owned();
        assert_eq!(
            decode_frames(vec![prefixed.as_bytes().to_vec()]),
            vec![frame(None, "x")]
        );
        assert!(decode_frames(vec![vec![0xEF, 0xBB, 0xBF]]).is_empty());
        assert!(decode_frames(vec![vec![0xEF, 0xBB]]).is_empty());
    }

    #[test]
    fn every_chunk_boundary_and_multibyte_split() {
        let wire = "\u{feff}event: ping\ndata: {\"x\":\"\u{e9}\"}\r\n\r\ndata: a\r\ndata: \u{fc}n";
        let expected = decode_frames(vec![wire.as_bytes().to_vec()]);
        assert_eq!(
            expected,
            vec![
                frame(Some("ping"), "{\"x\":\"\u{e9}\"}"),
                frame(None, "a\n\u{fc}n"),
            ]
        );
        let bytes = wire.as_bytes();
        for at in 0..=bytes.len() {
            let chunks = vec![bytes[..at].to_vec(), bytes[at..].to_vec()];
            assert_eq!(decode_frames(chunks), expected, "split at {at}");
        }
        let one_byte: Vec<Vec<u8>> = bytes.iter().map(|byte| vec![*byte]).collect();
        assert_eq!(decode_frames(one_byte), expected);
    }

    #[test]
    fn invalid_utf8_becomes_replacement_characters() {
        let mut wire = b"data: ".to_vec();
        wire.extend([0xFF, 0xFE]);
        wire.extend_from_slice(b"\n\ndata: tail ");
        wire.push(0xC3);
        assert_eq!(
            decode_frames(vec![wire]),
            vec![
                frame(None, "\u{FFFD}\u{FFFD}"),
                frame(None, "tail \u{FFFD}"),
            ]
        );
    }

    #[test]
    fn encode_writes_the_canonical_framing() {
        let events = vec![
            SseEvent {
                name: Some(String::from("message_start")),
                data: String::from("{\"type\":\"message_start\"}"),
            },
            SseEvent {
                name: None,
                data: String::from("a\nb"),
            },
            SseEvent {
                name: Some(String::new()),
                data: String::from(" x"),
            },
        ];
        assert_eq!(
            encode(&events),
            concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\"}\n",
                "\n",
                "data: a\n",
                "data: b\n",
                "\n",
                "event: \n",
                "data:  x\n",
                "\n",
            )
            .as_bytes()
        );
    }

    proptest::proptest! {
        #[test]
        fn sse_roundtrip_property(
            events in vec(event_strategy(), 0..8),
            cuts in vec(any::<usize>(), 0..24),
        ) {
            let wire = encode(&events);
            let frames = decode_frames(chunked(&wire, &cuts));
            proptest::prop_assert_eq!(frames, events);
        }
    }
}
