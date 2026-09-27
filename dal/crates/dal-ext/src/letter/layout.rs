//! Deterministic text layout over the Unifont glyph table and one-bit
//! grayscale PNG encoding.
//!
//! Layout is pure: equal font bytes and equal input always produce equal
//! image bytes. An undrawable character or an all-empty description falls
//! back as a whole before any pixel is painted, so a partial image or a
//! replacement glyph can never escape this module.

use std::sync::Arc;

use png::{BitDepth, ColorType, DeflateCompression, Encoder, Filter};
use unicode_width::UnicodeWidthChar;

use super::glyphs::{Font, Glyph, GlyphError, Glyphs};

/// Pixel width of one single-width glyph cell.
pub const SINGLE_W: u32 = 8;
/// Pixel width of one double-width glyph cell.
pub const DOUBLE_W: u32 = 16;
/// Bitmap height of every glyph in pixel rows.
pub const GLYPH_H: u32 = 16;
/// Pixel height of one laid-out line.
pub const LINE_H: u32 = 16;
/// White margin in pixels on every side of the image.
pub const MARGIN: u32 = 8;
/// Line length in glyph cells at which wrapping occurs.
pub const WRAP_CELLS: u32 = 96;
/// Maximum description length admitted by the letter budget layer.
pub const MAX_DESC_CHARS: usize = 4096;
/// TAB advance quantum in glyph cells.
pub const TAB_STOP: u32 = 4;

/// A rendered image: deterministic PNG bytes plus its geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Image {
    /// Complete PNG byte stream: one `IHDR`, one `IDAT`, one `IEND`.
    pub png: Arc<[u8]>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
}

/// Result of a pure [`draw`] call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrawOutcome {
    /// Every character was drawable and painted.
    Drawn,
    /// The description fell back as a whole and nothing was painted.
    Fallback {
        /// First undrawable code point, or `None` when the description
        /// was all-empty.
        first_undrawable: Option<u32>,
    },
}

