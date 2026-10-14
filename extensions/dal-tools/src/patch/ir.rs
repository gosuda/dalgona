//! Shared patch edit vocabulary and observer boundary.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use serde::Serialize;

/// The edit language selected for one provider request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DialectId {
    /// Find-and-action patch sections.
    Anchor,
    /// Structured old/new replacements.
    Replace,
    /// Snapshot-tagged line operations.
    Hashline,
    /// Snapshot-bound relaxed line operations.
    HashlineLight,
    /// Snapshot-bound observed line operations.
    HashlineEnhanced,
    /// Codex-compatible patch envelopes.
    ApplyPatch,
}

/// The prompt-intensity metadata for one concrete dialect.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Tier {
    /// Replace and apply-patch styles.
    Simple,
    /// Anchor style.
    Balanced,
    /// Hashline style.
    Strict,
}

/// The coordinates and proof form used by one edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Locator {
    /// Find one or more copies of exact or canonically equivalent text.
    Text {
        /// The copied source text.
        old: String,
        /// Optional one-based starting line.
        line_hint: Option<usize>,
        /// Replace every match.
        all: bool,
        /// The text window is relative to the original payload or previous edit.
        window: Window,
        /// Optional surrounding context for patch dialects.
        context: Option<String>,
        /// The match ends at end-of-file.
        at_eof: bool,
    },
    /// A line span quoted by its boundary or all of its lines.
    Span {
        /// First one-based line.
        first: usize,
        /// Last one-based line.
        last: usize,
        /// Quoted source lines.
        quoted: Vec<String>,
    },
    /// A one-based inclusive line range.
    Lines {
        /// First one-based line.
        first: usize,
        /// Last one-based line, inclusive.
        last: usize,
    },
    /// An insertion gap before a one-based line.
    Gap {
        /// Insert before this one-based line.
        before_line: usize,
    },
    /// The outermost named syntax node starting at this one-based line.
    Node {
        /// One-based line where the node begins.
        first_line: usize,
    },
    /// A syntax definition selected by name and optional source-order ordinal.
    Symbol {
        /// Definition name.
        name: String,
        /// One-based source-order duplicate selector.
        ordinal: Option<usize>,
        /// Optional unique fragment within the selected definition.
        old: Option<String>,
    },
    /// The whole file; this locator requires whole-file proof.
    Whole,
}

/// The edit action applied at its resolved location.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// Replace the selected bytes.
    Replace,
    /// Insert before the selected bytes.
    InsertBefore,
    /// Insert after the selected bytes.
    InsertAfter,
}

/// The proof required before an edit may remove bytes or create a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Guard {
    /// A bound snapshot reference token (`r<boot>.<seq>`) for new profiles.
    Reference(String),
    /// Exact copied source bytes prove the edit.
    Quoted,
    /// A whole-file tag proves the source version.
    WholeTag(String),
    /// The session has displayed all removed lines for this digest.
    Seen,
    /// A version tag proves the source version.
    Version(String),
    /// A current definition tag proves the selected syntax node.
    DefTag(String),
    /// The destination path must be absent.
    Absent,
    /// The source path must exist.
    Exists,
}

/// A position window used when edits in one payload share source bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Window {
    /// Resolve against the bytes before the payload began.
    BeforePayload,
    /// Resolve after the preceding payload edit.
    AfterPrevious,
}

/// One parsed operation. Dialects produce this IR and never stage or write files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Replace or insert bytes in an existing file.
    Change {
        /// Zero-based edit position in the payload.
        index: usize,
        /// Workspace-relative source path.
        path: PathBuf,
        /// Source locator and its proof.
        locator: Locator,
        /// Selected action.
        action: Action,
        /// Proof required by the engine.
        guard: Guard,
        /// Replacement or inserted text.
        body: String,
        /// Coordinate window for this edit.
        window: Window,
    },
    /// Create a file at an absent path.
    Create {
        /// Zero-based edit position in the payload.
        index: usize,
        /// Workspace-relative target path.
        path: PathBuf,
        /// Complete file content.
        body: String,
    },
    /// Delete an existing file.
    Delete {
        /// Zero-based edit position in the payload.
        index: usize,
        /// Workspace-relative source path.
        path: PathBuf,
        /// Bound snapshot reference for new profiles; `None` for legacy dialects.
        reference: Option<String>,
    },
    /// Rename an existing file to an absent path.
    Rename {
        /// Zero-based edit position in the payload.
        index: usize,
        /// Workspace-relative source path.
        from: PathBuf,
        /// Workspace-relative destination path.
        to: PathBuf,
        /// Bound snapshot reference for new profiles; `None` for legacy dialects.
        reference: Option<String>,
    },
}

