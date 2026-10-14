//! Repair evidence: the explicit record of every tolerance the codec
//! applied to non-standard input.

use std::ops::Range;

/// One component of the JSON path at which a tolerance was applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Object member name.
    Key(Box<str>),
    /// Array element index.
    Index(usize),
}

/// Exact tolerant-grammar branch that accepted non-standard JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairKind {
    /// A line or block comment was ignored.
    Comment,
    /// A string used single rather than double quotes.
    SingleQuotedString,
    /// An object key was not quoted.
    UnquotedKey,
    /// A Python-spelled literal was accepted.
    PythonLiteral,
    /// An extra comma was ignored between container entries.
    StrayComma,
    /// A comma immediately before a container close was ignored.
    TrailingComma,
    /// A string escape was not part of the JSON escape vocabulary and was
    /// kept literally.
    InvalidEscape,
    /// A raw control character occurred inside a string.
    RawControlCharacter,
    /// A hexadecimal or binary integer literal was accepted.
    RadixNumber,
    /// A number used a relaxed spelling such as a leading plus or bare dot.
    RelaxedNumber,
    /// An unquoted bareword was recovered as a string value.
    BarewordValue,
    /// An unescaped quote inside a string was treated as literal content.
    InnerQuote,
}

/// One lossless record of a tolerant grammar branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repair {
    /// Half-open byte range in the source text the repair covers.
    pub span: Range<usize>,
    /// JSON path being parsed when the tolerance fired. Key-level repairs
    /// (unquoted keys, a single-quoted key) carry the enclosing path; the
    /// span still identifies the key exactly.
    pub path: Vec<Segment>,
    /// Exact tolerant branch that fired.
    pub kind: RepairKind,
    /// Source spelling covered by `span`.
    pub before: Box<str>,
    /// Canonical spelling or replacement. Empty when bytes were dropped.
    pub after: Box<str>,
}

/// Append-only repair record for one parse.
///
/// Empty means the input was already strict JSON — the primary way a
/// caller distinguishes "parsed" from "repaired".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepairLog(Vec<Repair>);

impl RepairLog {
    /// Number of recorded repairs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no tolerance branch fired — strict JSON input.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Repairs in source order.
    #[must_use]
    pub fn as_slice(&self) -> &[Repair] {
        &self.0
    }

    /// Iterate repairs in source order.
    pub fn iter(&self) -> impl Iterator<Item = &Repair> {
        self.0.iter()
    }

    pub(crate) fn push(&mut self, repair: Repair) {
        self.0.push(repair);
    }
}

impl<'a> IntoIterator for &'a RepairLog {
    type IntoIter = std::slice::Iter<'a, Repair>;
    type Item = &'a Repair;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
