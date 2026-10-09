// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Strict history letter records and the journal input that feeds them.

use std::collections::HashMap;

use dal_agent::ext::CoveredEntry;
use dal_core::{AssistantPart, ContextItem, EntryId, Part};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::pipeline::SourceReader;
use super::spans::{CompactPiece, Role, SourceError, Span};
/// A strict history letter record body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum LetterRecord {
    /// A skill capture: image fields plus the `letter/<id>` source key.
    Skill {
        /// Record format version, always `1`.
        v: u8,
        /// `<name>` with `.2`, `.3` suffixes for later captures.
        id: String,
        /// Lowercase 64-hex blake3 digest of the PNG bytes.
        png_blob: String,
        /// Encoded PNG length in bytes.
        png_bytes: u32,
        /// Rendered pixel width.
        width: u32,
        /// Rendered pixel height.
        height: u32,
        /// Cell pitch `[w, h]`, each side `1..=64`.
        cell: [u8; 2],
        /// Journal spans; empty for skill captures.
        spans: Vec<Span>,
    },
    /// One compaction image with its exact journal spans.
    Compaction {
        /// Record format version, always `1`.
        v: u8,
        /// `history/<ordinal>.<index>` on the current path.
        id: String,
        /// Lowercase 64-hex blake3 digest of the PNG bytes.
        png_blob: String,
        /// Encoded PNG length in bytes.
        png_bytes: u32,
        /// Rendered pixel width.
        width: u32,
        /// Rendered pixel height.
        height: u32,
        /// Cell pitch `[w, h]`, each side `1..=64`.
        cell: [u8; 2],
        /// Exact journal spans drawn in this image.
        spans: Vec<Span>,
        /// One-based source-letter indexes.
        letters: Vec<u32>,
    },
    /// A consolidated dream summary with no PNG or spans.
    Dream {
        /// Record format version, always `1`.
        v: u8,
        /// `dream/<ordinal>` on the current path.
        id: String,
        /// Consumed letter ids as strings.
        letters: Vec<String>,
        /// The exact dream summary body.
        summary: String,
    },
}

/// A history record that fails structural validation.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub(crate) enum RecordError {
    /// More than 4096 letter references.
    #[error("history record has {count} letters; the limit is 4096.")]
    TooManyLetters {
        /// The rejected reference count.
        count: usize,
    },
    /// More than 65536 spans.
    #[error("history record has {count} spans; the limit is 65536.")]
    TooManySpans {
        /// The rejected span count.
        count: usize,
    },
    /// A skill capture carrying journal spans.
    #[error("history record is a skill capture with {count} spans; skill captures carry no spans.")]
    SkillSpans {
        /// The rejected span count.
        count: usize,
    },
    /// A PNG digest that is not 64 lowercase hexadecimal digits.
    #[error(
        "history record has png_blob \"{digest}\"; the digest must be 64 lowercase hexadecimal digits."
    )]
    BadDigest {
        /// The rejected digest text.
        digest: String,
    },
    /// A cell side outside `1..=64`.
    #[error("history record has cell {width}x{height}; each side must be 1 to 64.")]
    BadCell {
        /// The rejected cell width.
        width: u8,
        /// The rejected cell height.
        height: u8,
    },
    /// A span that cannot name bytes of an earlier entry.
    #[error(
        "history record has a span [{entry},{part},{off},{len}] that does not name bytes of an earlier entry."
    )]
    BadSpan {
        /// The rejected entry counter.
        entry: u64,
        /// The rejected part index.
        part: u32,
        /// The rejected byte offset.
        off: u32,
        /// The rejected byte length.
        len: u32,
    },
    #[error("history record is not a version 1 history body.")]
    Decode,
}

