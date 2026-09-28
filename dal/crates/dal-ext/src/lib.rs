//! Built-in extension composition and rendering for dal.

mod letter;
mod prompt;
mod skills;
pub mod ttsr;

pub use letter::{
    BYTE_BUDGET, DOUBLE_W, DrawError, DrawOutcome, FallbackReason, Font, GLYPH_H, Glyph,
    GlyphError, Glyphs, IMAGE_BUDGET, Image, LINE_H, LetterAssembly, LetterChunk, LetterFallback,
    MARGIN, MAX_DESC_CHARS, SINGLE_W, TAB_STOP, WRAP_CELLS, draw, image_parts, letters,
};
pub use prompt::instructions::{
    FileReader, InstructionFile, InstructionReadError, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
    TRUNCATION_MARKER, collect as collect_instructions, load_system_md,
    render as render_instructions,
};
pub use prompt::{
    D2_PREFERENCE_LINE, DEFAULT_DOCS_LINE, PromptInput, build as build_prompt, prefix_bytes,
};
pub use skills::{
    BodyInput, MAX_BODY_BYTES, MAX_DESCRIPTION_CHARS, PluginRejection, RegisteredSkill,
    SkillConflict, SkillError, SkillRegistration, SkillRegistry, validate_registration,
};
