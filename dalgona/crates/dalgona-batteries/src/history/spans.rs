use std::{num::NonZeroU64, ops::Range};

use dal_core::EntryId;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, SeqAccess, Visitor},
    ser::SerializeTuple,
};
use thiserror::Error;

/// One journal source range, serialized as `[entry, part, off, len]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Span {
    /// Journal entry holding the source bytes.
    pub(crate) entry: EntryId,
    /// Source part index within the entry.
    pub(crate) part: u32,
    /// Byte offset within the part.
    pub(crate) off: u32,
    /// Byte length of the range.
    pub(crate) len: u32,
}

impl Serialize for Span {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tuple = serializer.serialize_tuple(4)?;
        tuple.serialize_element(&self.entry.get())?;
        tuple.serialize_element(&self.part)?;
        tuple.serialize_element(&self.off)?;
        tuple.serialize_element(&self.len)?;
        tuple.end()
    }
}

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SpanVisitor;

        impl<'de> Visitor<'de> for SpanVisitor {
            type Value = Span;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a four-element [entry, part, offset, length] array")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let entry = seq
                    .next_element::<u64>()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let part = seq
                    .next_element::<u32>()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;
                let off = seq
                    .next_element::<u32>()?
                    .ok_or_else(|| de::Error::invalid_length(2, &self))?;
                let len = seq
                    .next_element::<u32>()?
                    .ok_or_else(|| de::Error::invalid_length(3, &self))?;
                if seq.next_element::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(5, &self));
                }
                let entry = NonZeroU64::new(entry)
                    .ok_or_else(|| de::Error::custom("span entry must be nonzero"))?;
                Ok(Span {
                    entry: EntryId::new(entry),
                    part,
                    off,
                    len,
                })
            }
        }

        deserializer.deserialize_tuple(4, SpanVisitor)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    User,
    Assistant,
    Call(Box<str>),
    Output(Box<str>),
    FailedOutput(Box<str>),
    Note,
    Reasoning,
}

impl Role {
    pub(crate) fn mark(&self) -> Option<Box<str>> {
        match self {
            Self::User => Some("¶user: ".into()),
            Self::Assistant => Some("¶ai: ".into()),
            Self::Call(tool) => Some(format!("¶call:{tool} ").into_boxed_str()),
            Self::Output(tool) => Some(format!("¶out:{tool} ").into_boxed_str()),
            Self::FailedOutput(tool) => Some(format!("¶failed:{tool} ").into_boxed_str()),
            Self::Note => Some("¶note: ".into()),
            Self::Reasoning => None,
        }
    }

    pub(crate) fn words(&self) -> String {
        match self {
            Self::User => "user".to_string(),
            Self::Assistant => "assistant".to_string(),
            Self::Call(tool) => format!("call {tool}"),
            Self::Output(tool) => format!("output {tool}"),
            Self::FailedOutput(tool) => format!("failed output {tool}"),
            Self::Note => "note".to_string(),
            Self::Reasoning => "reasoning".to_string(),
        }
    }

