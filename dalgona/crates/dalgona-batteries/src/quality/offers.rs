// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::fmt::Write as _;

use dal_tools::parse::{CodemodMatch, Language};

use super::codemods::{rethrow_after, whole_lines, whole_source_lines};

/// Codemod name for deleting commented-out code.
pub const DELETE_COMMENTED_CODE: &str = "delete-commented-code";
/// Codemod name for rethrowing from an empty catch handler.
pub const RETHROW_EMPTY_CATCH: &str = "rethrow-empty-catch";

/// Error text for an offer id that does not exist.
#[must_use]
pub fn unknown_offer_error(id: &str) -> String {
    format!("quality_apply: no offer {id}.")
}

/// Error text for an offer whose file changed after it was made.
#[must_use]
pub fn stale_offer_error(path: &str) -> String {
    format!("quality_apply: {path} changed since the offer; the offer is stale.")
}

/// One codemod edit offered to the model, applied only through `quality_apply`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Offer {
    /// Stable offer id, `q-<turn>-<index>`.
    pub id: String,
    /// Name of the codemod that produced the offer.
    pub codemod: String,
    /// Workspace path of the file the offer edits.
    pub path: String,
    /// Byte offset where the replaced span starts.
    pub byte_start: u64,
    /// Byte offset where the replaced span ends.
    pub byte_end: u64,
    /// First 1-based line the offer touches.
    pub line_start: u32,
    /// Last 1-based line the offer touches.
    pub line_end: u32,
    /// Exact bytes the span holds now.
    pub before: String,
    /// Text that replaces the span.
    pub after: String,
}

/// Builds the id of the `index`th offer of `turn`.
#[must_use]
pub fn offer_id(turn: &str, index: usize) -> String {
    format!("q-{turn}-{index}")
}

/// Builds delete-commented-code offers for non-Rust matches that cover whole comment lines.
#[must_use]
pub fn delete_offers(
    turn: &str,
    path: &str,
    bytes: &[u8],
    matches: &[CodemodMatch],
    start_index: usize,
) -> Vec<Offer> {
    let mut offers = Vec::new();
    for matched in matches {
        if matched.language == Language::Rust {
            continue;
        }
        let Some(span) = span_pair(matched) else {
            continue;
        };
        let Some((line_start, line_end, _)) = whole_lines(bytes, span) else {
            continue;
        };
        let Some((byte_start, byte_end, before)) = extended_span(bytes, span) else {
            continue;
        };
        offers.push(Offer {
            id: offer_id(turn, start_index + offers.len() + 1),
            codemod: DELETE_COMMENTED_CODE.to_owned(),
            path: path.to_owned(),
            byte_start,
            byte_end,
            line_start,
            line_end,
            before,
            after: String::new(),
        });
    }
    offers
}

/// Builds rethrow offers for empty catch matches in languages that have a rethrow form.
#[must_use]
pub fn rethrow_offers(
    turn: &str,
    path: &str,
    bytes: &[u8],
    matches: &[CodemodMatch],
    start_index: usize,
) -> Vec<Offer> {
    let mut offers = Vec::new();
    for matched in matches {
        if !rethrow_language(matched.language) {
            continue;
        }
        let Some(span) = rethrow_span(matched, bytes) else {
            continue;
        };
        let Some((line_start, line_end, _)) = whole_source_lines(bytes, span) else {
            continue;
        };
        let Some((byte_start, byte_end, before)) = extended_span(bytes, span) else {
            continue;
        };
        let after = rethrow_after(matched.language, matched.text.as_ref());
        if after.is_empty() {
            continue;
        }
        offers.push(Offer {
            id: offer_id(turn, start_index + offers.len() + 1),
            codemod: RETHROW_EMPTY_CATCH.to_owned(),
            path: path.to_owned(),
            byte_start,
            byte_end,
            line_start,
            line_end,
            before,
            after,
        });
    }
    offers
}

fn rethrow_language(language: Language) -> bool {
    matches!(
        language,
        Language::Python | Language::Cpp | Language::Ocaml | Language::OcamlInterface
    )
}

fn rethrow_span(matched: &CodemodMatch, bytes: &[u8]) -> Option<(u64, u64)> {
    let (start, end) = span_pair(matched)?;
    if matched.language != Language::Ocaml {
        return Some((start, end));
    }
    let start = usize::try_from(start).ok()?;
    let before = bytes.get(..start)?;
    let line_start = before
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let prefix = &bytes[line_start..start];
    let last_nonspace = prefix
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t' | b'\r'));
    if last_nonspace.is_some_and(|index| prefix[index] == b'|') {
        let bar = prefix.iter().rposition(|byte| *byte == b'|')?;
        return Some((u64::try_from(line_start + bar).ok()?, end));
    }
    Some((u64::try_from(start).ok()?, end))
}

fn span_pair(matched: &CodemodMatch) -> Option<(u64, u64)> {
    let start = u64::try_from(matched.byte_start).ok()?;
    let end = u64::try_from(matched.byte_end).ok()?;
    (start < end).then_some((start, end))
}

fn extended_span(bytes: &[u8], span: (u64, u64)) -> Option<(u64, u64, String)> {
    let start = usize::try_from(span.0).ok()?;
    let mut end = usize::try_from(span.1).ok()?;
    if start > end || end > bytes.len() {
        return None;
    }
    if end < bytes.len() && bytes[end] == b'\n' {
        end += 1;
    } else if end + 1 < bytes.len() && bytes[end] == b'\r' && bytes[end + 1] == b'\n' {
        end += 2;
    }
    let before = std::str::from_utf8(&bytes[start..end]).ok()?.to_owned();
    let start = u64::try_from(start).ok()?;
    let end = u64::try_from(end).ok()?;
    Some((start, end, before))
}

/// Renders the per-turn offer panel shown to the model.
#[must_use]
pub fn panel(offers: &[Offer], gaps: &[(&str, &str)]) -> String {
    let mut text = format!("quality: {} codemod offer(s) this turn.\n", offers.len());
    for offer in offers {
        let _ = writeln!(
            text,
            "{} {} {}:{}-{}",
            offer.id, offer.codemod, offer.path, offer.line_start, offer.line_end
        );
    }
    text.push_str("Apply one with quality_apply {\"id\": \"<id>\"}.\n");
    for (path, codemod) in gaps {
        if *codemod == DELETE_COMMENTED_CODE {
            text.push_str(&rust_gap_line(path));
            text.push('\n');
        }
    }
    text
}

/// Explains why a Rust file received no delete-commented-code offer.
#[must_use]
pub fn rust_gap_line(path: &str) -> String {
    format!(
        "no offer for {path}: the delete-commented-code query has no Rust entry (tree-sitter-rust spells comments line_comment and block_comment; the query set names only the shared comment node)."
    )
}
