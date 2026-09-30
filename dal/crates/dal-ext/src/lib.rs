//! Built-in extension composition and rendering for dal.

/// Built-in commands that report and manage the active session.
pub mod commands;
pub mod compact;
pub mod docs;
pub mod docsgen;
pub mod judge;
pub mod letter;
pub mod prompt;
pub mod skills;
pub mod subagent;
pub mod ttsr;

pub use judge::{
    Gate, GateSetting, Judge, JudgeConfig, JudgeError, JudgeOpen, JudgeQuestion, Verdict,
};
pub use letter::extension as letter_extension;
pub use letter::{
    BYTE_BUDGET, DOUBLE_W, DrawError, DrawOutcome, FallbackReason, Font, GLYPH_H, Glyph,
    GlyphError, Glyphs, IMAGE_BUDGET, Image, LINE_H, LetterAssembly, LetterChunk, LetterFallback,
    LetterKind, LetterRoute, LetterSession, LetterState, MARGIN, MAX_DESC_CHARS, SINGLE_W,
    SkillLetterRecord, TAB_STOP, WRAP_CELLS, classify_letter_path, draw, gone_source_error,
    image_parts, is_zero_width, letters, malformed_id_error, missing_id_error, over_budget_notice,
    parse_letter_id, render_failed_notice, undrawable_notice,
};
pub use prompt::instructions::{
    FileReader, InstructionFile, InstructionReadError, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
    TRUNCATION_MARKER, collect as collect_instructions, load_system_md,
    render as render_instructions,
};
pub use prompt::{
    D2_PREFERENCE_LINE, DEFAULT_DOCS_LINE, PromptInput, build as build_prompt,
    extension as prompt_extension, prefix_bytes,
};
pub use skills::{
    BodyInput, MAX_BODY_BYTES, MAX_DESCRIPTION_CHARS, PluginRejection, RegisteredSkill,
    SkillConflict, SkillError, SkillRegistration, SkillRegistry, extension as skills_extension,
    section, validate_registration,
};
