//! Typed path pulls over a possibly unfinished document.
//!
//! [`pull`] answers "what is at this path" on the latest snapshot of a
//! document that may still be being written: call it again with more text
//! and `Ok(None)` answers become `Ok(Some(..))` as the bytes arrive. It
//! is synchronous and allocation-light — it re-scans the current text
//! each call, which is the right shape for tool-argument-sized inputs.

use std::{error::Error, fmt, ops::Range};

use sonic_rs::Value;

use crate::mend::{
    Segment,
    lex::Lexer,
    scan::{Kind, Located, Probe, select_value},
};

/// One component of a pulled JSON path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullSegment<'a> {
    /// One exact object member name.
    Key(&'a str),
    /// Canonical object member name followed by accepted aliases.
    ///
    /// Selection is atomic and resolves the first matching occurrence in
    /// source order. An empty set can never match.
    Keys(&'a [&'a str]),
    /// Array element index.
    Index(usize),
}

impl PullSegment<'_> {
    pub(crate) fn names(&self) -> &[&str] {
        match self {
            Self::Key(key) => std::slice::from_ref(key),
            Self::Keys(keys) => keys,
            Self::Index(_) => &[],
        }
    }

    fn owned(&self) -> Segment {
        match self {
            Self::Key(key) => Segment::Key((*key).into()),
            Self::Keys(keys) => {
                Segment::Key(keys.first().map_or_else(|| "".into(), |k| (*k).into()))
            }
            Self::Index(index) => Segment::Index(*index),
        }
    }
}

/// Readiness requested from [`pull`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullMode {
    /// Resolve when the selected value's first token arrives.
    Started,
    /// Resolve when the selected value is structurally complete.
    Complete,
    /// Resolve with stable decoded string bytes after the supplied decoded
    /// offset.
    Chunk(usize),
    /// Resolve with complete decoded lines after the supplied decoded
    /// offset.
    ///
    /// While the string remains open, the frontier is rounded down to the
    /// last newline. The final unterminated line is emitted when the
    /// string closes.
    Line(usize),
}

/// JSON shape observed by a started pull.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PulledValueKind {
    /// JSON null.
    Null,
    /// Boolean.
    Boolean,
    /// Number.
    Number,
    /// String.
    String,
    /// Array.
    Array,
    /// Object.
    Object,
}

/// Payload resolved by a path-addressed pull.
#[derive(Clone, Debug, PartialEq)]
pub enum PulledKind {
    /// A value has started but may not yet be complete.
    Started(PulledValueKind),
    /// A complete parsed value.
    Complete(Value),
    /// Stable decoded string output.
    Chunk {
        /// Non-overlapping decoded bytes beginning at the requested offset.
        value: Box<str>,
        /// Whether the string's closing quote has arrived.
        complete: bool,
    },
}

/// One path-addressed pull result.
#[derive(Clone, Debug, PartialEq)]
pub struct Pulled {
    /// Resolved payload.
    pub kind: PulledKind,
    /// Half-open source span of the selected value as observed.
    pub span: Range<usize>,
    /// Actual object key spelling selected by a terminal key segment.
    pub matched_key: Option<Box<str>>,
}

/// Kind of pull failure represented by [`PullIssue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullIssueKind {
    /// The requested member was absent when its container completed.
    Missing,
    /// The document ended before the pulled value's closing token.
    Incomplete,
    /// A complete pulled value could not be parsed.
    Malformed,
    /// A value was present with a different JSON shape.
    TypeMismatch {
        /// Shape observed in the input.
        found: &'static str,
    },
}

impl fmt::Display for PullIssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("missing"),
            Self::Incomplete => f.write_str("incomplete"),
            Self::Malformed => f.write_str("malformed"),
            Self::TypeMismatch { found } => write!(f, "found {found}"),
        }
    }
}

/// Why a pull could not produce the requested shape.
#[derive(Debug, Clone, PartialEq)]
pub struct PullIssue {
    /// Full key/index path pulled by the caller (canonical key for an
    /// alias segment).
    pub path: Vec<Segment>,
    /// Shape requested.
    pub expected: &'static str,
    /// Why the pull could not produce that shape.
    pub kind: PullIssueKind,
}

impl fmt::Display for PullIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid JSON pull {:?}: expected {} ({})",
            self.path, self.expected, self.kind
        )
    }
}

impl Error for PullIssue {}

