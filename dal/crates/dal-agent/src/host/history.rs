//! The assistant turn texts a session holds when it opens.

use dal_core::{Block, EntryKind, Session};

const OPENING_TEXTS_MAX: usize = 32;

/// Extracts the assistant text of each entry on the fold's current
/// root-to-leaf path, oldest first, at most the newest 32.
///
/// An entry's text is its text blocks joined; reasoning and tool-call blocks
/// and entries without text are skipped, so a resumed lane sees what a live
/// lane accumulated.
pub(crate) fn opening_texts(fold: &Session) -> Vec<String> {
    let mut texts: Vec<String> = fold
        .leaf_entries()
        .into_iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .map(|content| {
            content
                .iter()
                .filter_map(|block| match block {
                    Block::Text { text } => Some(&**text),
                    _ => None,
                })
                .collect::<String>()
        })
        .filter(|text| !text.trim().is_empty())
        .collect();
    let excess = texts.len().saturating_sub(OPENING_TEXTS_MAX);
    texts.drain(..excess);
    texts
}
