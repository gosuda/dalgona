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
    use super::{commit_bytes, shrink_bytes};

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
        assert_eq!(shrink.iter().filter(|byte| **byte == b'K').count(), 2);
        assert!(shrink.windows(3).all(|window| window != b"\x1b[J"));
    }
}
