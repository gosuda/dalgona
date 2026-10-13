//! Typed argument readers for TTSR tool sources.
//!
//! A reader turns the raw argument bytes of one tool call into item and
//! added-text events while the call streams. Each byte is examined once; the
//! state is bounded by the nesting, key, path, and header-line limits below.

mod json;
mod lines;
#[cfg(test)]
mod tests;

use json::{JsonReader, Select};
use lines::{LineStyle, PatchReader};

/// Maximum open JSON containers before emission stops until close.
const MAX_FRAMES: usize = 64;
/// Maximum decoded key bytes that may still match a member name.
const MAX_KEY_BYTES: usize = 256;
/// Maximum decoded path bytes that may still emit a path event.
const MAX_PATH_BYTES: usize = 4096;
/// Maximum bytes of one held patch header candidate line.
const MAX_HOLD_BYTES: usize = MAX_PATH_BYTES + 256;

const KEY_PATH: u8 = 1;
const KEY_SELECTED: u8 = 2;
const REPLACEMENT: &str = "\u{FFFD}";

/// One event read from a tool call's argument stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReaderEvent {
    /// An item begins, for example one file of a patch.
    ItemStart,
    /// The path of the current item.
    Path(String),
    /// Decoded text that the call adds, in stream order.
    Added(String),
    /// The current item ends.
    ItemEnd,
}

/// A reader for the argument stream of one tool call.
pub trait ArgReader: Send {
    /// Reads the next argument delta and appends its events in order.
    fn feed(&mut self, delta: &str, out: &mut Vec<ReaderEvent>);
    /// Ends the call and appends the events still held, closing every open
    /// item innermost first.
    fn close(&mut self, out: &mut Vec<ReaderEvent>);
}

/// The active edit style of the patch tool, one per patch dialect.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EditStyle {
    /// `*** File:` sections with Find and action bodies.
    Anchor,
    /// JSON `changes` entries with `new` and `create` members.
    Replace,
    /// `[path#TAG]` sections with `PUT` row bodies.
    Hashline,
    /// Codex `*** Begin Patch` envelopes.
    ApplyPatch,
}

/// Returns the argument reader for one call of `tool`.
///
/// The patch tool uses the reader of `edit_style`: replace style emits the
/// `new` and `create` members; the line styles emit item start, path, added
/// rows only, and item end. Every other tool uses the JSON reader that emits
/// every string value.
#[must_use]
pub fn reader_for(tool: &str, edit_style: EditStyle) -> Box<dyn ArgReader> {
    if tool != "patch" {
        return Box::new(JsonReader::new(Select::EveryString));
    }
    match edit_style {
        EditStyle::Replace => Box::new(JsonReader::new(Select::Members(&["new", "create"]))),
        EditStyle::Anchor => Box::new(PatchReader::new(LineStyle::Anchor)),
        EditStyle::Hashline => Box::new(PatchReader::new(LineStyle::Hashline)),
        EditStyle::ApplyPatch => Box::new(PatchReader::new(LineStyle::ApplyPatch)),
    }
}
