//! Structural measurement over the tolerant grammar: how far one value
//! extends and what shape it has, without building the DOM. Used by
//! [`pull`](crate::mend::pull) to locate a path inside a possibly
//! unfinished document; `None`-ish progress is reported as pending so a
//! caller can retry once more bytes arrive.

use crate::mend::lex::{Atom, Lexer, MAX_DEPTH, Num};

/// Shape of a located value.
#[derive(Clone, Debug)]
pub(crate) enum Kind {
    /// `null` / `None`.
    Null,
    /// `true` / `false` / `True` / `False`.
    Bool(bool),
    /// Any finite number.
    Number(Num),
    /// String with its decoded content and stable frontier.
    String { value: String, stable: usize },
    /// `[`-started value.
    Array,
    /// `{`-started value.
    Object,
}

impl Kind {
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::String { .. } => "string",
            Self::Array => "array",
            Self::Object => "object",
        }
    }
}

/// A located value: `end` is `Some` only when the value is complete.
pub(crate) struct Located {
    pub(crate) start: usize,
    pub(crate) end: Option<usize>,
    pub(crate) kind: Kind,
    pub(crate) matched_key: Option<Box<str>>,
}

/// Outcome of measuring at a position or along a path.
pub(crate) enum Probe {
    /// The value is there (possibly unfinished: `end` unset).
    Located(Located),
    /// Bytes ran out before the question could be answered.
    Pending,
    /// The container closed without the requested member.
    Missing,
    /// A value was present with a different JSON shape.
    Type {
        expected: &'static str,
        found: &'static str,
    },
}

/// A scalar is complete only once a value terminator follows — or the
/// input is final and ended. An edge token may still extend (digits) or
/// reopen (inner-quote recovery).
fn scalar_complete(lex: &mut Lexer<'_>, ended: bool) -> bool {
    lex.ws();
    matches!(lex.peek(), Some(b',' | b'}' | b']')) || (ended && lex.at_end())
}

/// Measure one value at the cursor.
pub(crate) fn scan_value(lex: &mut Lexer<'_>, ended: bool, depth: u32) -> Probe {
    let start = lex.pos();
    let Some(byte) = lex.peek() else {
        return Probe::Pending;
    };
    match byte {
        b'{' => scan_object(lex, ended, depth, start),
        b'[' => scan_array(lex, ended, depth, start),
        quote @ (b'"' | b'\'') => {
            let progress = lex.string_progress(quote);
            let end = lex.pos();
            let complete = progress.complete && scalar_complete(lex, ended);
            Probe::Located(Located {
                start,
                end: complete.then_some(end),
                kind: Kind::String {
                    value: progress.value.into_owned(),
                    stable: progress.stable,
                },
                matched_key: None,
            })
        }
        b'-' | b'+' | b'.' | b'0'..=b'9' => {
            let Ok(number) = lex.number() else {
                return Probe::Pending;
            };
            let end = lex.pos();
            let complete = scalar_complete(lex, ended);
            Probe::Located(Located {
                start,
                end: complete.then_some(end),
                kind: Kind::Number(number),
                matched_key: None,
            })
        }
        _ => {
            if let Some(atom) = lex.match_keyword() {
                let end = lex.pos();
                let complete = scalar_complete(lex, ended);
                let kind = match atom {
                    Atom::Bool(value) => Kind::Bool(value),
                    Atom::Null => Kind::Null,
                };
                Probe::Located(Located {
                    start,
                    end: complete.then_some(end),
                    kind,
                    matched_key: None,
                })
            } else if let Ok(word) = lex.bareword() {
                let end = lex.pos();
                let complete = scalar_complete(lex, ended);
                Probe::Located(Located {
                    start,
                    end: complete.then_some(end),
                    kind: Kind::String {
                        value: word.to_string(),
                        stable: word.len(),
                    },
                    matched_key: None,
                })
            } else {
                Probe::Pending
            }
        }
    }
}

