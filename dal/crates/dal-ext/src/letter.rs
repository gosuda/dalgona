//! Deterministic rendering of text with the bundled bitmap font.

mod glyphs;
mod layout;

pub use glyphs::{Font, Glyph, GlyphError, Glyphs};
pub use layout::{
    DOUBLE_W, DrawError, DrawOutcome, GLYPH_H, Image, LINE_H, MARGIN, MAX_DESC_CHARS, SINGLE_W,
    TAB_STOP, WRAP_CELLS, draw,
};