/// Failure of a [`draw`] call.
#[derive(Debug, thiserror::Error)]
pub enum DrawError {
    /// The font table failed to parse.
    #[error(transparent)]
    Glyph(#[from] GlyphError),
    /// PNG encoding failed.
    #[error("letter PNG encode failed: {0}")]
    Png(String),
}

#[derive(Debug, Default)]
struct Line {
    cells: Vec<(usize, Glyph)>,
    advance_cells: usize,
}

impl Line {
    fn is_blank(&self) -> bool {
        self.cells.is_empty() && self.advance_cells == 0
    }
}

enum Rendered {
    Lines(Vec<Line>),
    Fallback(DrawOutcome),
}

/// Draws `text` with `font` into a deterministic one-bit grayscale PNG.
///
/// C0 controls other than LF and TAB, `U+007F`, and C1 controls are
/// dropped before layout. LF flushes the current line and a trailing LF
/// does not add an empty row. TAB advances to the next multiple of
/// [`TAB_STOP`] cells. A width-0 character overlays the previous cell
/// without advancing, or columns 0 through 7 when it leads a line.
/// Width-1 and width-2 characters occupy [`SINGLE_W`] and [`DOUBLE_W`]
/// pixels; a double-width character moves to a new line rather than
/// straddling cell [`WRAP_CELLS`]. Lines wrap at [`WRAP_CELLS`] cells and
/// the image carries an [`MARGIN`]-pixel white margin around its content.
///
/// A code point above `U+FFFF`, or one the font table does not cover,
/// returns [`DrawOutcome::Fallback`] for the whole description before any
/// pixel is painted. An all-empty description does the same.
///
/// # Errors
/// Returns [`DrawError::Glyph`] when the font table fails to parse and
/// [`DrawError::Png`] when PNG encoding fails; neither yields an image.
#[must_use = "inspect the outcome before using the optional image"]
pub fn draw(font: &Font, text: &str) -> Result<(DrawOutcome, Option<Image>), DrawError> {
    let table = font.glyphs()?;
    match render(table, text) {
        Rendered::Fallback(outcome) => Ok((outcome, None)),
        Rendered::Lines(lines) => {
            let image = encode(&lines)?;
            Ok((DrawOutcome::Drawn, Some(image)))
        }
    }
}

fn render(table: &Glyphs, text: &str) -> Rendered {
    let wrap_cells = usize::try_from(WRAP_CELLS).unwrap_or(usize::MAX);
    let tab_stop = usize::try_from(TAB_STOP).unwrap_or(usize::MAX);
    let single_w = usize::try_from(SINGLE_W).unwrap_or(0);
    let mut lines: Vec<Line> = Vec::new();
    let mut current = Line::default();
    let mut previous_cell_px: Option<usize> = None;
    for ch in text.chars() {
        if is_dropped_control(ch) {
            continue;
        }
        if ch == '\n' {
            flush_line(&mut lines, &mut current, &mut previous_cell_px);
            continue;
        }
        if ch == '\t' {
            let target_cells = (current.advance_cells / tab_stop + 1) * tab_stop;
            if target_cells > wrap_cells {
                flush_line(&mut lines, &mut current, &mut previous_cell_px);
            } else {
                current.advance_cells = target_cells;
            }
            continue;
        }
        let codepoint = u32::from(ch);
        if codepoint > 0xFFFF {
            return Rendered::Fallback(DrawOutcome::Fallback {
                first_undrawable: Some(codepoint),
            });
        }
        let Some(glyph) = table.find(codepoint) else {
            return Rendered::Fallback(DrawOutcome::Fallback {
                first_undrawable: Some(codepoint),
            });
        };
        let needed_cells = match UnicodeWidthChar::width(ch) {
            Some(0) => {
                let overlay_px = previous_cell_px.unwrap_or(0);
                current.cells.push((overlay_px, glyph));
                continue;
            }
            Some(width_cells) => width_cells,
            None => continue,
        };
        if current.advance_cells + needed_cells > wrap_cells {
            flush_line(&mut lines, &mut current, &mut previous_cell_px);
        }
        place_glyph(
            &mut current,
            &mut previous_cell_px,
            glyph,
            needed_cells,
            single_w,
        );
    }
    flush_line(&mut lines, &mut current, &mut previous_cell_px);
    while lines.last().is_some_and(Line::is_blank) {
        lines.pop();
    }
    if lines.iter().all(Line::is_blank) {
        return Rendered::Fallback(DrawOutcome::Fallback {
            first_undrawable: None,
        });
    }
    Rendered::Lines(lines)
}

fn place_glyph(
    current: &mut Line,
    previous_cell_px: &mut Option<usize>,
    glyph: Glyph,
    width_cells: usize,
    single_w: usize,
) {
    let cell_px = current.advance_cells * single_w;
    current.cells.push((cell_px, glyph));
    current.advance_cells += width_cells;
    *previous_cell_px = Some(cell_px);
}

fn flush_line(lines: &mut Vec<Line>, current: &mut Line, previous_cell_px: &mut Option<usize>) {
    lines.push(std::mem::take(current));
    *previous_cell_px = None;
}

fn is_dropped_control(ch: char) -> bool {
    let codepoint = u32::from(ch);
    if codepoint < 0x20 {
        return codepoint != 0x09 && codepoint != 0x0A;
    }
    codepoint == 0x7F || (0x80..=0x9F).contains(&codepoint)
}

fn encode(lines: &[Line]) -> Result<Image, DrawError> {
    let margin_px = usize::try_from(MARGIN).unwrap_or(0);
    let line_h = usize::try_from(LINE_H).unwrap_or(0);
    let single_w = usize::try_from(SINGLE_W).unwrap_or(0);
    let max_advance_cells = lines
        .iter()
        .map(|line| line.advance_cells)
        .max()
        .unwrap_or(0);
    let max_advance_px =
        u32::try_from(max_advance_cells.saturating_mul(single_w)).unwrap_or(u32::MAX);
    let line_count = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let width = 2 * MARGIN + max_advance_px;
    let height = 2 * MARGIN + LINE_H.saturating_mul(line_count);
    let row_bytes = usize::try_from(width / 8).unwrap_or(0);
    let height_px = usize::try_from(height).unwrap_or(0);
    // One byte per eight pixels; every byte starts white so the row-end
    // padding bits stay 1 even when a row does not fill its last byte.
    let mut pixels = vec![0xFF_u8; row_bytes.saturating_mul(height_px)];
    for (line_index, line) in lines.iter().enumerate() {
        let top = margin_px + line_h * line_index;
        for &(cell_px, glyph) in &line.cells {
            paint_glyph(&mut pixels, row_bytes, margin_px + cell_px, top, glyph);
        }
    }
    let mut png_bytes = Vec::new();
    {
        let mut encoder = Encoder::new(&mut png_bytes, width, height);
        encoder.set_color(ColorType::Grayscale);
        encoder.set_depth(BitDepth::One);
        encoder.set_deflate_compression(DeflateCompression::FdeflateUltraFast);
        // Last, so the fixed no-filter scanlines survive any compression
        // preset: every scanline filter byte must be 0.
        encoder.set_filter(Filter::NoFilter);
        let encode_failure = |error: png::EncodingError| DrawError::Png(error.to_string());
        let mut writer = encoder.write_header().map_err(encode_failure)?;
        writer.write_image_data(&pixels).map_err(encode_failure)?;
        writer.finish().map_err(encode_failure)?;
    }
    Ok(Image {
        png: png_bytes.into(),
        width,
        height,
    })
}

fn paint_glyph(pixels: &mut [u8], row_bytes: usize, left: usize, top: usize, glyph: Glyph) {
    for (row_index, &bits) in glyph.rows.iter().enumerate() {
        if bits == 0 {
            continue;
        }
        let pixel_y = top + row_index;
        for col in glyph_ink_columns(bits, glyph.cols) {
            clear_pixel(pixels, row_bytes, left + col, pixel_y);
        }
    }
}

fn glyph_ink_columns(bits: u16, cols: u8) -> impl Iterator<Item = usize> {
    (0..usize::from(cols)).filter(move |&col| ((bits >> (15 - col)) & 1) == 1)
}

fn clear_pixel(pixels: &mut [u8], row_bytes: usize, pixel_x: usize, pixel_y: usize) {
    // The geometry above guarantees the index fits the buffer.
    if let Some(byte) = pixels.get_mut(pixel_y.saturating_mul(row_bytes) + pixel_x / 8) {
        *byte &= !(0x80 >> (pixel_x % 8));
    }
}

#[cfg(test)]
pub(crate) mod oracle {
    //! Test-only independent oracle for glyph layout and PNG decoding.
    //!
    //! Re-derives expected pixels from the hex text without touching the
    //! renderer's layout code, and decodes rendered PNG bytes back to
    //! packed one-bit pixels through the `png` decoder.

    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::sync::LazyLock;

