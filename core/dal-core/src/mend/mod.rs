//! `mend`: tolerant JSON codec for model-generated payloads.
//!
//! Language models routinely emit JSON-shaped text that is not JSON:
//! single-quoted strings, unquoted keys, `//` comments, Python literals,
//! `0x` numbers, stray commas, unescaped apostrophes, and bareword values.
//! This module accepts that dialect on purpose and records every tolerance
//! it applies as a [`Repair`] entry, so a caller can always distinguish
//! "the payload was valid" from "the payload was recovered".
//!
//! The boundary is `serde`: [`from_str`] and [`from_slice`] drive any
//! `T: Deserialize` straight off the tolerant lexer with no intermediate
//! tree, [`parse`] decodes the shared [`sonic_rs::Value`] DOM, and
//! [`canonicalize`] emits a byte-valid [`RawJson`] plus its [`RepairLog`].
//! [`parse_partial`] never fails — it auto-closes truncated containers
//! and rolls incomplete trailing atoms back to the last valid prefix —
//! for callers that display a document while it is still being written.
//! [`classify`] answers the orthogonal strict question "is this byte
//! prefix complete, extendable, or dead", and [`pull`] reads a typed
//! value at a path out of a possibly unfinished document.
//!
//! Strict JSON parses unchanged and produces an empty repair log.
//! Malformed structure that would silently change the meaning — missing
//! separators, key-like colons inside barewords, non-finite numbers,
//! truncated values — still fails rather than guessing.

mod classify;
mod de;
mod error;
mod evidence;
mod lex;
mod pull;
mod scan;
#[cfg(test)]
mod tests;
mod value;

pub use classify::{Prefix, classify};
pub use de::{Deserializer, from_slice, from_str};
pub use error::ParseError;
pub use evidence::{Repair, RepairKind, RepairLog, Segment};
pub use lex::MAX_DEPTH;
pub use pull::{
    PullIssue, PullIssueKind, PullMode, PullSegment, Pulled, PulledKind, PulledValueKind, pull,
};
pub use value::{canonicalize, parse, parse_partial};

/// JSON insignificant whitespace.
pub(crate) const fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Index of the first byte at or after `i` that is not whitespace or part
/// of a `//` line or `/* */` block comment. A comment that runs to end of
/// input counts as insignificant.
pub(crate) fn skip_insignificant(s: &[u8], mut i: usize) -> usize {
    let n = s.len();
    loop {
        while i < n && is_whitespace(s[i]) {
            i += 1;
        }
        if i + 1 < n && s[i] == b'/' {
            match s[i + 1] {
                b'/' => {
                    i += 2;
                    while i < n && s[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'*' => {
                    i += 2;
                    while i + 1 < n && !(s[i] == b'*' && s[i + 1] == b'/') {
                        i += 1;
                    }
                    i = (i + 2).min(n);
                    continue;
                }
                _ => {}
            }
        }
        return i;
    }
}

/// Read exactly four hexadecimal digits at `i` as a code unit.
pub(crate) fn hex4(s: &[u8], i: usize) -> Option<u32> {
    let mut unit = 0u32;
    for k in 0..4 {
        let d = (*s.get(i + k)? as char).to_digit(16)?;
        unit = unit * 16 + d;
    }
    Some(unit)
}
