//! The AWS binary event-stream framing of one Bedrock response.
//!
//! Bedrock answers `converse-stream` with `application/vnd.amazon.eventstream`,
//! a binary framing, never Server-Sent Events: the stream is a sequence of
//! frames
//!
//! ```text
//! [total length u32 BE]
//! [headers length u32 BE]
//! [prelude CRC32 u32 BE]        over the first 8 bytes
//! [headers headers_len]         name/value pairs
//! [payload]                     total_len - headers_len - 16 bytes
//! [message CRC32 u32 BE]        over every byte before it
//! ```
//!
//! [`FrameDecoder`] reassembles network chunks split at any byte boundary
//! and emits one [`Frame`] per message. The prelude CRC is validated before
//! any declared length is trusted, and the message CRC before any header or
//! payload byte is read, both with `crc32fast`; a mismatch or a truncated
//! tail is a protocol failure, and the decoder yields that one failure and
//! then ends, dropping what it buffered. Header values decode as their typed
//! wire values ([`HeaderValue`]); Bedrock dispatches on the string headers
//! `:message-type`, `:event-type`, `:content-type`, and `:exception-type`.
//!
//! # Dependencies
//!
//! `crc32fast` computes the CRC-32 (IEEE, polynomial `0xEDB88320`) of both
//! checks.

use std::collections::BTreeMap;

use crc32fast::Hasher;
use futures::{Stream, StreamExt, stream};

/// One framing or header-block failure. Rendered into
/// [`crate::error::ProviderError::Protocol`] by the family decoder, which
/// owns the family binding.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum FrameError {
    /// A frame declared a length pair the framing cannot produce.
    #[error("event-stream frame lengths are invalid: total {total}, headers {headers}")]
    Lengths {
        /// The declared total length.
        total: u32,
        /// The declared headers length.
        headers: u32,
    },
    /// A frame declared a total length over [`MAX_FRAME_LEN`].
    #[error("event-stream frame of {total} bytes is over the {limit} byte limit")]
    FrameLimit {
        /// The declared total length.
        total: u32,
        /// The applied limit.
        limit: usize,
    },
    /// A header block over [`MAX_HEADERS_LEN`] was declared.
    #[error("event-stream headers of {headers} bytes are over the {limit} byte limit")]
    HeadersLimit {
        /// The declared headers length.
        headers: u32,
        /// The applied limit.
        limit: usize,
    },
    /// The prelude or message CRC did not match the bytes.
    #[error(
        "event-stream {scope} CRC mismatch: expected {expected:#010x}, computed {actual:#010x}"
    )]
    Crc {
        /// Which CRC failed.
        scope: CrcScope,
        /// The CRC the frame carried.
        expected: u32,
        /// The CRC the bytes produce.
        actual: u32,
    },
    /// The header block ended inside one header, or a name was empty or
    /// invalid UTF-8.
    #[error("event-stream headers are malformed at byte {offset}")]
    Header {
        /// The offset inside the header block.
        offset: usize,
    },
    /// A header value carried a type this decoder does not know.
    #[error("event-stream header value type {kind} is unknown")]
    HeaderType {
        /// The unknown type byte.
        kind: u8,
    },
    /// The stream ended inside one frame.
    #[error("event-stream ended inside a {declared} byte frame with {available} bytes available")]
    Truncated {
        /// The declared total length of the unfinished frame, or the bytes
        /// buffered when the prelude was incomplete.
        declared: u32,
        /// The bytes actually buffered.
        available: usize,
    },
}

/// Which CRC failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CrcScope {
    /// The CRC over the first 8 bytes of the prelude.
    Prelude,
    /// The CRC over the whole message before its trailing four bytes.
    Message,
}

impl std::fmt::Display for CrcScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Prelude => "prelude",
            Self::Message => "message",
        };
        f.write_str(name)
    }
}

/// One typed event-stream header value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HeaderValue {
    /// Type 0/1: one byte, zero or one.
    Bool(bool),
    /// Type 2: one signed byte.
    Byte(i8),
    /// Type 3: big-endian signed 16-bit.
    Int16(i16),
    /// Type 4: big-endian signed 32-bit.
    Int32(i32),
    /// Type 5: big-endian signed 64-bit.
    Int64(i64),
    /// Type 6: length-prefixed bytes, kept verbatim.
    ByteArray(Vec<u8>),
    /// Type 7: length-prefixed UTF-8.
    String(String),
    /// Type 8: milliseconds since the Unix epoch as a signed 64-bit.
    Timestamp(i64),
    /// Type 9: 16 raw UUID bytes.
    Uuid([u8; 16]),
}

impl HeaderValue {
    /// The text of a string-valued header.
    #[must_use]
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }
}