/// The error category returned to the provider and serialized output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorClass {
    /// The selected dialect rejected the payload.
    Parse,
    /// A path or edit shape could not be resolved.
    Resolve,
    /// The payload did not prove authority to remove bytes.
    Proof,
    /// A snapshot or approval became stale.
    Stale,
    /// A target is not a writable text file.
    File,
    /// A configured resource bound was exceeded.
    Limit,
    /// A staged observer blocked the change.
    Blocked,
    /// An I/O operation failed.
    Io,
}

/// A patch failure with its stable class and user-facing text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineError {
    /// The failure category.
    pub class: ErrorClass,
    /// The complete user-facing error text.
    pub message: String,
}

impl EngineError {
    /// Creates a categorized patch failure.
    #[must_use]
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EngineError {}

/// A dialect parse failure before the engine adds the current style suffix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    /// One-based input line, when the parser can identify one.
    pub line: Option<usize>,
    /// The dialect-specific explanation.
    pub message: String,
}

impl ParseError {
    /// Creates a parse failure tied to an input line.
    #[must_use]
    pub fn at_line(line: usize, message: impl Into<String>) -> Self {
        Self {
            line: Some(line),
            message: message.into(),
        }
    }

    /// Creates a parse failure not tied to one input line.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            line: None,
            message: message.into(),
        }
    }
}

/// The kind of file operation represented by a staged change.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// A newly created file.
    Create,
    /// An existing file was updated.
    Update,
    /// An existing file was deleted.
    Delete,
    /// A file was renamed.
    Rename,
}

/// One line in the human-readable staged diff.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    /// Whether the line was added, removed, or retained as context.
    pub kind: DiffLineKind,
    /// Line contents without a line-ending byte.
    pub text: Box<str>,
}

/// The role of a line in a displayed diff hunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLineKind {
    /// Context present in both versions.
    Context,
    /// A line in the post-image only.
    Added,
    /// A line in the pre-image only.
    Removed,
}

/// A unified diff hunk with three context lines.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
    /// One-based first line in the pre-image.
    pub old_start: usize,
    /// Number of pre-image lines in the hunk.
    pub old_lines: usize,
    /// One-based first line in the post-image.
    pub new_start: usize,
    /// Number of post-image lines in the hunk.
    pub new_lines: usize,
    /// Context, removed, and added lines in diff order.
    pub lines: Vec<DiffLine>,
}

/// One file entry in the display diff.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    /// Workspace-relative display path.
    pub path: Box<str>,
    /// Staged operation.
    pub op: Operation,
    /// Optional destination of a rename.
    pub renamed_to: Option<Box<str>>,
    /// Display hunks.
    pub hunks: Vec<DiffHunk>,
}

/// The output's true, bounded display diff.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    /// The stable diff discriminator.
    pub kind: Box<str>,
    /// Changed files in canonical path order.
    pub files: Vec<DiffFile>,
}

/// File-change counts returned through the tool result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// Workspace-relative display path.
    pub path: Box<str>,
    /// Staged file operation.
    pub op: Operation,
    /// Number of added lines.
    pub added: u64,
    /// Number of removed lines.
    pub removed: u64,
    /// Rename destination, when present.
    pub renamed_to: Option<Box<str>>,
}