/// Pull the value at `path` out of `src`.
///
/// `ended` marks the input final: a scalar at end of input is complete
/// only when `ended` is true, and a still-unanswered pull reports
/// [`PullIssueKind::Incomplete`] instead of `Ok(None)`.
///
/// # Errors
/// [`PullIssue`] on a missing member, a type mismatch, a malformed
/// complete value, or a final input that ends mid-answer. `Ok(None)`
/// means the question is unanswerable *yet* — more input may resolve it.
pub fn pull(
    src: &str,
    path: &[PullSegment<'_>],
    mode: PullMode,
    ended: bool,
) -> Result<Option<Pulled>, PullIssue> {
    let mut lex = Lexer::new(src);
    lex.ws();
    let probe = select_value(&mut lex, path, ended, 0);
    let located = match probe {
        Probe::Pending => {
            return if ended {
                Err(issue(path, "value", PullIssueKind::Incomplete))
            } else {
                Ok(None)
            };
        }
        Probe::Missing => return Err(issue(path, "present member", PullIssueKind::Missing)),
        Probe::Type { expected, found } => {
            return Err(PullIssue {
                path: owned(path),
                expected,
                kind: PullIssueKind::TypeMismatch { found },
            });
        }
        Probe::Located(located) => located,
    };
    let Located {
        start,
        end,
        kind,
        matched_key,
    } = located;
    let span = start..end.unwrap_or(src.len());
    let kind_result = match mode {
        PullMode::Started => Ok(PulledKind::Started(value_kind(&kind))),
        PullMode::Complete => {
            if end.is_none() {
                return if ended {
                    Err(issue(path, "complete value", PullIssueKind::Incomplete))
                } else {
                    Ok(None)
                };
            }
            match &kind {
                Kind::Null => Ok(PulledKind::Complete(Value::new_null())),
                Kind::Bool(v) => Ok(PulledKind::Complete(Value::from(*v))),
                Kind::Number(number) => Ok(PulledKind::Complete(number_value(number))),
                Kind::String { value, .. } => Ok(PulledKind::Complete(Value::from(value.as_str()))),
                Kind::Array | Kind::Object => match crate::mend::parse(&src[span.clone()]) {
                    Ok(value) => Ok(PulledKind::Complete(value)),
                    Err(_) => Err(issue(path, "parseable value", PullIssueKind::Malformed)),
                },
            }
        }
        PullMode::Chunk(emitted) | PullMode::Line(emitted) => match &kind {
            Kind::String { value, stable } => {
                let Some(remaining) = value.get(emitted..*stable) else {
                    return Err(issue(path, "valid string offset", PullIssueKind::Malformed));
                };
                let upper = match mode {
                    PullMode::Line(_) if end.is_none() => remaining
                        .rfind('\n')
                        .map_or(emitted, |off| emitted + off + 1),
                    _ => *stable,
                };
                if upper == emitted && end.is_none() {
                    return Ok(None);
                }
                let Some(chunk) = value.get(emitted..upper) else {
                    return Err(issue(path, "valid string offset", PullIssueKind::Malformed));
                };
                Ok(PulledKind::Chunk {
                    value: chunk.into(),
                    complete: end.is_some(),
                })
            }
            other => Err(PullIssue {
                path: owned(path),
                expected: "string",
                kind: PullIssueKind::TypeMismatch {
                    found: other.name(),
                },
            }),
        },
    };
    kind_result.map(|kind| {
        Some(Pulled {
            kind,
            span,
            matched_key,
        })
    })
}

fn owned(path: &[PullSegment<'_>]) -> Vec<Segment> {
    path.iter().map(PullSegment::owned).collect()
}

fn issue(path: &[PullSegment<'_>], expected: &'static str, kind: PullIssueKind) -> PullIssue {
    PullIssue {
        path: owned(path),
        expected,
        kind,
    }
}

const fn value_kind(kind: &Kind) -> PulledValueKind {
    match kind {
        Kind::Null => PulledValueKind::Null,
        Kind::Bool(_) => PulledValueKind::Boolean,
        Kind::Number(_) => PulledValueKind::Number,
        Kind::String { .. } => PulledValueKind::String,
        Kind::Array => PulledValueKind::Array,
        Kind::Object => PulledValueKind::Object,
    }
}

fn number_value(number: &crate::mend::lex::Num) -> Value {
    match number {
        crate::mend::lex::Num::U64(v) => Value::from(*v),
        crate::mend::lex::Num::I64(v) => Value::from(*v),
        crate::mend::lex::Num::F64(v) => Value::new_f64(*v).unwrap_or_else(Value::new_null),
    }
}