/// One CRC-validated event-stream message.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Frame {
    /// The typed headers in wire order; Bedrock header names are
    /// case-sensitive (`:event-type`, not `:Event-Type`). A repeated name
    /// keeps the last value, the overwrite semantics a header map has.
    pub(crate) headers: BTreeMap<String, HeaderValue>,
    /// The payload bytes between the header block and the message CRC.
    pub(crate) payload: Vec<u8>,
}

impl Frame {
    /// The string value of one header name.
    #[must_use]
    pub(crate) fn string_header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(HeaderValue::as_str)
    }
}

/// The bytes before the header block: both length fields plus the prelude
/// CRC.
const PRELUDE_LEN: usize = 12;

/// The framing overhead of every message: the prelude plus the message CRC.
const MESSAGE_OVERHEAD: usize = 16;

/// The longest header block accepted, bytes.
pub(crate) const MAX_HEADERS_LEN: u32 = 128 * 1024;

/// The longest single frame accepted, bytes, matching the SSE event bound
/// the other families decode under.
pub(crate) const MAX_FRAME_LEN: u32 = 8 << 20;

/// The CRC-32 of `bytes`, streamed so no slice copy is needed.
fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// Validates the prelude of one buffered frame and returns its lengths as
/// `usize`, proved lossless: every declared `u32` was already bounded by
/// [`MAX_FRAME_LEN`], and [`u32`] fits `usize` on the supported platforms.
fn inspect_prelude(buffer: &[u8]) -> Result<(u32, u32), FrameError> {
    // The prelude CRC comes first: until it matches, neither declared
    // length is evidence of anything.
    let expected = u32::from_be_bytes([buffer[8], buffer[9], buffer[10], buffer[11]]);
    let actual = crc32(&buffer[..8]);
    if expected != actual {
        return Err(FrameError::Crc {
            scope: CrcScope::Prelude,
            expected,
            actual,
        });
    }
    let total = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
    let headers = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);
    if total < u32::try_from(MESSAGE_OVERHEAD).unwrap_or(u32::MAX)
        || headers > total.saturating_sub(u32::try_from(MESSAGE_OVERHEAD).unwrap_or(u32::MAX))
    {
        return Err(FrameError::Lengths { total, headers });
    }
    if total > MAX_FRAME_LEN {
        return Err(FrameError::FrameLimit {
            total,
            limit: usize::try_from(MAX_FRAME_LEN).unwrap_or(usize::MAX),
        });
    }
    if headers > MAX_HEADERS_LEN {
        return Err(FrameError::HeadersLimit {
            headers,
            limit: usize::try_from(MAX_HEADERS_LEN).unwrap_or(usize::MAX),
        });
    }
    Ok((total, headers))
}

/// Reads one big-endian integer of `N` bytes at `cursor`, failing the whole
/// header block at `offset` when fewer bytes remain.
fn take<const N: usize>(
    buffer: &[u8],
    cursor: &mut usize,
    end: usize,
    offset: usize,
) -> Result<[u8; N], FrameError> {
    // The header block ends at `end`: a short value at the block tail must
    // fail here instead of reading payload or CRC bytes as header data.
    if *cursor + N > end {
        return Err(FrameError::Header { offset });
    }
    let Some(bytes) = buffer.get(*cursor..*cursor + N) else {
        return Err(FrameError::Header { offset });
    };
    *cursor += N;
    bytes.try_into().map_err(|_| FrameError::Header { offset })
}

/// Reads `length` bytes at `cursor` as one borrowed slice, advancing past
/// them; the caller bounds `length` with [`take_len`] first.
fn take_slice<'a>(
    buffer: &'a [u8],
    cursor: &mut usize,
    end: usize,
    length: usize,
    offset: usize,
) -> Result<&'a [u8], FrameError> {
    if *cursor + length > end {
        return Err(FrameError::Header { offset });
    }
    let Some(bytes) = buffer.get(*cursor..*cursor + length) else {
        return Err(FrameError::Header { offset });
    };
    *cursor += length;
    Ok(bytes)
}

/// Reads one `u16` byte length at `cursor` and bounds-checks the range it
/// names, failing the header block at `offset` when it leaves the block.
fn take_len(
    buffer: &[u8],
    cursor: &mut usize,
    end: usize,
    offset: usize,
) -> Result<usize, FrameError> {
    let length = u16::from_be_bytes(take::<2>(buffer, cursor, end, offset)?);
    let length = usize::from(length);
    if *cursor + length > end {
        return Err(FrameError::Header { offset });
    }
    Ok(length)
}

