//! Typed model judgments, per-session gate resolution, and bounded ledgering.
mod config;
mod format;
mod machine;
mod record;
#[cfg(test)]
mod tests;
mod types;
pub use config::{GateSetting, JudgeConfig};
pub use format::{SYSTEM_LINE, parse_answers, render_envelope};
pub use machine::{Judge, JudgeOpen};
pub use types::{Gate, JudgeError, JudgeQuestion, Verdict};
/// Maximum shared-context size, in UTF-8 bytes.
pub const SHARED_MAX: usize = 16_384;
/// Maximum prompt size for one typed question, in UTF-8 bytes.
pub const PROMPT_MAX: usize = 4_096;
/// Maximum option size, in UTF-8 bytes.
pub const OPTION_MAX: usize = 200;
/// Maximum number of questions in one model request.
pub const BATCH_MAX: usize = 32;
/// Prefix for a startup error when the explicitly enabled role cannot resolve.
pub const STARTUP_UNAVAILABLE_PREFIX: &str = "judge unavailable: ";
/// The exact reason used when the judge role has no usable credentials.
pub const REASON_NO_CREDENTIALS: &str = "no credentials for the judge role";
/// Template for the one session-open note emitted when `auto` resolves off.
pub const AUTO_OFF_NOTE: &str = "judge: auto resolved off ({reason}); judge-fed features are off";
/// Prefix for the notice emitted on the third consecutive judge failure.
pub const STREAK_NOTICE_PREFIX: &str = "judge: 3 consecutive failures (last: ";
