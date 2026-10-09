// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Oldest-plus-newest image selection and index text.
use std::num::NonZeroU64;

use dal_core::EntryId;

/// Selects image candidates oldest-plus-newest under one cap.
///
/// Keeps the oldest candidate when it fits, then walks newest to oldest while
/// each next candidate fits, and emits the kept set in history order.
pub(crate) fn select_oldest_plus_newest(
    count: usize,
    mut fits: impl FnMut(usize) -> bool,
) -> Vec<usize> {
    if count == 0 {
        return Vec::new();
    }
    let mut kept = Vec::new();
    if fits(0) {
        kept.push(0);
    }
    for index in (1..count).rev() {
        if fits(index) {
            kept.push(index);
        } else {
            break;
        }
    }
    kept.sort_unstable();
    kept.dedup();
    kept
}

/// Selects oldest-plus-newest candidates while their bytes fit one budget.
///
/// The byte total is charged in selection order: oldest first, then newest
/// toward the middle. A candidate that would exceed the budget stops that
/// direction, matching the other compaction caps.
pub(crate) fn select_oldest_plus_newest_by_bytes(sizes: &[usize], budget: usize) -> Vec<usize> {
    let mut used = 0_usize;
    select_oldest_plus_newest(sizes.len(), |index| {
        let Some(next) = used.checked_add(sizes[index]) else {
            return false;
        };
        if next > budget {
            return false;
        }
        used = next;
        true
    })
}

/// Formats the compaction index text.
#[must_use]
pub(crate) fn index_text(shown: usize, total: usize, hidden: &str) -> String {
    if shown >= total {
        format!("All {total} history images of this compaction are shown above.")
    } else {
        format!(
            "{shown} of {total} history images are shown above. Not shown: {hidden}. Read letter://<id> for the exact text of any image, or read letter:// for the list."
        )
    }
}

/// Visibility state of a history letter in one compaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the history image contract reserves these visibility states before producers land"
    )
)]
pub(crate) enum LetterVisibility {
    /// The PNG is drawn in the compacted message.
    Drawn,
    /// The source is shown as text because its glyph cannot be drawn.
    ShownAsText,
    /// The image is hidden by a compaction budget.
    NotDrawn,
}

/// Returns the history index line for one letter.
#[must_use]
pub(crate) fn history_index_line(
    id: &str,
    first: u64,
    last: u64,
    visibility: LetterVisibility,
) -> String {
    let mut line = format!("letter://{id}  history image, entries {first}-{last}");
    match visibility {
        LetterVisibility::Drawn => {}
        LetterVisibility::ShownAsText => line.push_str(", shown as text"),
        LetterVisibility::NotDrawn => line.push_str(", not drawn"),
    }
    line
}

/// Decodes an entry counter.
#[must_use]
pub(crate) fn entry_id(value: u64) -> Option<EntryId> {
    NonZeroU64::new(value).map(EntryId::new)
}
