//! Deterministic rendering of text with the bundled bitmap font.

mod budget;
mod glyphs;
mod layout;

pub use budget::{
    BYTE_BUDGET, FallbackReason, IMAGE_BUDGET, LetterAssembly, LetterChunk, LetterFallback,
    image_parts, letters,
};
pub use glyphs::{Font, Glyph, GlyphError, Glyphs};
pub use layout::{
    DOUBLE_W, DrawError, DrawOutcome, GLYPH_H, Image, LINE_H, MARGIN, MAX_DESC_CHARS, SINGLE_W,
    TAB_STOP, WRAP_CELLS, draw,
};
