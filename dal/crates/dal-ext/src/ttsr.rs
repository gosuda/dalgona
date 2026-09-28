//! Rule loading, matching, delivery, and reports for dal.

/// Build an immutable rule set from registered records and local files.
pub mod build;
/// Decide when a visible rule reminder may repeat.
pub mod gate;
/// Detect repeated settled turns without scanning the journal.
pub mod lane;
/// Match bounded regular expressions over streamed output.
pub mod matcher;
/// Read streamed tool arguments for rule matching.
pub mod readers;
/// Validate rules registered by an extension.
pub mod record;
/// Parse rule-file front matter and body text.
pub mod rulefile;
/// Parse output scopes and match tool paths.
pub mod scope;
/// Render rule fires and status notices.
pub mod texts;
mod value;

pub use value::{
    Bucket, CONDITION_MAX_BYTES, ConditionSource, InterruptMode, NAME_MAX_BYTES, Name, Origin,
    Problem, ProblemKind, RepeatMode, Rule, RuleAction, ScopeSpec, Severity, ToolPat, ToolScope,
    cut_utf8, name_is_valid,
};
