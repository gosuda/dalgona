// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! History 1-bit PNG rasterization.

use dal_ext::{Glyph, Glyphs, is_zero_width};

use super::cells::{Cell, CellKind, char_at, csi_end, is_newline, place, tokenize};
use super::{Cursor, DrawError, Grid};

/// Draws one text run at the cursor.
pub(crate) fn draw_text(
    glyphs: &Glyphs,
    grid: Grid,
    text: &str,
    cursor: &mut Cursor,
    pixels: &mut [u8],
    stride: usize,
) -> Result<(), DrawError> {
    let (cells, missing) = tokenize(glyphs, text);
    if let Some(codepoint) = missing {
        return Err(DrawError::Undrawable { codepoint });
    }
    for cell in cells {
        let placed = place(grid, *cursor, cell.width).ok_or(DrawError::CellTooLarge)?;
        if cell.width > 0 && placed.origin.row >= grid.rows {
            return Err(DrawError::MarkTooLarge);
        }
        draw_cell(glyphs, grid, text, &cell, placed.origin, pixels, stride)?;
        *cursor = placed.next;
    }
    Ok(())
}

fn draw_cell(
    glyphs: &Glyphs,
    grid: Grid,
    text: &str,
    cell: &Cell,
    origin: Cursor,
    pixels: &mut [u8],
    stride: usize,
) -> Result<(), DrawError> {
    match cell.kind {
        CellKind::Glyph(Some(glyph)) | CellKind::Newline(Some(glyph)) => {
            draw_glyph(glyphs, grid, glyph, origin, pixels, stride)?;
        }
        CellKind::Glyph(None) => {
            return Err(DrawError::Undrawable {
                codepoint: cell.undrawable.unwrap_or_default(),
            });
        }
        CellKind::Newline(None) => return Err(DrawError::Undrawable { codepoint: 0x2588 }),
        CellKind::Blank | CellKind::Zero => {}
    }
    draw_zero_overlays(glyphs, grid, text, cell, origin, pixels, stride)
}

fn draw_zero_overlays(
    glyphs: &Glyphs,
    grid: Grid,
    text: &str,
    cell: &Cell,
    origin: Cursor,
    pixels: &mut [u8],
    stride: usize,
) -> Result<(), DrawError> {
    let mut cursor = cell.bytes.start;
    while cursor < cell.bytes.end {
        let Some((ch, length)) = char_at(text, cursor) else {
            break;
        };
        if ch == '\u{1b}' {
            cursor = csi_end(text.as_bytes(), cursor).min(cell.bytes.end);
            continue;
        }
        if ch.is_control() || is_newline(ch) || matches!(ch, ' ' | '\t') {
            cursor += length;
            continue;
        }
        if is_zero_width(ch)
            && let Some(glyph) = glyphs.find(u32::from(ch))
        {
            draw_glyph(glyphs, grid, glyph, origin, pixels, stride)?;
        }
        cursor += length;
    }
    Ok(())
}

fn draw_glyph(
    glyphs: &Glyphs,
    grid: Grid,
    glyph: Glyph,
    origin: Cursor,
    pixels: &mut [u8],
    stride: usize,
) -> Result<(), DrawError> {
    let left = u32::from(origin.col)
        .checked_mul(u32::from(grid.cell_w))
        .ok_or(DrawError::Dimensions)?;
    let top = u32::from(origin.row)
        .checked_mul(u32::from(grid.cell_h))
        .ok_or(DrawError::Dimensions)?;
    let width = u32::from(grid.cols) * u32::from(grid.cell_w);
    let height = u32::from(grid.rows) * u32::from(grid.cell_h);
    for row in 0..16 {
        for col in 0..usize::from(glyph.cols) {
            if !glyphs.pixel(glyph, row, col) {
                continue;
            }
            let x = left
                .checked_add(u32::try_from(col).map_err(|_| DrawError::Dimensions)?)
                .ok_or(DrawError::Dimensions)?;
            let y = top
                .checked_add(u32::try_from(row).map_err(|_| DrawError::Dimensions)?)
                .ok_or(DrawError::Dimensions)?;
            if x >= width || y >= height {
                continue;
            }
            let offset = usize::try_from(y)
                .map_err(|_| DrawError::Dimensions)?
                .checked_mul(stride)
                .and_then(|value| value.checked_add(usize::try_from(x / 8).ok()?))
                .ok_or(DrawError::Dimensions)?;
            let mask = 0x80_u8 >> (x % 8);
            pixels[offset] &= !mask;
        }
    }
    Ok(())
}