    use unicode_width::UnicodeWidthChar;

    use crate::letter::glyphs::Font;

    /// Raw bytes of the bundled Unifont hex asset.
    pub(crate) const FONT_HEX: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/unifont-18.0.01.hex"
    ));

    const MARGIN_PX: usize = 8;
    const LINE_PX: usize = 16;
    const WRAP_PX: usize = 96 * 8;
    const TAB_CELLS: usize = 4;

    /// Independent glyph table: code point to bitmap columns and rows.
    pub(crate) type Table = BTreeMap<u32, (u8, [u16; 16])>;

    type Placed = (usize, u8, [u16; 16]);

    /// Returns the embedded font as one lazily parsed shared value.
    pub(crate) fn embedded_font() -> &'static Font {
        static FONT: LazyLock<Font> = LazyLock::new(Font::embedded);
        &FONT
    }

    /// Parses the hex font with an independent, lenient reader.
    pub(crate) fn parse_font(bytes: &[u8]) -> Option<Table> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut table = Table::new();
        for line in text.lines() {
            let (codepoint_text, bitmap_text) = line.split_once(':')?;
            let codepoint = u32::from_str_radix(codepoint_text, 16).ok()?;
            if bitmap_text.len() != 32 && bitmap_text.len() != 64 {
                return None;
            }
            let mut bitmap = [0_u16; 16];
            let digits_per_row = bitmap_text.len() / 16;
            for (slot, row_digits) in bitmap
                .iter_mut()
                .zip(bitmap_text.as_bytes().chunks(digits_per_row))
            {
                let mut value = 0_u16;
                for &digit in row_digits {
                    let nibble = nibble_value(digit)?;
                    value = value.checked_mul(16)?.checked_add(nibble)?;
                }
                *slot = if bitmap_text.len() == 32 {
                    value << 8
                } else {
                    value
                };
            }
            let cols = if bitmap_text.len() == 32 { 8 } else { 16 };
            table.insert(codepoint, (cols, bitmap));
        }
        Some(table)
    }

    fn nibble_value(digit: u8) -> Option<u16> {
        match digit {
            b'0'..=b'9' => Some(u16::from(digit - b'0')),
            b'a'..=b'f' => Some(u16::from(digit - b'a') + 10),
            b'A'..=b'F' => Some(u16::from(digit - b'A') + 10),
            _ => None,
        }
    }

    /// Renders expected image geometry and packed pixels independently.
    /// Returns `None` when the description falls back as a whole.
    pub(crate) fn expected_image(table: &Table, text: &str) -> Option<(u32, u32, Vec<u8>)> {
        let mut state = ExpectState::new();
        for ch in text.chars() {
            let codepoint = u32::from(ch);
            let dropped = (codepoint < 0x20 && codepoint != 0x0A && codepoint != 0x09)
                || codepoint == 0x7F
                || (0x80..=0x9F).contains(&codepoint);
            if dropped {
                continue;
            }
            if codepoint == 0x0A {
                state.newline();
                continue;
            }
            if codepoint == 0x09 {
                state.tab();
                continue;
            }
            if codepoint > 0xFFFF {
                return None;
            }
            let &(cols, bitmap) = table.get(&codepoint)?;
            match UnicodeWidthChar::width(ch) {
                Some(0) => state.overlay(cols, bitmap),
                Some(width_cells) => state.place(cols, bitmap, width_cells),
                None => {}
            }
        }
        state.finish()
    }

    struct ExpectState {
        rows: Vec<(Vec<Placed>, usize)>,
        cursor_px: usize,
        last_cell_px: Option<usize>,
    }

    impl ExpectState {
        fn new() -> Self {
            Self {
                rows: vec![(Vec::new(), 0)],
                cursor_px: 0,
                last_cell_px: None,
            }
        }

        fn newline(&mut self) {
            self.rows.push((Vec::new(), 0));
            self.cursor_px = 0;
            self.last_cell_px = None;
        }

        fn tab(&mut self) {
            let target_cells = self.cursor_px / 8 / TAB_CELLS + 1;
            let target_px = target_cells * TAB_CELLS * 8;
            if target_px > WRAP_PX {
                self.newline();
            } else {
                self.cursor_px = target_px;
                if let Some(row) = self.rows.last_mut() {
                    row.1 = target_px;
                }
            }
        }

        fn overlay(&mut self, cols: u8, bitmap: [u16; 16]) {
            let cell_px = self.last_cell_px.unwrap_or(0);
            if let Some(row) = self.rows.last_mut() {
                row.0.push((cell_px, cols, bitmap));
            }
        }

        fn place(&mut self, cols: u8, bitmap: [u16; 16], width_cells: usize) {
            let needed_px = width_cells * 8;
            if self.cursor_px + needed_px > WRAP_PX {
                self.newline();
            }
            if let Some(row) = self.rows.last_mut() {
                row.0.push((self.cursor_px, cols, bitmap));
            }
            self.last_cell_px = Some(self.cursor_px);
            self.cursor_px += needed_px;
            if let Some(row) = self.rows.last_mut() {
                row.1 = self.cursor_px;
            }
        }

        fn finish(mut self) -> Option<(u32, u32, Vec<u8>)> {
            while self
                .rows
                .last()
                .is_some_and(|(cells, advance)| cells.is_empty() && *advance == 0)
            {
                self.rows.pop();
            }
            if self
                .rows
                .iter()
                .all(|(cells, advance)| cells.is_empty() && *advance == 0)
            {
                return None;
            }
            let max_advance = self
                .rows
                .iter()
                .map(|&(_, advance)| advance)
                .max()
                .unwrap_or(0);
            let width_px = 2 * MARGIN_PX + max_advance;
            let height_px = 2 * MARGIN_PX + LINE_PX * self.rows.len();
            let mut white = vec![true; width_px * height_px];
            for (row_index, (cells, _)) in self.rows.iter().enumerate() {
                for &(cell_px, cols, bitmap) in cells {
                    for (row, &bits) in bitmap.iter().enumerate() {
                        let y = MARGIN_PX + LINE_PX * row_index + row;
                        for col in 0..usize::from(cols) {
                            if ((bits >> (15 - col)) & 1) == 1 {
                                let index = y * width_px + MARGIN_PX + cell_px + col;
                                if let Some(pixel) = white.get_mut(index) {
                                    *pixel = false;
                                }
                            }
                        }
                    }
                }
            }
            let row_bytes = width_px.div_ceil(8);
            let mut packed = vec![0xFF_u8; row_bytes * height_px];
            for (index, &is_white) in white.iter().enumerate() {
                if is_white {
                    continue;
                }
                let y = index / width_px;
                let x = index % width_px;
                if let Some(byte) = packed.get_mut(y * row_bytes + x / 8) {
                    *byte &= !(0x80 >> (x % 8));
                }
            }
            let width = u32::try_from(width_px).unwrap();
            let height = u32::try_from(height_px).unwrap();
            Some((width, height, packed))
        }
    }

    /// Decodes a rendered PNG back to packed one-bit grayscale pixels.
    pub(crate) fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(Cursor::new(bytes.to_vec()));
        let mut reader = decoder.read_info().unwrap();
        let size = reader.output_buffer_size().unwrap();
        let mut pixels = vec![0_u8; size];
        let info = reader.next_frame(&mut pixels).unwrap();
        (info.width, info.height, pixels)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::oracle::{decode_png, embedded_font};
    use super::{DrawOutcome, draw};

    fn is_white(bits: &[u8], width: u32, x: u32, y: u32) -> bool {
        let row_bytes = usize::try_from(width.div_ceil(8)).unwrap();
        let index = usize::try_from(y).unwrap() * row_bytes + usize::try_from(x).unwrap() / 8;
        let byte = bits.get(index).copied().unwrap_or(0xFF);
        byte & (0x80 >> (x % 8)) != 0
    }

    #[test]
    fn render_determinism() {
        let font = embedded_font();
        let text = "한글 éß\nHello world\n제2행 line three";
        let first = draw(font, text).unwrap();
        let second = draw(font, text).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn wide_glyph_metrics() {
        let font = embedded_font();
        let (outcome, image) = draw(font, "한A ").unwrap();
        assert_eq!(outcome, DrawOutcome::Drawn);
        let image = image.unwrap();
        assert_eq!(image.width, 16 + 16 + 8 + 8);
        assert_eq!(image.height, 16 + 16);
    }

    #[test]
    fn undrawable_falls_back_whole() {
        let font = embedded_font();
        let (outcome, image) = draw(font, "abc \u{1F600} def").unwrap();
        assert_eq!(
            outcome,
            DrawOutcome::Fallback {
                first_undrawable: Some(0x1F600)
            }
        );
        assert!(image.is_none());
    }

    #[test]
    fn line_wrapping() {
        let font = embedded_font();
        let (outcome, image) = draw(font, &"x".repeat(200)).unwrap();
        assert_eq!(outcome, DrawOutcome::Drawn);
        let image = image.unwrap();
        assert_eq!(image.width, 2 * 8 + 96 * 8);
        assert_eq!(image.height, 16 + 3 * 16);
    }

    #[test]
    fn worst_case_bounds() {
        let font = embedded_font();
        let (outcome, image) = draw(font, &"一".repeat(4096)).unwrap();
        assert_eq!(outcome, DrawOutcome::Drawn);
        let image = image.unwrap();
        assert_eq!(image.width, 2 * 8 + 96 * 8);
        assert_eq!(image.height, 2 * 8 + 86 * 16);
        assert!(!image.png.is_empty());
    }

    #[test]
    fn png_byte_shape() {
        let font = embedded_font();
        let (_, image) = draw(font, "Hi\n").unwrap();
        let image = image.unwrap();
        let bytes = &image.png;
        assert_eq!(
            bytes.get(..8),
            Some(&[137_u8, 80, 78, 71, 13, 10, 26, 10][..])
        );
        let mut chunk_types: Vec<[u8; 4]> = Vec::new();
        let mut position = 8;
        while position < bytes.len() {
            let length_bytes = bytes.get(position..position + 4).unwrap();
            let length = u32::from_be_bytes(length_bytes.try_into().unwrap());
            let length = usize::try_from(length).unwrap();
            let type_bytes = bytes.get(position + 4..position + 8).unwrap();
            chunk_types.push(type_bytes.try_into().unwrap());
            if type_bytes == b"IHDR" {
                let data = bytes.get(position + 8..position + 8 + length).unwrap();
                assert_eq!(length, 13);
                assert_eq!(data.get(8), Some(&1_u8));
                assert_eq!(data.get(9), Some(&0_u8));
                assert_eq!(data.get(10), Some(&0_u8));
                assert_eq!(data.get(11), Some(&0_u8));
                assert_eq!(data.get(12), Some(&0_u8));
            }
            position += 12 + length;
        }
        assert_eq!(chunk_types, vec![*b"IHDR", *b"IDAT", *b"IEND"]);
        let tail = bytes.len();
        assert_eq!(bytes.get(tail - 12..tail - 8), Some(&[0_u8, 0, 0, 0][..]));
    }

    #[test]
    fn character_class_handling() {
        let font = embedded_font();
        let text = "\u{0300}A\u{0301}\u{0007}\u{0085}\tB\n";
        let (outcome, image) = draw(font, text).unwrap();
        assert_eq!(outcome, DrawOutcome::Drawn);
        let image = image.unwrap();
        assert_eq!(image.width, 2 * 8 + 5 * 8);
        assert_eq!(image.height, 16 + 16);
        let (width, height, bits) = decode_png(&image.png);
        assert_eq!((width, height), (image.width, image.height));
        // The leading mark overlays columns 0-7 and 'A' follows in the
        // same cell: nothing advances, so ink starts at cell 0.
        assert!((8..16).any(|x| (8..24).any(|y| !is_white(&bits, width, x, y))));
        // Cells 1 through 3 stay white: the dropped controls vanish and
        // the marks advance nothing before TAB lands on cell 4.
        assert!((16..40).all(|x| (8..24).all(|y| is_white(&bits, width, x, y))));
        // 'B' sits at cell 4, a multiple of TAB_STOP.
        assert!((40..48).any(|x| (8..24).any(|y| !is_white(&bits, width, x, y))));
        // The trailing LF adds no empty row and the margins stay white.
        assert!((48..56).all(|x| (0..32).all(|y| is_white(&bits, width, x, y))));
        assert!((0..48).all(|x| (24..32).all(|y| is_white(&bits, width, x, y))));
    }

    proptest! {
        #[test]
        fn no_image_from_bad_input(chars in prop::collection::vec(any::<char>(), 0..=48)) {
            let text: String = chars.iter().collect();
            let font = embedded_font();
            let glyphs = font.glyphs().unwrap();
            let mut expected_fallback: Option<u32> = None;
            let mut has_content = false;
            for &ch in &chars {
                let codepoint = u32::from(ch);
                let dropped = (codepoint < 0x20 && codepoint != 0x0A && codepoint != 0x09)
                    || codepoint == 0x7F
                    || (0x80..=0x9F).contains(&codepoint);
                if dropped {
                    continue;
                }
                if ch == '\n' {
                    continue;
                }
                has_content = true;
                if ch == '\t' {
                    continue;
                }
                if codepoint > 0xFFFF || glyphs.find(codepoint).is_none() {
                    expected_fallback = Some(codepoint);
                    break;
                }
            }
            let (outcome, image) = draw(font, &text).unwrap();
            if let Some(codepoint) = expected_fallback {
                assert_eq!(
                    outcome,
                    DrawOutcome::Fallback {
                        first_undrawable: Some(codepoint)
                    }
                );
                assert!(image.is_none());
            } else if !has_content {
                assert_eq!(outcome, DrawOutcome::Fallback { first_undrawable: None });
                assert!(image.is_none());
            } else {
                assert_eq!(outcome, DrawOutcome::Drawn);
                assert!(image.is_some());
            }
        }
    }
}
