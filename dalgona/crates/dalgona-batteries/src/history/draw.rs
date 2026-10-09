// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Pure pagination and deterministic 1-bit PNG drawing for history images.

pub(crate) mod cells;
pub(crate) mod raster;

use dal_ext::Glyphs;
use png::{BitDepth, ColorType, DeflateCompression, Encoder, Filter};
use thiserror::Error;

use super::spans::{Item, Role, Span};
use cells::{fits, place, tokenize};
use raster::draw_text;

/// Grid geometry from the provider catalog profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Grid {
    /// Cell columns per image.
    pub(crate) cols: u16,
    /// Cell rows per image.
    pub(crate) rows: u16,
    /// Pixel width of one cell.
    pub(crate) cell_w: u8,
    /// Pixel height of one cell.
    pub(crate) cell_h: u8,
}

/// One paginated history image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Page {
    /// Items drawn on this page with page-local span slices.
    pub(crate) items: Vec<Item>,
    /// Rows occupied by placed cells.
    pub(crate) rows_used: u16,
    /// False when any cell holds an undrawable glyph.
    pub(crate) drawable: bool,
}

/// A pagination or encoding failure.
#[derive(Debug, Error)]
pub(crate) enum DrawError {
    /// A grid dimension is zero.
    #[error("history image grid has a zero dimension")]
    EmptyGrid,
    /// An indivisible mark exceeds the empty grid.
    #[error("history image mark does not fit the configured grid")]
    MarkTooLarge,
    /// A text cell exceeds the grid columns.
    #[error("history image text cell does not fit the configured grid")]
    CellTooLarge,
    /// The page holds an undrawable glyph.
    #[error("history image page contains an undrawable glyph")]
    UndrawablePage,
    /// One code point has no glyph.
    #[error("history image contains an undrawable code point U+{codepoint:04X}")]
    Undrawable {
        /// The first undrawable code point.
        codepoint: u32,
    },
    /// The PNG encoder rejected the image.
    #[error("history PNG encode failed: {0}")]
    Png(String),
    /// Pixel dimensions overflow the encoder.
    #[error("history PNG dimensions exceed the encoder limit")]
    Dimensions,
}

/// A cell cursor for pagination and drawing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Cursor {
    /// Current cell column.
    pub(crate) col: u16,
    /// Current cell row.
    pub(crate) row: u16,
}

/// Placement of one cell run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Placed {
    /// Origin cell of the run.
    pub(crate) origin: Cursor,
    /// Cursor after the run.
    pub(crate) next: Cursor,
}

struct TextInput<'a> {
    item: &'a Item,
    span: Span,
    role: &'a Role,
    total: u32,
    text: &'a str,
}

struct PageBuilder {
    items: Vec<Item>,
    cursor: Cursor,
    rows_used: u16,
    drawable: bool,
}

impl PageBuilder {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            cursor: Cursor::default(),
            rows_used: 0,
            drawable: true,
        }
    }

    fn finish(self) -> Page {
        Page {
            items: self.items,
            rows_used: self.rows_used,
            drawable: self.drawable,
        }
    }
}

/// Paginates items into grid-sized drawable pages.
pub(crate) fn paginate(glyphs: &Glyphs, grid: Grid, items: &[Item]) -> Vec<Page> {
    if items.is_empty() {
        return Vec::new();
    }
    if grid.cols == 0 || grid.rows == 0 || grid.cell_w == 0 || grid.cell_h == 0 {
        return vec![Page {
            items: items.to_vec(),
            rows_used: u16::MAX,
            drawable: false,
        }];
    }

    let mut pages = Vec::new();
    let mut page = PageBuilder::new();
    for item in items {
        match item {
            Item::Mark(mark) => {
                add_indivisible(glyphs, grid, &mut pages, &mut page, item, mark);
            }
            Item::Picture { mime, bytes, .. } => {
                let mark = picture_mark(mime, *bytes);
                add_indivisible(glyphs, grid, &mut pages, &mut page, item, &mark);
            }
            Item::Text {
                span,
                role,
                total,
                text,
            } => add_text(
                glyphs,
                grid,
                &mut pages,
                &mut page,
                &TextInput {
                    item,
                    span: *span,
                    role,
                    total: *total,
                    text,
                },
            ),
        }
    }
    if !page.items.is_empty() || pages.is_empty() {
        pages.push(page.finish());
    }
    pages
}