/// Parses the typed headers of one validated frame between `start` and
/// `end` of `buffer`.
fn parse_headers(
    buffer: &[u8],
    start: usize,
    end: usize,
) -> Result<BTreeMap<String, HeaderValue>, FrameError> {
    let mut headers = BTreeMap::new();
    let mut cursor = start;
    while cursor < end {
        let offset = cursor - start;
        let name_len = usize::from(take::<1>(buffer, &mut cursor, end, offset)?[0]);
        if name_len == 0 || cursor + name_len > end {
            return Err(FrameError::Header { offset });
        }
        let name_bytes = take_slice(buffer, &mut cursor, end, name_len, offset)?;
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| FrameError::Header { offset })?
            .to_owned();
        let kind = take::<1>(buffer, &mut cursor, end, offset)?[0];
        let value = match kind {
            0 => HeaderValue::Bool(true),
            1 => HeaderValue::Bool(false),
            // One wire byte is one `i8` by definition; `cast_signed` reinterprets
            // the same octet the sender wrote.
            2 => HeaderValue::Byte(take::<1>(buffer, &mut cursor, end, offset)?[0].cast_signed()),
            3 => HeaderValue::Int16(i16::from_be_bytes(take::<2>(
                buffer,
                &mut cursor,
                end,
                offset,
            )?)),
            4 => HeaderValue::Int32(i32::from_be_bytes(take::<4>(
                buffer,
                &mut cursor,
                end,
                offset,
            )?)),
            5 => HeaderValue::Int64(i64::from_be_bytes(take::<8>(
                buffer,
                &mut cursor,
                end,
                offset,
            )?)),
            6 => {
                let length = take_len(buffer, &mut cursor, end, offset)?;
                let bytes = take_slice(buffer, &mut cursor, end, length, offset)?;
                HeaderValue::ByteArray(bytes.to_vec())
            }
            7 => {
                let length = take_len(buffer, &mut cursor, end, offset)?;
                let bytes = take_slice(buffer, &mut cursor, end, length, offset)?;
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| FrameError::Header { offset })?
                    .to_owned();
                HeaderValue::String(text)
            }
            8 => HeaderValue::Timestamp(i64::from_be_bytes(take::<8>(
                buffer,
                &mut cursor,
                end,
                offset,
            )?)),
            9 => HeaderValue::Uuid(take::<16>(buffer, &mut cursor, end, offset)?),
            unknown => return Err(FrameError::HeaderType { kind: unknown }),
        };
        headers.insert(name, value);
    }
    Ok(headers)
}

/// Decodes one complete frame out of `buffer[0..total]`; both CRCs are
/// checked before any byte is consumed.
///
/// # Errors
///
/// [`FrameError`] for the message CRC or any header failure; the prelude
/// CRC was checked by [`inspect_prelude`].
pub(crate) fn decode_frame(
    buffer: &[u8],
    total: u32,
    headers_len: u32,
) -> Result<Frame, FrameError> {
    // Both lengths passed `inspect_prelude`, so the widening casts are
    // lossless on the supported platforms.
    let total = total as usize;
    let headers_len = headers_len as usize;
    let headers_end = PRELUDE_LEN + headers_len;
    let crc_offset = total - 4;
    let expected = u32::from_be_bytes([
        buffer[crc_offset],
        buffer[crc_offset + 1],
        buffer[crc_offset + 2],
        buffer[crc_offset + 3],
    ]);
    let actual = crc32(&buffer[..crc_offset]);
    if expected != actual {
        return Err(FrameError::Crc {
            scope: CrcScope::Message,
            expected,
            actual,
        });
    }
    let headers = parse_headers(buffer, PRELUDE_LEN, headers_end)?;
    Ok(Frame {
        headers,
        payload: buffer[headers_end..crc_offset].to_vec(),
    })
}

/// What one network chunk produced: every frame that completed before the
/// first failure, in wire order, then that failure, if any. Frames that
/// arrived intact ahead of a corrupt one are never lost with it.
#[must_use]
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Pushed {
    /// The frames that completed, each fully validated.
    pub(crate) frames: Vec<Frame>,
    /// The failure that ended the decoder, reported once.
    pub(crate) failure: Option<FrameError>,
}

/// The incremental decoder of one Bedrock event-stream response.
///
/// Feed it every network chunk with [`FrameDecoder::push`], then call
/// [`FrameDecoder::finish`] at end of input. Complete frames are emitted as
/// they arrive; the first framing failure is returned once, and a failed or
/// finished decoder ignores all further input.
#[must_use]
#[derive(Debug)]
pub(crate) struct FrameDecoder {
    buffer: Vec<u8>,
    /// The `(total, headers_len)` of the frame being reassembled, fixed at
    /// its validated prelude so a partial frame is never re-inspected.
    pending: Option<(u32, u32)>,
    failed: bool,
    finished: bool,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// A decoder for one response.
    pub(crate) fn new() -> Self {
        Self {
            buffer: Vec::new(),
            pending: None,
            failed: false,
            finished: false,
        }
    }

