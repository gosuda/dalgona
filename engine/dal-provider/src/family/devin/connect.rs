//! Connect-protocol framing for the Devin family.
//!
//! Devin speaks Connect over HTTP/1.1: every message on the wire is one frame
//! of a flag byte plus a 4-byte big-endian payload length plus the payload.
//! Flag bit `0x01` marks a gzip payload; flag bit `0x02` marks the
//! end-of-stream frame, whose payload is a JSON trailer (`{ error: ... }` on
//! failure, an empty object on success) instead of a protobuf message.
//! Request frames are always gzipped; response frames may be either.
//!
//! [`FrameDecoder`] reassembles network chunks split at any byte boundary and
//! yields complete frames in wire order. It joins a split frame once, when the
//! announced length has fully arrived, and aliases single-chunk frames without
//! copying. A length prefix above [`MAX_FRAME_PAYLOAD`] fails before a byte
//! of that payload is buffered.
//!
//! Payloads inflate through [`gunzip_capped`], which caps decompressed output
//! at the same bound so a tiny frame cannot expand into gigabytes. Unary
//! responses decode through [`decode_unary`], which tries the raw bytes first
//! and falls back to gunzip, because edges variously return bare or gzipped
//! protobuf.

use std::collections::VecDeque;
use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

/// Flag bit: the frame payload is gzip-compressed.
pub(crate) const COMPRESSED_FLAG: u8 = 0x01;
/// Flag bit: the frame carries the end-of-stream JSON trailer.
pub(crate) const END_STREAM_FLAG: u8 = 0x02;
/// Wire bytes of one frame header: 1 flag byte plus a `u32` length.
pub(crate) const HEADER_LEN: usize = 5;
/// Hard upper bound on one frame payload and on gunzip output, in bytes.
pub(crate) const MAX_FRAME_PAYLOAD: usize = 16 * 1024 * 1024;
/// Upper bound on retained trailer evidence, in characters.
pub(crate) const MAX_TRAILER_EVIDENCE_CHARS: usize = 2000;

/// One framing, compression, or trailer failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ConnectError {
    /// A frame declared a payload length over [`MAX_FRAME_PAYLOAD`].
    #[error("devin Connect frame length {length} exceeds {MAX_FRAME_PAYLOAD}-byte cap")]
    FrameTooLarge {
        /// The declared payload length.
        length: u32,
    },
    /// A payload did not inflate.
    #[error("devin Connect payload did not decompress: {detail}")]
    Inflate {
        /// The gunzip failure text.
        detail: String,
    },
    /// Inflation passed the output cap: a zip bomb, not a message.
    #[error("devin Connect payload inflates past {MAX_FRAME_PAYLOAD}-byte cap")]
    InflateTooLarge,
    /// A payload did not compress.
    #[error("devin Connect payload did not compress: {detail}")]
    Deflate {
        /// The gzip failure text.
        detail: String,
    },
    /// A payload did not decode as its protobuf message.
    #[error("devin Connect payload is not a {message}: {detail}")]
    Decode {
        /// The expected message name.
        message: &'static str,
        /// The decode failure text.
        detail: String,
    },
}

/// One decoded Connect envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Frame {
    /// The frame flag byte.
    pub(crate) flags: u8,
    /// The frame payload; still compressed when `COMPRESSED_FLAG` is set.
    pub(crate) payload: Vec<u8>,
}

/// Wraps one message in a Connect frame: flag byte, big-endian length, body.
#[must_use]
pub(crate) fn frame_message(body: &[u8], flags: u8) -> Vec<u8> {
    let length = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut frame = Vec::with_capacity(HEADER_LEN.saturating_add(body.len()));
    frame.push(flags);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

/// Compresses one request body with gzip.
pub(crate) fn gzip(body: &[u8]) -> Result<Vec<u8>, ConnectError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(body)
        .map_err(|error| ConnectError::Deflate {
            detail: error.to_string(),
        })?;
    encoder.finish().map_err(|error| ConnectError::Deflate {
        detail: error.to_string(),
    })
}

