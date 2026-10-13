// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! History cell tokenization and grid placement.

use std::ops::Range;

use dal_ext::{Glyphs, is_zero_width};

use super::{Cursor, Grid, Placed};

/// One laid-out cell run with its source byte range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Cell {
    /// Source bytes covered by this cell run.
    pub(crate) bytes: Range<usize>,
    /// Cell columns occupied: 0, 1, or 2.
    pub(crate) width: u16,
    /// How the run draws.
    pub(crate) kind: CellKind,
    /// First undrawable code point in the run, if any.
    pub(crate) undrawable: Option<u32>,
}

/// How one cell run paints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CellKind {
    /// A base glyph, if present.
    Glyph(Option<dal_ext::Glyph>),
    /// A blank space/tab run.
    Blank,
    /// A newline run drawn as U+2588, if present.
    Newline(Option<dal_ext::Glyph>),
    /// Only zero-cell content.
    Zero,
}

/// Splits text into indivisible cell runs with undrawable tracking.
pub(crate) fn tokenize(glyphs: &Glyphs, text: &str) -> (Vec<Cell>, Option<u32>) {
    let bytes = text.as_bytes();
    let mut cells = Vec::new();
    let mut cursor = 0;
    let mut leading_zero: Option<Range<usize>> = None;
    let mut leading_missing = None;
    let mut first_undrawable = None;
    while cursor < bytes.len() {
        let Some((ch, char_len)) = char_at(text, cursor) else {
            break;
        };
        let end = cursor + char_len;
        if ch == '\u{1b}' {
            let escape_end = csi_end(bytes, cursor);
            append_zero(
                &mut cells,
                &mut leading_zero,
                &mut leading_missing,
                cursor,
                escape_end,
                None,
            );
            cursor = escape_end;
            continue;
        }
        if is_newline(ch) {
            let run_end = run_end(text, end, is_newline);
            let block = glyphs.find(0x2588);
            let missing = block.is_none().then_some(0x2588);
            if first_undrawable.is_none() {
                first_undrawable = missing;
            }
            cells.push(Cell {
                bytes: leading_zero
                    .take()
                    .map_or(cursor..run_end, |zero| zero.start..run_end),
                width: 1,
                kind: CellKind::Newline(block),
                undrawable: leading_missing.take().or(missing),
            });
            cursor = run_end;
            continue;
        }
        if matches!(ch, ' ' | '\t') {
            let run_end = run_end(text, end, |next| matches!(next, ' ' | '\t'));
            cells.push(Cell {
                bytes: leading_zero
                    .take()
                    .map_or(cursor..run_end, |zero| zero.start..run_end),
                width: 1,
                kind: CellKind::Blank,
                undrawable: leading_missing.take(),
            });
            cursor = run_end;
            continue;
        }
        if ch.is_control() {
            append_zero(
                &mut cells,
                &mut leading_zero,
                &mut leading_missing,
                cursor,
                end,
                None,
            );
            cursor = end;
            continue;
        }
        if is_zero_width(ch) {
            let mark = glyphs.find(u32::from(ch));
            let missing = mark.is_none().then_some(u32::from(ch));
            if first_undrawable.is_none() {
                first_undrawable = missing;
            }
            append_zero(
                &mut cells,
                &mut leading_zero,
                &mut leading_missing,
                cursor,
                end,
                missing,
            );
            cursor = end;
            continue;
        }
        append_glyph(
            glyphs,
            ch,
            cursor..end,
            &mut cells,
            &mut leading_zero,
            &mut leading_missing,
            &mut first_undrawable,
        );
        cursor = end;
    }
    flush_leading_zero(&mut cells, leading_zero, leading_missing);
    (cells, first_undrawable)
}