/// Draws one drawable page as a deterministic 1-bit grayscale PNG.
pub(crate) fn draw(glyphs: &Glyphs, grid: Grid, page: &Page) -> Result<Vec<u8>, DrawError> {
    validate_grid(grid)?;
    if !page.drawable {
        return Err(DrawError::UndrawablePage);
    }
    if page.rows_used > grid.rows {
        return Err(DrawError::MarkTooLarge);
    }
    let width = u32::from(grid.cols)
        .checked_mul(u32::from(grid.cell_w))
        .ok_or(DrawError::Dimensions)?;
    let height = u32::from(grid.rows)
        .checked_mul(u32::from(grid.cell_h))
        .ok_or(DrawError::Dimensions)?;
    if width == 0 || height == 0 {
        return Err(DrawError::EmptyGrid);
    }
    let row_bytes = usize::try_from(width.div_ceil(8)).map_err(|_| DrawError::Dimensions)?;
    let byte_len = row_bytes
        .checked_mul(usize::try_from(height).map_err(|_| DrawError::Dimensions)?)
        .ok_or(DrawError::Dimensions)?;
    let mut pixels = vec![u8::MAX; byte_len];

    let mut cursor = Cursor::default();
    for item in &page.items {
        match item {
            Item::Mark(mark) => draw_text(glyphs, grid, mark, &mut cursor, &mut pixels, row_bytes)?,
            Item::Text { text, .. } => {
                draw_text(glyphs, grid, text, &mut cursor, &mut pixels, row_bytes)?;
            }
            Item::Picture { mime, bytes, .. } => {
                let mark = picture_mark(mime, *bytes);
                draw_text(glyphs, grid, &mark, &mut cursor, &mut pixels, row_bytes)?;
            }
        }
    }

    let mut png_bytes = Vec::new();
    let mut encoder = Encoder::new(&mut png_bytes, width, height);
    encoder.set_color(ColorType::Grayscale);
    encoder.set_depth(BitDepth::One);
    encoder.set_deflate_compression(DeflateCompression::FdeflateUltraFast);
    encoder.set_filter(Filter::NoFilter);
    let encode_failure = |error: png::EncodingError| DrawError::Png(error.to_string());
    let mut writer = encoder.write_header().map_err(encode_failure)?;
    writer.write_image_data(&pixels).map_err(encode_failure)?;
    writer.finish().map_err(encode_failure)?;
    Ok(png_bytes)
}

fn validate_grid(grid: Grid) -> Result<(), DrawError> {
    if grid.cols == 0 || grid.rows == 0 || grid.cell_w == 0 || grid.cell_h == 0 {
        return Err(DrawError::EmptyGrid);
    }
    Ok(())
}

fn add_indivisible(
    glyphs: &Glyphs,
    grid: Grid,
    pages: &mut Vec<Page>,
    page: &mut PageBuilder,
    item: &Item,
    text: &str,
) {
    let (cells, first_undrawable) = tokenize(glyphs, text);
    let fit = fits(grid, page.cursor, &cells);
    if fit.is_none() && !page.items.is_empty() {
        pages.push(std::mem::replace(page, PageBuilder::new()).finish());
    }
    let Some((cursor, rows)) = fits(grid, page.cursor, &cells) else {
        page.items.push(item.clone());
        page.rows_used = grid.rows.saturating_add(1);
        page.drawable = first_undrawable.is_none();
        return;
    };
    page.cursor = cursor;
    page.rows_used = page.rows_used.max(rows);
    page.drawable &= first_undrawable.is_none();
    page.items.push(item.clone());
}