/// A staged file as an observer sees it before approval.
#[derive(Clone, Debug)]
pub struct StagedFile<'a> {
    /// Workspace-relative display path.
    pub path: &'a Path,
    /// Canonical absolute path used for the write.
    pub absolute_path: &'a Path,
    /// Exact pre-image bytes; `None` for a new file.
    pub before: Option<&'a [u8]>,
    /// Exact post-image bytes; `None` for a deleted file.
    pub after: Option<&'a [u8]>,
    /// Display hunks computed from these immutable images.
    pub hunks: &'a [DiffHunk],
    /// Cached pre-image syntax tree, when symbols are enabled and supported.
    #[cfg(feature = "symbols")]
    pub pre_parse: Option<std::sync::Arc<crate::parse::Parsed>>,
    /// Cached post-image syntax tree, when symbols are enabled and supported.
    #[cfg(feature = "symbols")]
    pub post_parse: Option<std::sync::Arc<crate::parse::Parsed>>,
}

/// One immutable staged batch bound to the originating tool call.
#[derive(Clone, Debug)]
pub struct StagedBatch<'a> {
    /// Session that owns the patch call.
    pub session: dal_core::SessionId,
    /// Turn that owns the patch call.
    pub turn: dal_core::TurnId,
    /// Provider call identifier.
    pub call: dal_core::CallId,
    /// Complete staged write set in canonical path order.
    pub files: Vec<StagedFile<'a>>,
}

/// A synchronous, pure observer of the immutable staged write set.
pub trait EditObserver: Send + Sync + 'static {
    /// Inspects every staged image without writing or changing the plan.
    fn inspect(&self, batch: &StagedBatch<'_>) -> Vec<EditFinding>;
}

/// A rename destination resolved through the workspace containment rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameTarget {
    /// Workspace-relative normalized display path.
    pub path: PathBuf,
    /// Canonical absolute path used for the write.
    pub absolute_path: PathBuf,
}

/// A staged file owned by a complete plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedFileOwned {
    /// Workspace-relative display path.
    pub path: PathBuf,
    /// Canonical absolute path used for the write.
    pub absolute_path: PathBuf,
    /// Exact pre-image bytes; `None` for a new file.
    pub before: Option<Box<[u8]>>,
    /// Exact post-image bytes; `None` for a deleted file.
    pub after: Option<Box<[u8]>>,
    /// Operation represented by this staged target.
    pub op: Operation,
    /// Resolved rename destination, when present.
    pub renamed_to: Option<RenameTarget>,
    /// Display hunks computed from the immutable images.
    pub hunks: Vec<DiffHunk>,
}

/// A complete staged patch plan before authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    /// The style used to parse the payload.
    pub style: DialectId,
    /// The complete canonical staged write set.
    pub files: Vec<StagedFileOwned>,
    /// Observer findings in registration and return order.
    pub findings: Vec<EditFinding>,
}

/// The severity of a pure staged-edit finding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    /// Prevent approval and commit.
    Block,
    /// Report after the patch summary.
    Report,
}

/// A finding returned by one registered edit observer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditFinding {
    /// Stable rule identifier.
    pub rule: Box<str>,
    /// Whether this finding blocks the staged patch.
    pub severity: FindingSeverity,
    /// User-facing finding text.
    pub text: Box<str>,
}

/// The exact serialized patch tool result.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Output {
    /// Human-readable notes and diff echo.
    pub text: String,
    /// Error category, or `None` on success.
    pub error_class: Option<ErrorClass>,
    /// Changed paths; empty on errors except a failed restore.
    pub changes: Vec<FileChange>,
    /// The true display diff.
    pub display: Diff,
}

/// The domain-separation input for a short read/patch tag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagDomain {
    /// Whole-file read tag.
    Whole,
    /// Current version tag.
    Version,
    /// Raw definition tag, only used when syntax support is compiled in.
    #[cfg(feature = "symbols")]
    Def,
}

/// Computes the full, undomained BLAKE3 digest used by Seen and Chain.
#[must_use]
pub fn version_digest(bytes: &[u8]) -> [u8; 32] {
    crate::digest32(bytes)
}

/// Computes a domain-separated snapshot tag shared with read and search.
#[must_use]
pub fn tag(domain: TagDomain, bytes: &[u8]) -> String {
    let (domain, short) = match domain {
        TagDomain::Whole => ("whole", false),
        TagDomain::Version => ("version", true),
        #[cfg(feature = "symbols")]
        TagDomain::Def => ("def", false),
    };
    let mut tag = crate::tag8(domain, bytes);
    if short {
        tag.truncate(4);
    }
    tag
}