    /// Admits one network chunk and returns every frame that completed,
    /// then the first [`FrameError`] the bytes produce, if any. After a
    /// failure the decoder drops its buffer and ignores later input.
    ///
    /// Completed frames are cut from the front of the buffer in one pass
    /// after the last one, so a chunk of many frames moves the tail once.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Pushed {
        if self.failed || self.finished {
            return Pushed::default();
        }
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        let mut consumed = 0;
        let failure = loop {
            match self.next_frame(consumed) {
                Ok(Some((frame, length))) => {
                    frames.push(frame);
                    consumed += length;
                }
                Ok(None) => break None,
                Err(error) => break Some(error),
            }
        };
        match failure {
            Some(error) => Pushed {
                frames,
                failure: Some(self.fail(error)),
            },
            None => {
                self.buffer.drain(..consumed);
                Pushed {
                    frames,
                    failure: None,
                }
            }
        }
    }

    /// The complete frame that starts `consumed` bytes into the buffer,
    /// with its wire length, or `None` while more bytes are needed. The
    /// prelude of a partial frame is validated once and remembered.
    fn next_frame(&mut self, consumed: usize) -> Result<Option<(Frame, usize)>, FrameError> {
        let available = self.buffer.len() - consumed;
        let (total, headers_len) = match self.pending {
            Some(pending) => pending,
            None if available < PRELUDE_LEN => return Ok(None),
            None => {
                let pending = inspect_prelude(&self.buffer[consumed..])?;
                self.pending = Some(pending);
                pending
            }
        };
        // `inspect_prelude` bounded `total` by `MAX_FRAME_LEN`, so the
        // widening cast is lossless on the supported platforms.
        let total_len = total as usize;
        if available < total_len {
            return Ok(None);
        }
        let frame = decode_frame(&self.buffer[consumed..], total, headers_len)?;
        self.pending = None;
        Ok(Some((frame, total_len)))
    }

    /// Fails the decoder: nothing buffered is kept, and later input is
    /// ignored. Returns `error` for the caller to yield.
    fn fail(&mut self, error: FrameError) -> FrameError {
        self.failed = true;
        self.pending = None;
        self.buffer = Vec::new();
        error
    }

    /// Ends the input.
    ///
    /// # Errors
    ///
    /// [`FrameError::Truncated`] when a partial frame remains; an ended or
    /// failed decoder emits nothing.
    pub(crate) fn finish(&mut self) -> Result<(), FrameError> {
        if self.failed {
            return Ok(());
        }
        self.finished = true;
        let available = self.buffer.len();
        if available == 0 {
            return Ok(());
        }
        let declared = self.pending.map_or_else(
            || u32::try_from(available).unwrap_or(u32::MAX),
            |(total, _)| total,
        );
        Err(self.fail(FrameError::Truncated {
            declared,
            available,
        }))
    }
}

/// Admits one chunk or ends the input, queueing every produced frame and
/// then the failure, if any. The first [`FrameError`] is queued once and
/// has already failed the decoder, so the loop yields the queued frames and
/// the error before it observes the failure and ends.
async fn pump<S>(
    bytes: &mut std::pin::Pin<Box<S>>,
    decoder: &mut FrameDecoder,
    queue: &mut std::collections::VecDeque<Result<Frame, FrameError>>,
) where
    S: Stream<Item = Vec<u8>> + Send,
{
    let failure = match bytes.next().await {
        Some(chunk) => {
            let pushed = decoder.push(&chunk);
            queue.extend(pushed.frames.into_iter().map(Ok));
            pushed.failure
        }
        None => decoder.finish().err(),
    };
    queue.extend(failure.map(Err));
}

/// Decodes a byte stream of one Bedrock event-stream response into frames.
///
/// End of input ends the stream through [`FrameDecoder::finish`]. A framing
/// failure is yielded once as [`FrameError`]; the family decoder binds it
/// to [`dal_core::Family::Bedrock`]. After the error the stream ends and
/// the byte source is released, which cancels the underlying transfer.
pub(crate) fn decode_stream<S>(
    bytes: S,
) -> impl Stream<Item = Result<Frame, FrameError>> + Send + 'static
where
    S: Stream<Item = Vec<u8>> + Send + 'static,
{
    let state = (
        Box::pin(bytes),
        FrameDecoder::new(),
        std::collections::VecDeque::new(),
    );
    stream::unfold(state, |(mut bytes, mut decoder, mut queue)| async move {
        loop {
            if let Some(item) = queue.pop_front() {
                return Some((item, (bytes, decoder, queue)));
            }
            if decoder.failed || decoder.finished {
                return None;
            }
            pump(&mut bytes, &mut decoder, &mut queue).await;
        }
    })
}

#[cfg(test)]
mod tests;