/// Measure one object from its `{`.
fn scan_object(lex: &mut Lexer<'_>, ended: bool, depth: u32, start: usize) -> Probe {
    if depth >= MAX_DEPTH {
        return Probe::Pending;
    }
    lex.bump();
    loop {
        lex.ws();
        match lex.peek() {
            None => return incomplete(start, Kind::Object),
            Some(b'}') => {
                lex.bump();
                return Probe::Located(Located {
                    start,
                    end: Some(lex.pos()),
                    kind: Kind::Object,
                    matched_key: None,
                });
            }
            Some(b',') => {
                let comma = lex.pos();
                lex.bump();
                lex.ws();
                lex.record_comma(comma, lex.peek() == Some(b'}'));
                continue;
            }
            _ => {}
        }
        match lex.peek() {
            Some(quote @ (b'"' | b'\'')) => {
                let progress = lex.string_progress(quote);
                if !progress.complete {
                    return incomplete(start, Kind::Object);
                }
            }
            Some(_) => {
                if lex.unquoted_key().is_empty() {
                    return incomplete(start, Kind::Object);
                }
            }
            None => return incomplete(start, Kind::Object),
        }
        lex.ws();
        if lex.peek() != Some(b':') {
            return incomplete(start, Kind::Object);
        }
        lex.bump();
        lex.ws();
        match scan_value(lex, ended, depth + 1) {
            Probe::Located(Located { end: Some(_), .. }) => {}
            _ => return incomplete(start, Kind::Object),
        }
        lex.ws();
        match lex.peek() {
            Some(b',') => {
                let comma = lex.pos();
                lex.bump();
                lex.ws();
                if lex.peek() == Some(b'}') {
                    lex.record_comma(comma, true);
                }
            }
            Some(b'}') => {
                lex.bump();
                return Probe::Located(Located {
                    start,
                    end: Some(lex.pos()),
                    kind: Kind::Object,
                    matched_key: None,
                });
            }
            _ => return incomplete(start, Kind::Object),
        }
    }
}

/// Measure one array from its `[`.
fn scan_array(lex: &mut Lexer<'_>, ended: bool, depth: u32, start: usize) -> Probe {
    if depth >= MAX_DEPTH {
        return Probe::Pending;
    }
    lex.bump();
    loop {
        lex.ws();
        match lex.peek() {
            None => return incomplete(start, Kind::Array),
            Some(b']') => {
                lex.bump();
                return Probe::Located(Located {
                    start,
                    end: Some(lex.pos()),
                    kind: Kind::Array,
                    matched_key: None,
                });
            }
            Some(b',') => {
                let comma = lex.pos();
                lex.bump();
                lex.ws();
                lex.record_comma(comma, lex.peek() == Some(b']'));
                continue;
            }
            _ => {}
        }
        match scan_value(lex, ended, depth + 1) {
            Probe::Located(Located { end: Some(_), .. }) => {}
            _ => return incomplete(start, Kind::Array),
        }
        lex.ws();
        match lex.peek() {
            Some(b',') => {
                let comma = lex.pos();
                lex.bump();
                lex.ws();
                if lex.peek() == Some(b']') {
                    lex.record_comma(comma, true);
                }
            }
            Some(b']') => {
                lex.bump();
                return Probe::Located(Located {
                    start,
                    end: Some(lex.pos()),
                    kind: Kind::Array,
                    matched_key: None,
                });
            }
            _ => return incomplete(start, Kind::Array),
        }
    }
}

const fn incomplete(start: usize, kind: Kind) -> Probe {
    Probe::Located(Located {
        start,
        end: None,
        kind,
        matched_key: None,
    })
}

