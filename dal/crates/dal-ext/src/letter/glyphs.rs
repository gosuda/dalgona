//! Strict parsing of GNU Unifont hex glyph data with lazy, cached access to
//! the bundled font.
//!
//! Parsing is all-or-nothing: a [`Glyphs`] table only comes out of input
//! that satisfies every structural rule, so a malformed asset never yields
//! a partial font, a skipped glyph, or a blank tofu cell. The lazy state
//! lives inside the extension-owned [`Font`] value, never in process-global
//! mutable state.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// A single bitmap glyph: 16 rows of pixels, 8 or 16 columns wide.
///
/// Every row stores its pixels in the high bits of the 16-bit cell, so bit
/// 15 is the leftmost pixel of the row. A single-width glyph left-aligns
/// its eight pixel columns in that cell and leaves the low byte empty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Glyph {
    /// Number of pixel columns the bitmap occupies: 8 or 16.
    pub cols: u8,
    /// The 16 pixel rows, most significant bit at the left edge.
    pub rows: [u16; 16],
}

/// Failure to parse Unifont hex data.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GlyphError {
    /// A line violates the `<hex>:<hex>` line grammar, carries invalid hex,
    /// a wrong bitmap length, a code point outside Unicode, or is a blank
    /// line that is not the single blank final line.
    #[error("unifont hex line {line} is malformed")]
    Malformed {
        /// One-based number of the offending line.
        line: usize,
    },
    /// A code point appears in more than one line.
    #[error("unifont hex line {line} duplicates codepoint U+{codepoint:04X}")]
    Duplicate {
        /// One-based number of the offending line.
        line: usize,
        /// The repeated code point.
        codepoint: u32,
    },
}

/// An immutable code point to glyph table parsed from Unifont hex data.
#[derive(Debug)]
pub struct Glyphs {
    table: BTreeMap<u32, Glyph>,
}

impl Glyphs {
    /// Parses a complete Unifont hex table from raw file bytes.
    ///
    /// Every non-blank line is `<codepoint hex>:<bitmap hex>` with 4 to 6
    /// hexadecimal code point digits and exactly 32 or 64 hexadecimal
    /// bitmap digits: 16 rows of one or two bytes. A 32-digit bitmap has
    /// `cols = 8` and is left-aligned in the 16-bit row cell; a 64-digit
    /// bitmap has `cols = 16`. Rows are stored big-endian with the most
    /// significant bit at the left edge. Exactly one blank final line is
    /// allowed; any other blank line is malformed. An empty input yields
    /// an empty table.
    ///
    /// # Errors
    /// Returns [`GlyphError::Malformed`] for the first line that breaks
    /// the line grammar, carries invalid or non-UTF-8 bytes, names a
    /// surrogate or another code point outside Unicode, or has a
    /// wrong-length bitmap, and [`GlyphError::Duplicate`] for a repeated
    /// code point.
    pub fn from_hex(bytes: &[u8]) -> Result<Self, GlyphError> {
        let mut lines: Vec<&[u8]> = bytes.split(|&byte| byte == b'\n').collect();
        if lines.last().is_some_and(|line| line.is_empty()) {
            // The final newline of the last record is not a blank line.
            lines.pop();
        }
        let mut table = BTreeMap::new();
        let mut blank_final_seen = false;
        for (index, line) in lines.iter().enumerate() {
            let line_no = index + 1;
            if line.is_empty() {
                if index + 1 != lines.len() || blank_final_seen {
                    return Err(GlyphError::Malformed { line: line_no });
                }
                blank_final_seen = true;
                continue;
            }
            parse_line(line, line_no, &mut table)?;
        }
        Ok(Self { table })
    }

    /// Returns the glyph for `codepoint`, or `None` when the table has no
    /// glyph for it. An absent glyph is never treated as a blank glyph.
    #[must_use]
    pub fn find(&self, codepoint: u32) -> Option<Glyph> {
        self.table.get(&codepoint).copied()
    }

    /// Returns whether the pixel at `row` and `col` of `glyph` is ink.
    /// Out-of-range rows and columns read as blank.
    #[must_use]
    pub fn pixel(&self, glyph: Glyph, row: usize, col: usize) -> bool {
        if col >= usize::from(glyph.cols) {
            return false;
        }
        let Some(&bits) = glyph.rows.get(row) else {
            return false;
        };
        ((bits >> (15 - col)) & 1) == 1
    }
}