/// Inflates one frame payload, capped at [`MAX_FRAME_PAYLOAD`] output bytes.
pub(crate) fn gunzip_capped(payload: &[u8]) -> Result<Vec<u8>, ConnectError> {
    let cap = MAX_FRAME_PAYLOAD.saturating_add(1);
    let mut decoder = GzDecoder::new(payload);
    let mut out = Vec::new();
    decoder
        .by_ref()
        .take(cap as u64)
        .read_to_end(&mut out)
        .map_err(|error| ConnectError::Inflate {
            detail: error.to_string(),
        })?;
    if out.len() > MAX_FRAME_PAYLOAD {
        return Err(ConnectError::InflateTooLarge);
    }
    Ok(out)
}

/// Decodes one unary response: raw protobuf first, gunzip fallback second.
///
/// Edges variously return bare or gzipped bodies, and the runtime may already
/// have decompressed the transfer, so the direct decode runs before the gzip
/// fallback. Both failures report the message name for diagnostics.
pub(crate) fn decode_unary<M>(bytes: &[u8], message: &'static str) -> Result<M, ConnectError>
where
    M: prost::Message + Default,
{
    match M::decode(bytes) {
        Ok(decoded) => Ok(decoded),
        Err(first) => match gunzip_capped(bytes) {
            Ok(raw) => M::decode(raw.as_slice()).map_err(|error| ConnectError::Decode {
                message,
                detail: error.to_string(),
            }),
            Err(_) => Err(ConnectError::Decode {
                message,
                detail: first.to_string(),
            }),
        },
    }
}

/// Incremental Connect frame decoder over a byte stream.
///
/// Chunks accumulate until each announced frame is complete; complete frames
/// wait in [`FrameDecoder::drain`]. A header that arrives split across chunks
/// is joined in place. Every chunk is copied into the buffer when it arrives
/// and every payload is copied again into its `Frame`, so each frame is copied
/// at least twice and none aliases the input. A multi-megabyte frame still
/// costs one join instead of one re-concatenation per chunk.
#[derive(Debug, Default)]
pub(crate) struct FrameDecoder {
    /// Buffered chunk tails, oldest first.
    chunks: VecDeque<Vec<u8>>,
    /// Bytes held in `chunks`.
    buffered: usize,
    /// Complete frames awaiting [`FrameDecoder::drain`].
    ready: Vec<Frame>,
}

impl FrameDecoder {
    /// An empty decoder.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Buffers `chunk` and extracts every frame now complete, in wire order.
    ///
    /// An oversize length prefix fails after the frames that precede it have
    /// been queued; the offending header stays buffered and the decoder
    /// refuses further input until drained by a fresh decoder.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<(), ConnectError> {
        if !chunk.is_empty() {
            self.buffered = self.buffered.saturating_add(chunk.len());
            self.chunks.push_back(chunk.to_vec());
        }
        while self.buffered >= HEADER_LEN {
            self.join_header();
            let length = self.peek_length();
            if length > MAX_FRAME_PAYLOAD as u64 {
                let length = u32::try_from(length).unwrap_or(u32::MAX);
                return Err(ConnectError::FrameTooLarge { length });
            }
            let frame_len =
                HEADER_LEN.saturating_add(usize::try_from(length).unwrap_or(usize::MAX));
            if self.buffered < frame_len {
                return Ok(());
            }
            let frame = self.take(frame_len);
            self.ready.push(Frame {
                flags: frame[0],
                payload: frame[HEADER_LEN..].to_vec(),
            });
        }
        Ok(())
    }

    /// Takes every complete frame queued so far, in wire order.
    #[must_use]
    pub(crate) fn drain(&mut self) -> Vec<Frame> {
        std::mem::take(&mut self.ready)
    }

    /// Whether bytes are still buffered: a torn frame at end of input.
    #[must_use]
    pub(crate) fn has_partial(&self) -> bool {
        self.buffered > 0
    }

    /// Reads the length prefix of the first chunk, which spans the header.
    fn peek_length(&self) -> u64 {
        let first = self.chunks.front().map(Vec::as_slice).unwrap_or_default();
        u64::from(first[1]) << 24
            | u64::from(first[2]) << 16
            | u64::from(first[3]) << 8
            | u64::from(first[4])
    }

    /// Joins chunks until the first spans the whole header.
    fn join_header(&mut self) {
        while self
            .chunks
            .front()
            .is_some_and(|chunk| chunk.len() < HEADER_LEN)
        {
            let mut head = self.chunks.pop_front().unwrap_or_default();
            if let Some(next) = self.chunks.pop_front() {
                head.extend_from_slice(&next);
            }
            self.chunks.push_front(head);
        }
    }

    /// Removes and returns the next `size` buffered bytes.
    fn take(&mut self, size: usize) -> Vec<u8> {
        self.buffered = self.buffered.saturating_sub(size);
        let Some(first) = self.chunks.pop_front() else {
            return Vec::new();
        };
        if first.len() == size {
            return first;
        }
        if first.len() > size {
            let rest = first[size..].to_vec();
            self.chunks.push_front(rest);
            return first[..size].to_vec();
        }
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(&first);
        while out.len() < size {
            let Some(next) = self.chunks.pop_front() else {
                break;
            };
            let want = size.saturating_sub(out.len()).min(next.len());
            out.extend_from_slice(&next[..want]);
            if want < next.len() {
                self.chunks.push_front(next[want..].to_vec());
            }
        }
        out
    }
}