impl LetterRecord {
    /// Returns the letter id.
    #[must_use]
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Skill { id, .. } | Self::Compaction { id, .. } | Self::Dream { id, .. } => id,
        }
    }

    /// Returns the journal spans (`Skill` and `Compaction` only).
    #[must_use]
    pub(crate) fn spans(&self) -> &[Span] {
        match self {
            Self::Skill { spans, .. } | Self::Compaction { spans, .. } => spans,
            Self::Dream { .. } => &[],
        }
    }

    /// Returns the compaction ordinal `k` of a `history/<k>.<i>` record id.
    #[must_use]
    pub(crate) fn compaction_ordinal(&self) -> Option<u32> {
        let Self::Compaction { id, .. } = self else {
            return None;
        };
        let (ordinal, _) = id.strip_prefix("history/")?.split_once('.')?;
        ordinal.parse().ok()
    }

    /// Validates structural caps and entry ordering without journal reads.
    ///
    /// # Errors
    /// Returns the exact cap error for more than 4096 letters, more than
    /// 65536 spans, a skill capture with spans, a malformed PNG digest, a
    /// cell side outside `1..=64`, or a span that cannot name an earlier
    /// entry.
    pub(crate) fn check(record: &Self, record_entry: EntryId) -> Result<(), RecordError> {
        match record {
            Self::Skill {
                png_blob,
                spans,
                cell,
                ..
            } => {
                if !spans.is_empty() {
                    return Err(RecordError::SkillSpans { count: spans.len() });
                }
                check_cell(*cell)?;
                check_digest(png_blob)?;
            }
            Self::Compaction {
                png_blob,
                spans,
                letters,
                cell,
                ..
            } => {
                check_cell(*cell)?;
                check_digest(png_blob)?;
                check_spans(spans, record_entry)?;
                check_letters(letters.len())?;
            }
            Self::Dream { letters, .. } => {
                check_letters(letters.len())?;
            }
        }
        Ok(())
    }
}

fn check_letters(count: usize) -> Result<(), RecordError> {
    if count > 4096 {
        return Err(RecordError::TooManyLetters { count });
    }
    Ok(())
}

fn check_digest(digest: &str) -> Result<(), RecordError> {
    let valid = digest.len() == 64
        && digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if !valid {
        return Err(RecordError::BadDigest {
            digest: digest.to_string(),
        });
    }
    Ok(())
}

fn check_cell(cell: [u8; 2]) -> Result<(), RecordError> {
    if cell[0] < 1 || cell[0] > 64 || cell[1] < 1 || cell[1] > 64 {
        return Err(RecordError::BadCell {
            width: cell[0],
            height: cell[1],
        });
    }
    Ok(())
}

fn check_spans(spans: &[Span], record_entry: EntryId) -> Result<(), RecordError> {
    if spans.len() > 65_536 {
        return Err(RecordError::TooManySpans { count: spans.len() });
    }
    for span in spans {
        check_span(span, record_entry)?;
    }
    Ok(())
}

fn check_span(span: &Span, record_entry: EntryId) -> Result<(), RecordError> {
    let entry = span.entry.get();
    if entry < 1 || entry >= record_entry.get() || span.len < 1 {
        return Err(RecordError::BadSpan {
            entry,
            part: span.part,
            off: span.off,
            len: span.len,
        });
    }
    Ok(())
}

impl LetterRecord {
    /// Decodes one version-1 history body and validates structural caps.
    ///
    /// # Errors
    /// Returns `RecordError::Decode` when the body is not a version-1
    /// history record, or the exact cap error for oversized or
    /// out-of-order spans and letters.
    pub(crate) fn decode(
        body: &dal_core::RawJson,
        record_entry: EntryId,
    ) -> Result<Self, RecordError> {
        let record: Self = sonic_rs::from_str(body.as_str()).map_err(|_| RecordError::Decode)?;
        if record.version() != 1 {
            return Err(RecordError::Decode);
        }
        Self::check(&record, record_entry)?;
        Ok(record)
    }

    /// Returns the record format version.
    fn version(&self) -> u8 {
        match self {
            Self::Skill { v, .. } | Self::Compaction { v, .. } | Self::Dream { v, .. } => *v,
        }
    }
}

/// The exact journal bytes behind the pieces of one compaction span.
///
/// Text and tool-call arguments are copied once from the covered entries.
/// Reasoning text is never copied, because it is never drawn.
#[derive(Debug, Default)]
pub(crate) struct JournalSource {
    parts: HashMap<(u64, u32), Box<[u8]>>,
}

impl SourceReader for JournalSource {
    fn read(&self, span: Span) -> Result<Vec<u8>, SourceError> {
        let Some(bytes) = self.parts.get(&(span.entry.get(), span.part)) else {
            return Err(SourceError {
                message: format!("part {} holds no text from the compacted span", span.part),
            });
        };
        let end = u64::from(span.off) + u64::from(span.len);
        let range = usize::try_from(span.off)
            .ok()
            .zip(usize::try_from(end).ok())
            .and_then(|(start, end)| bytes.get(start..end));
        range.map(<[u8]>::to_vec).ok_or_else(|| SourceError {
            message: format!(
                "bytes {} to {end} are outside the {} bytes of part {}",
                span.off,
                bytes.len(),
                span.part
            ),
        })
    }
}