fn add_text(
    glyphs: &Glyphs,
    grid: Grid,
    pages: &mut Vec<Page>,
    page: &mut PageBuilder,
    input: &TextInput<'_>,
) {
    let &TextInput {
        item,
        span,
        role,
        total,
        text,
    } = input;
    let (cells, first_undrawable) = tokenize(glyphs, text);
    let Some(first) = cells.first() else {
        return;
    };
    let mut fit = fits(grid, page.cursor, &cells);
    if fit.is_none()
        && let Some(empty_fit) = fits(grid, Cursor::default(), &cells)
    {
        if !page.items.is_empty() {
            pages.push(std::mem::replace(page, PageBuilder::new()).finish());
        }
        fit = Some(empty_fit);
    }
    if let Some((cursor, rows)) = fit {
        page.cursor = cursor;
        page.rows_used = page.rows_used.max(rows);
        page.drawable &= first_undrawable.is_none();
        page.items.push(item.clone());
        return;
    }
    let mut segment_start = first.bytes.start;
    for cell in &cells {
        let placed = place(grid, page.cursor, cell.width);
        let needs_page = placed.is_none_or(|value| value.origin.row >= grid.rows);
        if needs_page && !page.items.is_empty() {
            append_text_segment(
                page,
                span,
                role,
                total,
                text,
                segment_start,
                cell.bytes.start,
            );
            pages.push(std::mem::replace(page, PageBuilder::new()).finish());
            add_continuation_mark(glyphs, grid, page, role);
            segment_start = cell.bytes.start;
        }
        let Some(placed) = place(grid, page.cursor, cell.width) else {
            overflow_text(page, grid, span, role, total, text, segment_start);
            return;
        };
        if placed.origin.row >= grid.rows {
            overflow_text(page, grid, span, role, total, text, segment_start);
            return;
        }
        page.cursor = placed.next;
        page.rows_used = page.rows_used.max(placed.origin.row.saturating_add(1));
        if cell.undrawable.is_some() {
            page.drawable = false;
        }
    }
    append_text_segment(page, span, role, total, text, segment_start, text.len());
}

fn add_continuation_mark(glyphs: &Glyphs, grid: Grid, page: &mut PageBuilder, role: &Role) {
    let Some(mark) = role.mark() else {
        return;
    };
    let (cells, first_undrawable) = tokenize(glyphs, &mark);
    let Some((cursor, rows)) = fits(grid, page.cursor, &cells) else {
        page.rows_used = grid.rows.saturating_add(1);
        page.drawable = false;
        return;
    };
    page.cursor = cursor;
    page.rows_used = page.rows_used.max(rows);
    page.drawable &= first_undrawable.is_none();
    page.items.push(Item::Mark(mark));
}

fn overflow_text(
    page: &mut PageBuilder,
    grid: Grid,
    span: Span,
    role: &Role,
    total: u32,
    text: &str,
    start: usize,
) {
    append_text_segment(page, span, role, total, text, start, text.len());
    page.rows_used = grid.rows.saturating_add(1);
    page.drawable = false;
}

fn append_text_segment(
    page: &mut PageBuilder,
    original: Span,
    role: &Role,
    total: u32,
    text: &str,
    start: usize,
    end: usize,
) {
    if start >= end {
        return;
    }
    let Ok(offset) = u32::try_from(start) else {
        page.drawable = false;
        return;
    };
    let Ok(length) = u32::try_from(end - start) else {
        page.drawable = false;
        return;
    };
    let Some(off) = original.off.checked_add(offset) else {
        page.drawable = false;
        return;
    };
    page.items.push(Item::Text {
        span: Span {
            off,
            len: length,
            ..original
        },
        role: role.clone(),
        total,
        text: text[start..end].into(),
    });
}

fn picture_mark(mime: &str, bytes: u32) -> String {
    format!("¶image:{mime} {bytes} bytes")
}