fn parse_line(
    line: &[u8],
    line_no: usize,
    table: &mut BTreeMap<u32, Glyph>,
) -> Result<(), GlyphError> {
    let malformed = || GlyphError::Malformed { line: line_no };
    let text = std::str::from_utf8(line).map_err(|_| malformed())?;
    let Some((codepoint_text, bitmap_text)) = text.split_once(':') else {
        return Err(malformed());
    };
    if !(4..=6).contains(&codepoint_text.len()) {
        return Err(malformed());
    }
    if bitmap_text.len() != 32 && bitmap_text.len() != 64 {
        return Err(malformed());
    }
    let Some(codepoint) = hex_value(codepoint_text.as_bytes()) else {
        return Err(malformed());
    };
    if char::from_u32(codepoint).is_none() {
        return Err(malformed());
    }
    let cols = if bitmap_text.len() == 32 { 8 } else { 16 };
    let digit_rows = bitmap_text.len() / 16;
    let mut rows = [0_u16; 16];
    for (slot, row_digits) in rows
        .iter_mut()
        .zip(bitmap_text.as_bytes().chunks(digit_rows))
    {
        let Some(value) = hex_value(row_digits) else {
            return Err(malformed());
        };
        *slot = if cols == 8 {
            // A single-width row left-aligns its byte in the 16-bit cell.
            u16::try_from(value).map_err(|_| malformed())? << 8
        } else {
            u16::try_from(value).map_err(|_| malformed())?
        };
    }
    let glyph = Glyph { cols, rows };
    if table.insert(codepoint, glyph).is_some() {
        return Err(GlyphError::Duplicate {
            line: line_no,
            codepoint,
        });
    }
    Ok(())
}

fn hex_value(digits: &[u8]) -> Option<u32> {
    let mut value = 0_u32;
    for &digit in digits {
        let nibble = match digit {
            b'0'..=b'9' => u32::from(digit - b'0'),
            b'a'..=b'f' => u32::from(digit - b'a') + 10,
            b'A'..=b'F' => u32::from(digit - b'A') + 10,
            _ => return None,
        };
        value = value.checked_mul(16)?.checked_add(nibble)?;
    }
    Some(value)
}

/// Lazy handle to an immutable Unifont hex blob, usually the bundled asset.
///
/// The first call to [`Font::glyphs`] parses the bytes and caches either
/// the complete table or the exact parse error; later calls replay the
/// cached result without re-parsing.
#[derive(Debug)]
pub struct Font {
    bytes: &'static [u8],
    cache: OnceLock<Result<Glyphs, GlyphError>>,
}