    fn is_tool_text(&self) -> bool {
        matches!(
            self,
            Self::Call(_) | Self::Output(_) | Self::FailedOutput(_)
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Item {
    Mark(Box<str>),
    Text {
        span: Span,
        role: Role,
        total: u32,
        text: Box<str>,
    },
    Picture {
        span: Span,
        mime: Box<str>,
        bytes: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Retained {
    Text(Range<usize>),
    Gap(u32),
}

const BASE64_PREFIX: &[u8] = b";base64,";
const BASE64_MIN_RUN: usize = 256;

pub(crate) fn retained_segments(text: &str, role: &Role) -> Option<Vec<Retained>> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return Some(Vec::new());
    }

    let is_truncated = role.is_tool_text() && bytes.len() > 2_000;
    let head_end = if is_truncated {
        utf8_floor(bytes, 1_200)
    } else {
        bytes.len()
    };
    let tail_start = if is_truncated {
        utf8_ceil(bytes, bytes.len().saturating_sub(800).max(head_end))
    } else {
        bytes.len()
    };
    let redactions = base64_ranges(bytes);
    let mut result = Vec::new();
    append_segmented_range(0..head_end, &redactions, &mut result)?;
    if is_truncated {
        let omitted = u32::try_from(tail_start - head_end).ok()?;
        if omitted > 0 {
            result.push(Retained::Gap(omitted));
        }
        append_segmented_range(tail_start..bytes.len(), &redactions, &mut result)?;
    }
    Some(result)
}

fn base64_ranges(bytes: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut search = 0;
    while search < bytes.len() {
        let Some(relative) = find_subslice(&bytes[search..], BASE64_PREFIX) else {
            break;
        };
        let run_start = search + relative + BASE64_PREFIX.len();
        let mut run_end = run_start;
        while run_end < bytes.len() && is_base64(bytes[run_end]) {
            run_end += 1;
        }
        if run_end - run_start >= BASE64_MIN_RUN {
            ranges.push(run_start..run_end);
        }
        search = run_end.max(run_start);
    }
    ranges
}

fn append_segmented_range(
    range: Range<usize>,
    redactions: &[Range<usize>],
    result: &mut Vec<Retained>,
) -> Option<()> {
    let mut cursor = range.start;
    for redaction in redactions {
        let start = range.start.max(redaction.start);
        let end = range.end.min(redaction.end);
        if start >= end {
            continue;
        }
        if cursor < start {
            result.push(Retained::Text(cursor..start));
        }
        result.push(Retained::Gap(u32::try_from(end - start).ok()?));
        cursor = end;
    }
    if cursor < range.end {
        result.push(Retained::Text(cursor..range.end));
    }
    Some(())
}

fn utf8_floor(bytes: &[u8], end: usize) -> usize {
    let mut end = end.min(bytes.len());
    while end > 0 && !is_utf8_boundary(bytes, end) {
        end -= 1;
    }
    end
}

fn utf8_ceil(bytes: &[u8], start: usize) -> usize {
    let mut start = start.min(bytes.len());
    while start < bytes.len() && !is_utf8_boundary(bytes, start) {
        start += 1;
    }
    start
}

fn is_utf8_boundary(bytes: &[u8], index: usize) -> bool {
    index == bytes.len()
        || bytes
            .get(index)
            .is_some_and(|byte| byte & 0b1100_0000 != 0b1000_0000)
}

fn is_base64(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// One journal source piece used to build history image items.
///
/// The compaction host provides these exact source ranges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompactPiece {
    /// Journal entry holding the source bytes.
    pub(crate) entry: EntryId,
    /// Source part index within the entry.
    pub(crate) part: u32,
    /// Byte offset of the source range.
    pub(crate) off: u32,
    /// Byte length of the source range.
    pub(crate) len: u32,
    /// Total bytes of the source part.
    pub(crate) total: u32,
    /// Speaker or tool role of the source range.
    pub(crate) role: Role,
    /// Image source: `(mime, bytes)` for picture pieces, `None` for text.
    pub(crate) picture: Option<(Box<str>, u32)>,
}

/// A journal byte read failure.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("{message}")]
pub(crate) struct SourceError {
    /// Why the source bytes could not be read.
    pub(crate) message: String,
}

/// A history source-stream build failure.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub(crate) enum HistoryError {
    /// The fetch closure failed for one span.
    #[error("cannot read the journal text of entry {entry}: {message}")]
    Fetch {
        /// The entry whose bytes could not be read.
        entry: u64,
        /// The fetch closure's message.
        message: String,
    },
}

/// Builds the ordered history item stream in journal order.
///
/// Reasoning pieces emit nothing. Empty text emits its role mark without a
/// span. Tool text longer than 2000 bytes keeps a UTF-8-safe 1200-byte head
/// and 800-byte tail with a gap mark. Base64 runs after `;base64,` of at
/// least 256 bytes are elided to gap marks; emitted spans never name elided
/// bytes. The whole piece range is read once for segmentation, then each
/// retained span is fetched separately, so every emitted span names bytes
/// the build actually read.
///
/// # Errors
/// Returns `HistoryError::Fetch` when any fetch fails, aborting the whole
/// build without partial source text.
pub(crate) fn items(
    pieces: &[CompactPiece],
    mut fetch: impl FnMut(Span) -> Result<Vec<u8>, SourceError>,
) -> Result<Vec<Item>, HistoryError> {
    let mut out = Vec::new();
    for piece in pieces {
        if matches!(piece.role, Role::Reasoning) {
            continue;
        }
        if let Some((mime, bytes)) = piece.picture.as_ref() {
            out.push(Item::Picture {
                span: Span {
                    entry: piece.entry,
                    part: piece.part,
                    off: piece.off,
                    len: piece.len,
                },
                mime: mime.clone(),
                bytes: *bytes,
            });
            continue;
        }
        let whole = Span {
            entry: piece.entry,
            part: piece.part,
            off: piece.off,
            len: piece.len,
        };
        let bytes = fetch(whole).map_err(|error| HistoryError::Fetch {
            entry: piece.entry.get(),
            message: error.message,
        })?;
        let text = String::from_utf8(bytes).map_err(|error| HistoryError::Fetch {
            entry: piece.entry.get(),
            message: error.to_string(),
        })?;
        if let Some(mark) = piece.role.mark() {
            out.push(Item::Mark(mark));
        }
        if text.is_empty() {
            continue;
        }
        let Some(segments) = retained_segments(&text, &piece.role) else {
            continue;
        };
        for segment in segments {
            match segment {
                Retained::Text(range) => {
                    out.push(fetch_text(piece, range, &mut fetch)?);
                }
                Retained::Gap(omitted) => {
                    out.push(Item::Mark(gap_mark(omitted)));
                }
            }
        }
    }
    Ok(out)
}

fn fetch_text(
    piece: &CompactPiece,
    range: Range<usize>,
    fetch: &mut impl FnMut(Span) -> Result<Vec<u8>, SourceError>,
) -> Result<Item, HistoryError> {
    let start = u32::try_from(range.start).map_err(|_| HistoryError::Fetch {
        entry: piece.entry.get(),
        message: "span offset exceeds u32".to_string(),
    })?;
    let length = u32::try_from(range.len()).map_err(|_| HistoryError::Fetch {
        entry: piece.entry.get(),
        message: "span length exceeds u32".to_string(),
    })?;
    let Some(off) = piece.off.checked_add(start) else {
        return Err(HistoryError::Fetch {
            entry: piece.entry.get(),
            message: "span offset exceeds u32".to_string(),
        });
    };
    let span = Span {
        entry: piece.entry,
        part: piece.part,
        off,
        len: length,
    };
    let retained = fetch(span).map_err(|error| HistoryError::Fetch {
        entry: piece.entry.get(),
        message: error.message,
    })?;
    let retained = String::from_utf8(retained).map_err(|error| HistoryError::Fetch {
        entry: piece.entry.get(),
        message: error.to_string(),
    })?;
    Ok(Item::Text {
        span,
        role: piece.role.clone(),
        total: piece.total,
        text: retained.into(),
    })
}

fn gap_mark(omitted: u32) -> Box<str> {
    format!("¶gap:{omitted} bytes").into_boxed_str()
}
