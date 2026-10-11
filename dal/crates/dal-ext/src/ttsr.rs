//! Rule loading, matching, delivery, and reports for dal.

/// Build an immutable rule set from registered records and local files.
pub mod build;
/// The thin `ttsr` extension record for product assembly.
pub mod extension;
/// Decide when a visible rule reminder may repeat.
pub mod gate;
/// Gate judged rules on bool verdicts within a bounded lane.
pub mod judged;
/// Detect repeated settled turns without scanning the journal.
pub mod lane;
/// Match bounded regular expressions over streamed output.
pub mod matcher;
/// Read streamed tool arguments for rule matching.
pub mod readers;
/// Validate rules registered by an extension.
pub mod record;
/// Render the `dalgon rules` report and the offline test prover.
pub mod report;
/// Parse rule-file front matter and body text.
pub mod rulefile;
/// Parse output scopes and match tool paths.
pub mod scope;
/// Render rule fires and status notices.
pub mod texts;
mod value;
/// Watch the output stream, resolve fire actions, and deliver fires.
pub mod watch;

pub use extension::extension;
pub use value::{
    Bucket, CONDITION_MAX_BYTES, ConditionSource, InterruptMode, NAME_MAX_BYTES, Name, Origin,
    Problem, ProblemKind, RepeatMode, Rule, RuleAction, ScopeSpec, Severity, ToolPat, ToolScope,
    cut_utf8, name_is_valid,
};
pub use watch::factory::TtsrWatchFactory;