/// Continue a path walk at the current position.
pub(crate) fn select_value(
    lex: &mut Lexer<'_>,
    path: &[crate::mend::PullSegment<'_>],
    ended: bool,
    depth: u32,
) -> Probe {
    let Some(byte) = lex.peek() else {
        return Probe::Pending;
    };
    if path.is_empty() {
        return scan_value(lex, ended, depth);
    }
    match (path[0], byte) {
        (
            segment @ (crate::mend::PullSegment::Key(_) | crate::mend::PullSegment::Keys(_)),
            b'{',
        ) => select_key(lex, segment.names(), &path[1..], ended, depth),
        (crate::mend::PullSegment::Index(index), b'[') => {
            select_index(lex, index, &path[1..], ended, depth)
        }
        (crate::mend::PullSegment::Key(_) | crate::mend::PullSegment::Keys(_), _) => Probe::Type {
            expected: "object",
            found: byte_name(byte),
        },
        (crate::mend::PullSegment::Index(_), _) => Probe::Type {
            expected: "array",
            found: byte_name(byte),
        },
    }
}

/// Walk object members for the first key in `wanted` (source order —
/// first match wins, unlike DOM last-wins semantics).
fn select_key(
    lex: &mut Lexer<'_>,
    wanted: &[&str],
    rest: &[crate::mend::PullSegment<'_>],
    ended: bool,
    depth: u32,
) -> Probe {
    if depth >= MAX_DEPTH {
        return Probe::Pending;
    }
    lex.bump();
    loop {
        lex.ws();
        match lex.peek() {
            None => return Probe::Pending,
            Some(b'}') => {
                lex.bump();
                return Probe::Missing;
            }
            Some(b',') => {
                let comma = lex.pos();
                lex.bump();
                lex.ws();
                lex.record_comma(comma, lex.peek() == Some(b'}'));
                continue;
            }
            _ => {}
        }
        let actual_key: String = match lex.peek() {
            Some(quote @ (b'"' | b'\'')) => {
                let progress = lex.string_progress(quote);
                if !progress.complete {
                    return Probe::Pending;
                }
                progress.value.into_owned()
            }
            Some(_) => {
                let key = lex.unquoted_key();
                if key.is_empty() {
                    return Probe::Pending;
                }
                key.to_string()
            }
            None => return Probe::Pending,
        };
        let key_matches = wanted.iter().any(|name| *name == actual_key);
        lex.ws();
        if lex.peek() != Some(b':') {
            return Probe::Pending;
        }
        lex.bump();
        lex.ws();
        if lex.at_end() {
            return Probe::Pending;
        }
        if key_matches {
            let mut probe = select_value(lex, rest, ended, depth + 1);
            if rest.is_empty()
                && let Probe::Located(located) = &mut probe
            {
                located.matched_key = Some(actual_key.into_boxed_str());
            }
            return probe;
        }
        match scan_value(lex, ended, depth + 1) {
            Probe::Located(Located { end: Some(_), .. }) => {}
            _ => return Probe::Pending,
        }
        lex.ws();
        match lex.peek() {
            Some(b',') => {
                let comma = lex.pos();
                lex.bump();
                lex.ws();
                if lex.peek() == Some(b'}') {
                    lex.record_comma(comma, true);
                }
            }
            Some(b'}') => {
                lex.bump();
                return Probe::Missing;
            }
            _ => return Probe::Pending,
        }
    }
}

/// Walk array elements to `wanted`.
fn select_index(
    lex: &mut Lexer<'_>,
    wanted: usize,
    rest: &[crate::mend::PullSegment<'_>],
    ended: bool,
    depth: u32,
) -> Probe {
    if depth >= MAX_DEPTH {
        return Probe::Pending;
    }
    lex.bump();
    let mut index = 0usize;
    loop {
        lex.ws();
        match lex.peek() {
            None => return Probe::Pending,
            Some(b']') => {
                lex.bump();
                return Probe::Missing;
            }
            Some(b',') => {
                let comma = lex.pos();
                lex.bump();
                lex.ws();
                lex.record_comma(comma, lex.peek() == Some(b']'));
                continue;
            }
            _ => {}
        }
        if index == wanted {
            return select_value(lex, rest, ended, depth + 1);
        }
        match scan_value(lex, ended, depth + 1) {
            Probe::Located(Located { end: Some(_), .. }) => {}
            _ => return Probe::Pending,
        }
        index += 1;
        lex.ws();
        match lex.peek() {
            Some(b',') => {
                let comma = lex.pos();
                lex.bump();
                lex.ws();
                if lex.peek() == Some(b']') {
                    lex.record_comma(comma, true);
                }
            }
            Some(b']') => {
                lex.bump();
                return Probe::Missing;
            }
            _ => return Probe::Pending,
        }
    }
}

/// Byte-to-shape name for mismatch diagnostics.
const fn byte_name(byte: u8) -> &'static str {
    match byte {
        b'{' => "object",
        b'[' => "array",
        b'"' | b'\'' => "string",
        b'-' | b'+' | b'.' | b'0'..=b'9' => "number",
        b't' | b'f' | b'T' | b'F' => "boolean",
        b'n' | b'N' => "null",
        _ => "value",
    }
}
