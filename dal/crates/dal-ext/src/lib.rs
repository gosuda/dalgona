//! Built-in extension composition and rendering for dal.

mod letter;
mod prompt;

pub use letter::{
    DOUBLE_W, DrawError, DrawOutcome, Font, GLYPH_H, Glyph, GlyphError, Glyphs, Image, LINE_H,
    MARGIN, MAX_DESC_CHARS, SINGLE_W, TAB_STOP, WRAP_CELLS, draw,
};
pub use prompt::instructions::{
    FileReader, InstructionFile, InstructionReadError, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
    TRUNCATION_MARKER, collect as collect_instructions, load_system_md,
    render as render_instructions,
};
pub use prompt::{
    D2_PREFERENCE_LINE, DEFAULT_DOCS_LINE, PromptInput, build as build_prompt, prefix_bytes,
};