impl Font {
    /// Returns a font over the bundled uncompressed Unifont asset.
    #[must_use]
    pub fn embedded() -> Self {
        Self {
            bytes: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/unifont-18.0.01.hex"
            )),
            cache: OnceLock::new(),
        }
    }

    /// Returns a font over caller-provided static bytes, for tests.
    #[must_use]
    pub fn from_bytes_for_test(bytes: &'static [u8]) -> Self {
        Self {
            bytes,
            cache: OnceLock::new(),
        }
    }

    /// Parses the font on the first call and caches success or error.
    ///
    /// # Errors
    /// Returns the cached [`GlyphError`] when the bytes do not form a
    /// complete, duplicate-free Unifont table.
    pub fn glyphs(&self) -> Result<&Glyphs, GlyphError> {
        self.cache
            .get_or_init(|| Glyphs::from_hex(self.bytes))
            .as_ref()
            .map_err(Clone::clone)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use super::{Font, Glyph, GlyphError, Glyphs};
    use crate::letter::layout::oracle::{
        FONT_HEX, Table, embedded_font, expected_image, parse_font,
    };
    use crate::letter::layout::{DrawOutcome, draw};
    use proptest::prelude::*;

    /// Characters with known Unifont coverage: ASCII, Latin-1, Hangul, and
    /// BMP CJK, mixing single-width and double-width cells.
    const POOL: &[char] = &[
        'A', 'b', 'x', '0', '!', '.', ' ', 'e', 'ß', 'ñ', '한', '글', '秋', '字', '一',
    ];

    fn pool_char() -> impl Strategy<Value = char> {
        (0..POOL.len()).prop_map(|index| POOL.get(index).copied().unwrap())
    }

    static ORACLE_TABLE: LazyLock<Table> = LazyLock::new(|| parse_font(FONT_HEX).unwrap());

    fn oracle_table() -> &'static Table {
        &ORACLE_TABLE
    }

    fn assert_malformed(bytes: &[u8], line: usize) {
        let Err(error) = Glyphs::from_hex(bytes) else {
            panic!("input must be rejected: {bytes:?}");
        };
        assert_eq!(error, GlyphError::Malformed { line });
    }

    #[test]
    fn font_asset_integrity() {
        let Err(error) = Font::from_bytes_for_test(b"0041:00\n").glyphs() else {
            panic!("a short bitmap must be rejected");
        };
        assert_eq!(error, GlyphError::Malformed { line: 1 });
        assert_eq!(error.to_string(), "unifont hex line 1 is malformed");

        let duplicated = b"0041:00000000000000000000000000000000\n\
                           0041:00000000000000000000000000000000\n";
        let Err(error) = Font::from_bytes_for_test(duplicated).glyphs() else {
            panic!("a repeated codepoint must be rejected");
        };
        assert_eq!(
            error,
            GlyphError::Duplicate {
                line: 2,
                codepoint: 0x41
            }
        );
        assert_eq!(
            error.to_string(),
            "unifont hex line 2 duplicates codepoint U+0041"
        );

        let glyphs = embedded_font().glyphs().unwrap();
        assert_eq!(glyphs.find(u32::from('A')).unwrap().cols, 8);
        assert_eq!(glyphs.find(u32::from('한')).unwrap().cols, 16);
        assert!(glyphs.find(0x1F600).is_none());
    }

    #[test]
    fn glyph_parse_boundaries() {
        let zeros = "0".repeat(32);
        // A valid narrow glyph left-aligns its byte: row 0x00FF becomes 0xFF00.
        let narrow = format!("0041:FF{}\n", "0".repeat(30));
        let table = Glyphs::from_hex(narrow.as_bytes()).unwrap();
        let glyph: Glyph = table.find(u32::from('A')).unwrap();
        assert_eq!(glyph.cols, 8);
        assert_eq!(glyph.rows.first(), Some(&0xFF00));
        assert!(table.pixel(glyph, 0, 0));
        assert!(table.pixel(glyph, 0, 7));
        assert!(!table.pixel(glyph, 0, 8));
        assert!(!table.pixel(glyph, 0, 15));
        assert!(!table.pixel(glyph, 1, 0));
        assert!(!table.pixel(glyph, 16, 0));

        // A valid wide glyph keeps big-endian rows verbatim.
        let wide = format!("4E00:00FF{}\n", "0".repeat(60));
        let table = Glyphs::from_hex(wide.as_bytes()).unwrap();
        let glyph: Glyph = table.find(0x4E00).unwrap();
        assert_eq!(glyph.cols, 16);
        assert_eq!(glyph.rows.first(), Some(&0x00FF));
        assert!(!table.pixel(glyph, 0, 7));
        assert!(table.pixel(glyph, 0, 8));
        assert!(table.pixel(glyph, 0, 15));
        assert!(!table.pixel(glyph, 0, 16));

        // One blank final line is allowed, two are not.
        let blank_final = format!("0041:{zeros}\n\n");
        assert!(Glyphs::from_hex(blank_final.as_bytes()).is_ok());
        let two_blanks = format!("0041:{zeros}\n\n\n");
        assert_malformed(two_blanks.as_bytes(), 2);

        // Blank lines anywhere else are rejected with their line number.
        let leading_blank = format!("\n0041:{zeros}\n");
        assert_malformed(leading_blank.as_bytes(), 1);
        let interior_blank = format!("0041:{zeros}\n\n0042:{zeros}\n");
        assert_malformed(interior_blank.as_bytes(), 2);

        // Code points need 4 to 6 hex digits.
        let short_codepoint = format!("041:{zeros}\n");
        assert_malformed(short_codepoint.as_bytes(), 1);
        let long_codepoint = format!("0000041:{zeros}\n");
        assert_malformed(long_codepoint.as_bytes(), 1);

        // Code points outside Unicode (surrogates and beyond U+10FFFF).
        let surrogate = format!("D800:{zeros}\n");
        assert_malformed(surrogate.as_bytes(), 1);
        let beyond_unicode = format!("110000:{zeros}\n");
        assert_malformed(beyond_unicode.as_bytes(), 1);

        // Bitmaps must be exactly 32 or 64 hex digits.
        let long_bitmap = format!("0041:{zeros}0\n");
        assert_malformed(long_bitmap.as_bytes(), 1);
        let odd_bitmap = format!("0041:0{}\n", "0".repeat(64));
        assert_malformed(odd_bitmap.as_bytes(), 1);

        // Invalid hex, separators, and non-UTF-8 bytes are malformed.
        let bad_digit = format!("0041:{}\n", "g".repeat(32));
        assert_malformed(bad_digit.as_bytes(), 1);
        let no_colon = format!("0041{zeros}\n");
        assert_malformed(no_colon.as_bytes(), 1);
        let second_colon = format!("0041:{zeros}:00\n");
        assert_malformed(second_colon.as_bytes(), 1);
        assert_malformed(&[0xFF, b'\n'], 1);

        // Empty input parses to an empty table with nothing in it.
        let empty = Glyphs::from_hex(b"").unwrap();
        assert!(empty.find(u32::from('A')).is_none());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]
        #[test]
        fn glyph_oracle_property(chars in prop::collection::vec(pool_char(), 0..=64)) {
            let text: String = chars.into_iter().collect();
            let expected = expected_image(oracle_table(), &text);
            let (outcome, image) = draw(embedded_font(), &text).unwrap();
            let Some((want_width, want_height, want_pixels)) = expected else {
                assert_eq!(outcome, DrawOutcome::Fallback { first_undrawable: None });
                assert!(image.is_none());
                return Ok(());
            };
            assert_eq!(outcome, DrawOutcome::Drawn);
            let image = image.unwrap();
            assert_eq!(image.width, want_width);
            assert_eq!(image.height, want_height);
            let (got_width, got_height, got_pixels) =
                crate::letter::layout::oracle::decode_png(&image.png);
            assert_eq!((got_width, got_height), (want_width, want_height));
            assert_eq!(got_pixels, want_pixels);
        }
    }
}