/// Accumulates source pieces and their bytes in journal order.
#[derive(Default)]
struct PieceBuilder {
    pieces: Vec<CompactPiece>,
    source: JournalSource,
}

impl PieceBuilder {
    /// Adds one whole text part and keeps its bytes.
    fn text(&mut self, entry: EntryId, part: u32, role: Role, text: &str) -> Option<()> {
        let length = u32::try_from(text.len()).ok()?;
        self.source
            .parts
            .insert((entry.get(), part), Box::from(text.as_bytes()));
        self.push(entry, part, role, length, None);
        Some(())
    }

    /// Adds one reasoning part that keeps its place but none of its bytes.
    fn reasoning(&mut self, entry: EntryId, part: u32, text: &str) -> Option<()> {
        let length = u32::try_from(text.len()).ok()?;
        self.push(entry, part, Role::Reasoning, length, None);
        Some(())
    }

    /// Adds one image or stored blob that the history names but never reads.
    ///
    /// An empty part names no bytes and is left out.
    fn picture(&mut self, entry: EntryId, part: u32, role: Role, mime: &str, length: u32) {
        if length > 0 {
            self.push(entry, part, role, length, Some((mime.into(), length)));
        }
    }

    fn push(
        &mut self,
        entry: EntryId,
        part: u32,
        role: Role,
        length: u32,
        picture: Option<(Box<str>, u32)>,
    ) {
        self.pieces.push(CompactPiece {
            entry,
            part,
            off: 0,
            len: length,
            total: length,
            role,
            picture,
        });
    }

    /// Adds the parts of a user message or tool result under one role.
    fn content(&mut self, entry: EntryId, role: &Role, parts: &[Part]) -> Option<()> {
        for (index, part) in parts.iter().enumerate() {
            let index = u32::try_from(index).ok()?;
            match part {
                Part::Text { text } => self.text(entry, index, role.clone(), text)?,
                Part::Image { mime, bytes } => {
                    let length = u32::try_from(bytes.len()).ok()?;
                    self.picture(entry, index, role.clone(), mime, length);
                }
                Part::Blob { mime, bytes, .. } => {
                    let length = u32::try_from(*bytes).ok()?;
                    self.picture(entry, index, role.clone(), mime, length);
                }
            }
        }
        Some(())
    }

    /// Adds the blocks of one assistant message: text, reasoning, and calls.
    fn assistant(&mut self, entry: EntryId, parts: &[AssistantPart]) -> Option<()> {
        for (index, part) in parts.iter().enumerate() {
            let index = u32::try_from(index).ok()?;
            match part {
                AssistantPart::Text { text } => {
                    self.text(entry, index, Role::Assistant, text)?;
                }
                AssistantPart::Thinking { text, .. } => self.reasoning(entry, index, text)?,
                AssistantPart::ToolCall { name, args, .. } => {
                    self.text(entry, index, Role::Call(name.clone()), args.as_str())?;
                }
            }
        }
        Some(())
    }
}

/// Decodes the covered entries of one compaction span into source pieces.
///
/// Each piece carries its journal role: user text, assistant text, tool
/// calls, tool output, failed tool output, or reasoning. The part index of a
/// piece is its position in the covered message, so spans name the same
/// parts that `letter://` reads back from the journal. Images and stored
/// blobs become picture pieces and are never read. The returned
/// [`JournalSource`] serves the exact bytes of every text piece.
///
/// Returns `None` when a part is larger than a span can name (4 GiB).
#[must_use]
pub(crate) fn journal_input(
    covered: &[CoveredEntry],
) -> Option<(Vec<CompactPiece>, JournalSource)> {
    let mut builder = PieceBuilder::default();
    for covered_entry in covered {
        let entry = covered_entry.entry;
        match &covered_entry.content {
            ContextItem::User { parts } => builder.content(entry, &Role::User, parts)?,
            ContextItem::Assistant { parts, .. } => builder.assistant(entry, parts)?,
            ContextItem::ToolResult {
                name,
                is_error,
                parts,
                ..
            } => {
                let role = if *is_error {
                    Role::FailedOutput(name.clone())
                } else {
                    Role::Output(name.clone())
                };
                builder.content(entry, &role, parts)?;
            }
        }
    }
    Some((builder.pieces, builder.source))
}