fn append_glyph(
    glyphs: &Glyphs,
    ch: char,
    bytes: Range<usize>,
    cells: &mut Vec<Cell>,
    leading_zero: &mut Option<Range<usize>>,
    leading_missing: &mut Option<u32>,
    first_undrawable: &mut Option<u32>,
) {
    let codepoint = u32::from(ch);
    let glyph = if codepoint > 0xFFFF {
        None
    } else {
        glyphs.find(codepoint)
    };
    let missing = glyph.is_none().then_some(codepoint);
    if first_undrawable.is_none() {
        *first_undrawable = missing;
    }
    let width = glyph.map_or(1, |glyph| if glyph.cols > 8 { 2 } else { 1 });
    let start = leading_zero.take().map_or(bytes.start, |zero| zero.start);
    cells.push(Cell {
        bytes: start..bytes.end,
        width,
        kind: CellKind::Glyph(glyph),
        undrawable: leading_missing.take().or(missing),
    });
}

/// Places one cell width at the cursor with wrap and wide-glyph rules.
pub(crate) fn place(grid: Grid, cursor: Cursor, width: u16) -> Option<Placed> {
    if grid.cols == 0 || width > grid.cols {
        return None;
    }
    let mut next = cursor;
    if width == 2 && next.col == grid.cols - 1 {
        next.col = 0;
        next.row = next.row.saturating_add(1);
    }
    if next.col.saturating_add(width) > grid.cols {
        next.col = 0;
        next.row = next.row.saturating_add(1);
    }
    let origin = next;
    if width > 0 {
        next.col += width;
        if next.col == grid.cols {
            next.col = 0;
            next.row = next.row.saturating_add(1);
        }
    }
    Some(Placed { origin, next })
}

/// Fits all cells from the cursor, returning the end cursor and rows used.
pub(crate) fn fits(grid: Grid, cursor: Cursor, cells: &[Cell]) -> Option<(Cursor, u16)> {
    let mut cursor = cursor;
    let mut rows_used = 0;
    for cell in cells {
        let placed = place(grid, cursor, cell.width)?;
        if cell.width > 0 && placed.origin.row >= grid.rows {
            return None;
        }
        if cell.width > 0 {
            rows_used = rows_used.max(placed.origin.row.saturating_add(1));
        }
        cursor = placed.next;
    }
    Some((cursor, rows_used))
}

fn run_end(text: &str, start: usize, mut matches: impl FnMut(char) -> bool) -> usize {
    let mut end = start;
    while let Some((next, length)) = char_at(text, end) {
        if !matches(next) {
            break;
        }
        end += length;
    }
    end
}

fn flush_leading_zero(
    cells: &mut Vec<Cell>,
    leading_zero: Option<Range<usize>>,
    leading_missing: Option<u32>,
) {
    let Some(zero) = leading_zero else {
        return;
    };
    if let Some(last) = cells.last_mut() {
        last.bytes.end = zero.end;
        if last.undrawable.is_none() {
            last.undrawable = leading_missing;
        }
    } else {
        cells.push(Cell {
            bytes: zero,
            width: 0,
            kind: CellKind::Zero,
            undrawable: leading_missing,
        });
    }
}

fn append_zero(
    cells: &mut [Cell],
    leading_zero: &mut Option<Range<usize>>,
    leading_missing: &mut Option<u32>,
    start: usize,
    end: usize,
    missing: Option<u32>,
) {
    let Some(last) = cells.last_mut() else {
        if let Some(pending) = leading_zero {
            pending.end = end;
        } else {
            *leading_zero = Some(start..end);
        }
        if leading_missing.is_none() {
            *leading_missing = missing;
        }
        return;
    };
    last.bytes.end = end;
    if last.undrawable.is_none() {
        last.undrawable = missing;
    }
}

pub(crate) fn char_at(text: &str, offset: usize) -> Option<(char, usize)> {
    let ch = text.get(offset..)?.chars().next()?;
    Some((ch, ch.len_utf8()))
}

pub(crate) fn csi_end(bytes: &[u8], start: usize) -> usize {
    let next = start.saturating_add(1);
    if bytes.get(next) != Some(&b'[') {
        return next;
    }
    let mut cursor = next + 1;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        cursor += 1;
        if (0x40..=0x7E).contains(&byte) {
            return cursor;
        }
    }
    cursor
}

pub(crate) fn is_newline(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}
