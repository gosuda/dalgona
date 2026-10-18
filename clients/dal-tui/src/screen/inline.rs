//! Owned inline driver: region arithmetic, commits, synchronized brackets.
//!
//! The stock ratatui inline viewport is never used: its resize duplicates
//! content into scrollback. This layer owns row arithmetic; ratatui diffs
//! cells only.

/// Live region position owned by the driver.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Region {
    /// Screen row of the live block's first row.
    pub top: u16,
    /// Current live block height in rows.
    pub height: u16,
}

/// Where the stale live block sits when a frame starts on the main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// No resize since the last paint: the block is at the recorded anchor.
    Anchor,
    /// The terminal reported the caret: the stale block's first row, 1-based.
    Located(u16),
    /// Position unknown or untrusted: write no live bytes this frame. The
    /// stale block stays on screen and the next frame measures again.
    Hold,
}

/// Returns the stale block's first row from a reported caret row and the
/// caret's offset below the block top at the last paint, plus any extra
/// wrapped rows the width change added at or above the caret (`piece` and
/// `reflow_extra` are zero on a height-only resize). A block whose top
/// scrolled off the screen starts at the second row.
#[must_use]
pub(crate) fn located_top(reported_row: u16, offset: u16, piece: u16, extra: u16) -> u16 {
    reported_row
        .saturating_sub(offset)
        .saturating_sub(piece)
        .saturating_sub(extra)
        .max(1)
}

/// Returns the wrap piece the caret occupies after a width change: the caret
/// cell index (`0`-based column of the caret on its row at the old width)
/// divided by the new width. `0` when the caret still lands on the first
/// wrapped piece or when the width did not change (`piece` is only called on
/// width changes by the driver).
#[must_use]
pub(crate) fn piece(cell_index_before_caret: usize, new_width: usize) -> u16 {
    u16::try_from(cell_index_before_caret / new_width.max(1)).unwrap_or(u16::MAX)
}

/// Returns the wrapped height of one known live row at `width` in cells.
/// An empty row still occupies one row; `width` of 0 is treated as 1.
#[must_use]
pub(crate) fn wrapped_height(cells: usize, width: usize) -> u16 {
    let width = width.max(1);
    u16::try_from(cells.max(1).div_ceil(width)).unwrap_or(u16::MAX)
}

/// Returns the extra rows a width change adds above the caret: the sum of
/// `wrapped_height(cells, new_width) - 1` over the given above-caret live
/// rows. Callers pass only rows whose reflow cannot be verified.
#[must_use]
pub(crate) fn reflow_extra(above_caret_cells: &[usize], new_width: usize) -> u16 {
    above_caret_cells.iter().fold(0_u16, |extra, cells| {
        extra.saturating_add(wrapped_height(*cells, new_width).saturating_sub(1))
    })
}

/// Builds the byte sequence committing `rows` above the live region.
///
/// Every line ends fully SGR-reset; the commit sits in one sync bracket pair
/// when synchronized updates are supported. Committed rows are never erased.
#[must_use]
pub fn commit_bytes(rows: &[String], top: u16, sync: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if sync {
        bytes.extend_from_slice(b"\x1b[?2026h");
    }
    bytes.extend_from_slice(format!("\x1b[{top};1H").as_bytes());
    for row in rows {
        bytes.extend_from_slice(row.as_bytes());
        bytes.extend_from_slice(b"\x1b[0m\r\n");
    }
    if sync {
        bytes.extend_from_slice(b"\x1b[?2026l");
    }
    bytes
}

/// Builds the byte sequence shrinking the region by `surplus` rows.
///
/// Erasing touches only rows the previous frame's block occupied, using EL
/// per surplus row plus CUD motion and CUU return, in one bracket pair.
#[must_use]
pub fn shrink_bytes(first_surplus_row: u16, surplus: u16, sync: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if sync {
        bytes.extend_from_slice(b"\x1b[?2026h");
    }
    bytes.extend_from_slice(format!("\x1b[{first_surplus_row};1H").as_bytes());
    for _ in 0..surplus {
        bytes.extend_from_slice(b"\x1b[2K\x1b[1B");
    }
    bytes.extend_from_slice(format!("\x1b[{surplus}A").as_bytes());
    if sync {
        bytes.extend_from_slice(b"\x1b[?2026l");
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::{commit_bytes, located_top, piece, reflow_extra, shrink_bytes, wrapped_height};

    #[test]
    fn commit_and_shrink_use_one_bracket_pair_without_erase_display() {
        let commit = commit_bytes(&["hello".to_owned()], 20, true);
        assert!(commit.starts_with(b"\x1b[?2026h"));
        assert!(commit.ends_with(b"\x1b[?2026l"));
        assert!(commit.windows(3).all(|window| window != b"\x1b[J"));
        assert!(
            commit
                .windows(4)
                .all(|window| window != b"\x1b[2J" && window != b"\x1b[3J")
        );

        let shrink = shrink_bytes(22, 2, true);
        assert_eq!(
            shrink.iter().filter(|byte| matches!(**byte, b'K')).count(),
            2
        );
        assert!(shrink.windows(3).all(|window| window != b"\x1b[J"));
    }

    #[test]
    fn located_top_clamps_at_row_one() {
        assert_eq!(located_top(9, 0, 0, 0), 9);
        assert_eq!(located_top(16, 2, 0, 0), 14);
        assert_eq!(located_top(4, 8, 0, 0), 1);
        assert_eq!(located_top(10, 1, 2, 3), 4);
    }

    #[test]
    fn piece_counts_wrapped_pieces_before_the_caret() {
        assert_eq!(piece(0, 60), 0);
        assert_eq!(piece(59, 60), 0);
        assert_eq!(piece(60, 60), 1);
        assert_eq!(piece(200, 60), 3);
        assert_eq!(piece(10, 0), 10);
    }

    #[test]
    fn wrapped_height_and_reflow_extra_measure_known_rows() {
        assert_eq!(wrapped_height(0, 60), 1);
        assert_eq!(wrapped_height(60, 60), 1);
        assert_eq!(wrapped_height(61, 60), 2);
        assert_eq!(wrapped_height(180, 60), 3);
        assert_eq!(reflow_extra(&[30, 61, 120], 60), 3);
        assert_eq!(reflow_extra(&[], 60), 0);
    }
}
