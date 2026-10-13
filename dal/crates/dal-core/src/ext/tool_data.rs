//! Typed tool output data carried beside display text (R06 E06 P06 T-E04).
//!
//! Read, search, find, and symbol owners return these values so evidence
//! flows with the data instead of being reconstructed from text. None of
//! these types implements `Deserialize`: views are minted by the host, and
//! a caller that only holds text has no evidence.

use std::num::NonZeroU64;

use super::{RawJson, SessionId};

/// The recipient an observation is delivered to (R06).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Consumer {
    /// The root model surface of the turn.
    Model,
    /// One host-minted invocation, by its invocation number.
    Invocation(NonZeroU64),
}

/// Host-only delivery record bound into a view's header (R06 E06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Provenance {
    /// The consumer-bound observation reference.
    pub reference: Box<str>,
    /// The consumer the view was delivered to.
    pub consumer: Consumer,
    /// The session the observation belongs to.
    pub session: SessionId,
}

/// One physical source line as delivered (R06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceRow {
    /// The one-based physical line number.
    pub line: u64,
    /// The exact line text.
    pub text: Box<str>,
    /// Whether the full line is present; truncated or folded rows carry no
    /// full-line evidence.
    pub complete: bool,
}

/// A typed text-read view (R06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReadView {
    /// The read path as the read owner spelled it.
    pub path: Box<str>,
    /// The consumer-bound header reference.
    pub header: Box<str>,
    /// The delivered rows, in file order.
    pub rows: Box<[SourceRow]>,
    /// Whether the read stopped before the end of the file.
    pub truncated: bool,
    /// Host-only provenance, absent before delivery.
    pub provenance: Option<Provenance>,
}

/// One search match with its intact source view (R06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SearchHit {
    /// The matching file path.
    pub path: Box<str>,
    /// The one-based physical line number.
    pub line: u64,
    /// The exact line text.
    pub text: Box<str>,
    /// The intact source view this hit was taken from.
    pub source: ReadView,
}

/// A typed grep search page (R06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SearchPage {
    /// The matches, in search order.
    pub matches: Box<[SearchHit]>,
    /// Whether the search stopped before scanning everything.
    pub truncated: bool,
}

/// One find-mode entry (R06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FindEntry {
    /// The matched file path.
    pub path: Box<str>,
    /// The owner's entry kind label.
    pub kind: Box<str>,
}

/// A typed find page with its own schema (R06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FindPage {
    /// The entries, in find order.
    pub entries: Box<[FindEntry]>,
    /// Whether the find stopped before scanning everything.
    pub truncated: bool,
}

/// One symbol-mode hit (R06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SymbolHit {
    /// The file containing the symbol.
    pub path: Box<str>,
    /// The first line of the symbol span.
    pub first: u64,
    /// The last line of the symbol span.
    pub last: u64,
    /// The symbol kind label.
    pub kind: Box<str>,
    /// The symbol name.
    pub name: Box<str>,
}

/// A typed symbol page with its own schema (R06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SymbolPage {
    /// The hits, in symbol order.
    pub hits: Box<[SymbolHit]>,
    /// Whether the scan stopped before finishing.
    pub truncated: bool,
}

/// A node of the display output tree (P06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ViewNode {
    /// A text block.
    Text(Box<str>),
    /// A table with column labels and raw JSON cells.
    Table {
        /// The column labels.
        columns: Box<[Box<str>]>,
        /// One row per entry, each with one cell per column.
        rows: Box<[Box<[RawJson]>]>,
    },
    /// An intact source view.
    Source(ReadView),
    /// A nested group of nodes.
    Group(Box<[ViewNode]>),
}

/// Typed tool output data returned beside display text (R06 E06 P06).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ToolData {
    /// A text read view.
    Read(ReadView),
    /// A grep search page.
    Search(SearchPage),
    /// A find page.
    Find(FindPage),
    /// A symbol page.
    Symbols(SymbolPage),
    /// The intact views found inside an eval result tree (R06 E06).
    Views(Box<[ReadView]>),
    /// A display tree (P06).
    Display(ViewNode),
}