/// A parsed end-of-stream trailer carrying a server error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrailerError {
    /// Connect error code, e.g. `invalid_argument`; empty when unnamed.
    pub(crate) code: String,
    /// `Devin stream error <code>: <message>`, with details appended.
    pub(crate) formatted: String,
}

/// Parses an end-of-stream payload: `None` is a clean finish, `Some` is the
/// server's rejection. The payload is untrusted server output, so the shape
/// is checked with guards rather than asserted, mirroring the pinned
/// reference: empty bytes, unparseable bytes, and a missing, null,
/// non-object, or empty `error` member all mean a clean finish. Only an
/// error object naming a code or a message rejects the turn.
#[must_use]
pub(crate) fn parse_trailer(payload: &[u8]) -> Option<TrailerError> {
    if payload.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(payload).ok()?;
    let parsed: sonic_rs::Value = sonic_rs::from_str(text).ok()?;
    let error = parsed.get("error")?;
    if error.is_null() || !error.is_object() {
        return None;
    }
    let code = error
        .get("code")
        .and_then(|code| code.as_str())
        .unwrap_or_default();
    let message = error
        .get("message")
        .and_then(|message| message.as_str())
        .unwrap_or_default();
    if code.is_empty() && message.is_empty() {
        return None;
    }
    let mut formatted = String::from("Devin stream error");
    if !code.is_empty() {
        formatted.push(' ');
        formatted.push_str(code);
    }
    formatted.push_str(": ");
    formatted.push_str(message);
    if let Some(detail) = summarize_details(error.get("details")) {
        formatted.push_str(" [details: ");
        formatted.push_str(&detail);
        formatted.push(']');
    }
    Some(TrailerError {
        code: String::from(code),
        formatted,
    })
}

/// Truncates to a character boundary so evidence never splits a scalar.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return String::from(text);
    }
    let mut out = String::new();
    for (index, point) in text.chars().enumerate() {
        if index >= max_chars {
            break;
        }
        out.push(point);
    }
    out.push('…');
    out
}

/// Summarizes Connect error `details`: loosely shaped `{ type, value, debug }`
/// records joined with `; `, bounded so log lines stay short.
fn summarize_details(details: Option<&sonic_rs::Value>) -> Option<String> {
    let entries = details?.as_array()?;
    let mut summary = String::new();
    for entry in entries {
        let kind = entry
            .get("type")
            .and_then(|kind| kind.as_str())
            .filter(|kind| !kind.is_empty());
        let debug = entry.get("debug").and_then(|debug| {
            debug
                .as_str()
                .map(String::from)
                .or_else(|| sonic_rs::to_string(debug).ok())
        });
        let value = entry
            .get("value")
            .and_then(|value| {
                value
                    .as_str()
                    .map(String::from)
                    .or_else(|| sonic_rs::to_string(value).ok())
            })
            .filter(|value| !value.is_empty());
        let evidence = debug
            .or(value)
            .map(|text| truncate_chars(&text, MAX_TRAILER_EVIDENCE_CHARS));
        let part = match (kind, evidence) {
            (Some(kind), Some(evidence)) => format!("{kind}: {evidence}"),
            (Some(kind), None) => String::from(kind),
            (None, Some(evidence)) => evidence,
            (None, None) => continue,
        };
        if !summary.is_empty() {
            summary.push_str("; ");
        }
        summary.push_str(&truncate_chars(&part, MAX_TRAILER_EVIDENCE_CHARS));
        if summary.len() > MAX_TRAILER_EVIDENCE_CHARS {
            return Some(truncate_chars(&summary, MAX_TRAILER_EVIDENCE_CHARS));
        }
    }
    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}
