//! The tolerant lexer: token-level primitives every reader in this module
//! shares.
//!
//! One grammar, no modes. Every reader here is strict about structure:
//! an unterminated string or container is an error. Callers that tolerate
//! truncated input ([`pull`](crate::mend::pull), [`parse_partial`](
//! crate::mend::parse_partial)) get there by measuring progress
//! ([`Lexer::string_progress`], [`measure`](crate::mend::scan)) or by
//! repairing the text first — never by switching the grammar.
//!
//! Accepted dialect over RFC 8259:
//!
//! - single-quoted strings and unquoted object keys;
//! - trailing, leading, and doubled commas between container entries;
//! - `//` line and `/* */` block comments;
//! - Python literals `True` / `False` / `None`, `0x` / `0b` integers;
//! - relaxed number spellings (`+1`, `.5`, `1.`);
//! - raw control characters and invalid `\x` escapes inside strings, kept
//!   literally;
//! - unescaped quotes inside strings — a quote only closes when the next
//!   significant byte ends a value, so `'it's'` recovers;
//! - unquoted bareword values in value position (`{"paths": pkgs/foo/*}`),
//!   bounded by delimiters and structure guards.
//!
//! Every accepted deviation is recorded as a [`Repair`]; a strict-JSON
//! input always yields an empty log.

use std::{borrow::Cow, mem, ops::Range};

use crate::mend::{
    ParseError, Repair, RepairKind, RepairLog, Segment, hex4, is_whitespace, skip_insignificant,
};

/// Maximum container nesting before the parser refuses. Applies to both
/// the strict and the partial paths.
pub const MAX_DEPTH: u32 = 128;

/// A numeric token, classified for `serde` visiting.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Num {
    /// Non-negative integer that fits `u64`.
    U64(u64),
    /// Negative integer that fits `i64`.
    I64(i64),
    /// Anything else that is finite.
    F64(f64),
}

/// A keyword literal: standard JSON plus Python `True`/`False`/`None`.
#[derive(Clone, Copy)]
pub(crate) enum Atom {
    /// `true` / `false` / `True` / `False`.
    Bool(bool),
    /// `null` / `None`.
    Null,
}

/// Decoded state of a string token at the current buffer edge.
pub(crate) struct StringProgress<'a> {
    /// Decoded content so far (the whole string when `complete`).
    pub(crate) value: Cow<'a, str>,
    /// Length of the decoded prefix whose meaning cannot change when more
    /// bytes arrive — excludes a trailing split escape and everything from
    /// a quote whose close/inner reading is still undecidable at the edge.
    pub(crate) stable: usize,
    /// A closing quote was consumed.
    pub(crate) complete: bool,
}

/// Reading of the lookahead past a candidate closing quote.
enum Look {
    /// A value terminator (or end of input) follows: the quote closes.
    Closes,
    /// Ordinary content follows: the quote is literal string content.
    Inner,
    /// The lookahead ends on a lone `/` at the buffer edge, which may grow
    /// into a comment and flip this quote from inner to closing.
    Undecided,
}

const KEYWORDS: [(&str, Atom); 6] = [
    ("true", Atom::Bool(true)),
    ("false", Atom::Bool(false)),
    ("null", Atom::Null),
    ("True", Atom::Bool(true)),
    ("False", Atom::Bool(false)),
    ("None", Atom::Null),
];

const fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Cursor over the input with the tolerant token readers.
pub(crate) struct Lexer<'a> {
    src: &'a str,
    s: &'a [u8],
    i: usize,
    repairs: RepairLog,
    path: Vec<Segment>,
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        Self {
            src,
            s: src.as_bytes(),
            i: 0,
            repairs: RepairLog::default(),
            path: Vec::new(),
        }
    }

    pub(crate) const fn pos(&self) -> usize {
        self.i
    }

    pub(crate) const fn at_end(&self) -> bool {
        self.i >= self.s.len()
    }

    pub(crate) fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    pub(crate) const fn bump(&mut self) {
        self.i += 1;
    }

    /// Repairs recorded so far.
    pub(crate) fn repairs(&self) -> &[Repair] {
        self.repairs.as_slice()
    }

    /// Consume the rest of the input (partial-read rollback of an
    /// unrecognized trailing token).
    pub(crate) const fn skip_to_end(&mut self) {
        self.i = self.s.len();
    }

    pub(crate) fn take_repairs(&mut self) -> RepairLog {
        mem::take(&mut self.repairs)
    }

    pub(crate) fn push_path(&mut self, segment: Segment) {
        self.path.push(segment);
    }

    pub(crate) fn pop_path(&mut self) {
        self.path.pop();
    }

    /// Record a comma that strict JSON would reject: leading, doubled, or
    /// immediately before `close` (trailing).
    pub(crate) fn record_comma(&mut self, at: usize, trailing: bool) {
        self.record(
            at..at + 1,
            if trailing {
                RepairKind::TrailingComma
            } else {
                RepairKind::StrayComma
            },
            "",
        );
    }

    fn record(&mut self, span: Range<usize>, kind: RepairKind, after: impl Into<String>) {
        self.repairs.push(Repair {
            before: self.src[span.clone()].into(),
            span,
            path: self.path.clone(),
            kind,
            after: after.into().into_boxed_str(),
        });
    }

    /// Skip whitespace plus `//` line and `/* */` block comments. A comment
    /// running to end of input counts as consumed; the enclosing parse
    /// reports the real error (unterminated container, missing token).
    pub(crate) fn ws(&mut self) {
        loop {
            while self.i < self.s.len() && is_whitespace(self.s[self.i]) {
                self.i += 1;
            }
            if self.i + 1 >= self.s.len() || self.s[self.i] != b'/' {
                return;
            }
            let start = self.i;
            match self.s[self.i + 1] {
                b'/' => {
                    self.i += 2;
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                b'*' => {
                    self.i += 2;
                    while self.i + 1 < self.s.len()
                        && !(self.s[self.i] == b'*' && self.s[self.i + 1] == b'/')
                    {
                        self.i += 1;
                    }
                    self.i = if self.i + 1 < self.s.len() {
                        self.i + 2
                    } else {
                        self.s.len()
                    };
                }
                _ => return,
            }
            self.record(start..self.i, RepairKind::Comment, "");
        }
    }

    /// Read a complete string starting at the opening `quote`.
    ///
    /// # Errors
    /// [`ParseError::UnterminatedString`] when no closing quote follows.
    pub(crate) fn string(&mut self, quote: u8) -> Result<Cow<'a, str>, ParseError> {
        let progress = self.string_progress(quote);
        if progress.complete {
            Ok(progress.value)
        } else {
            Err(ParseError::UnterminatedString)
        }
    }

    /// Read a string and retain the incremental information a progress
    /// reader needs. Unterminated input reports `complete: false` with the
    /// decoded-so-far value rather than erroring; `stable` bounds which of
    /// it is final.
    #[expect(
        clippy::too_many_lines,
        reason = "one contiguous token state machine; splitting it would obscure the flow"
    )]
    pub(crate) fn string_progress(&mut self, quote: u8) -> StringProgress<'a> {
        let s = self.s;
        let n = s.len();
        let mut i = self.i + 1;
        let string_start = self.i;
        let mut out: Option<String> = None;
        let mut run_start = i;
        let mut unstable_from = None;
        while i < n {
            let b = s[i];
            if b < 0x20 {
                let escaped = match b {
                    b'\n' => "\\n",
                    b'\r' => "\\r",
                    b'\t' => "\\t",
                    b'\x08' => "\\b",
                    b'\x0c' => "\\f",
                    _ => "",
                };
                if escaped.is_empty() {
                    self.record(
                        i..i + 1,
                        RepairKind::RawControlCharacter,
                        format!("\\u{b:04X}"),
                    );
                } else {
                    self.record(i..i + 1, RepairKind::RawControlCharacter, escaped);
                }
                i += 1;
                continue;
            }
            if b != b'\\' && b != quote {
                i += 1;
                continue;
            }
            if b == quote {
                match self.quote_lookahead(i + 1) {
                    Look::Closes => {
                        self.i = i + 1;
                        let value = finish(out, &self.src[run_start..i]);
                        let stable = value.len();
                        if quote == b'\'' {
                            self.record(
                                string_start..self.i,
                                RepairKind::SingleQuotedString,
                                quote_value(&value),
                            );
                        }
                        return StringProgress {
                            value,
                            stable,
                            complete: true,
                        };
                    }
                    // Nothing from the quote on is stable yet.
                    Look::Undecided => {
                        if unstable_from.is_none() {
                            unstable_from =
                                Some(out.as_ref().map_or(0, String::len) + (i - run_start));
                        }
                    }
                    Look::Inner => {
                        self.record(i..i + 1, RepairKind::InnerQuote, escaped_quote(quote));
                    }
                }
                i += 1;
                continue;
            }
            // Backslash escape.
            let out = out.get_or_insert_with(String::new);
            out.push_str(&self.src[run_start..i]);
            let escape_output_start = out.len();
            i += 1;
            if i >= n {
                unstable_from = Some(escape_output_start);
                out.push('\\');
                run_start = i;
                break;
            }
            match s[i] {
                b'"' => out.push('"'),
                b'\'' => out.push('\''),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{0008}'),
                b'f' => out.push('\u{000C}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    if let Some(unit) = hex4(s, i + 1) {
                        i += 4;
                        if let Some(ch) = char::from_u32(unit) {
                            out.push(ch);
                        } else if (0xd800..0xdc00).contains(&unit)
                            && s.get(i + 1) == Some(&b'\\')
                            && s.get(i + 2) == Some(&b'u')
                            && let Some(low) = hex4(s, i + 3)
                            && (0xdc00..0xe000).contains(&low)
                        {
                            // Surrogate pair split across two \u escapes.
                            let combined = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                            if let Some(ch) = char::from_u32(combined) {
                                out.push(ch);
                            }
                            i += 6;
                        } else {
                            // A high surrogate at the buffer edge may acquire its
                            // low surrogate in the next fragment: not stable yet.
                            if (0xd800..0xdc00).contains(&unit) && i + 7 > n {
                                unstable_from = Some(escape_output_start);
                            }
                            // Lone surrogate: representable in a JS string, not in Rust.
                            out.push('\u{FFFD}');
                        }
                    } else {
                        if i + 5 > n {
                            unstable_from = Some(escape_output_start);
                        } else {
                            self.record(i - 1..i + 1, RepairKind::InvalidEscape, "\\\\u");
                        }
                        out.push_str("\\u");
                    }
                }
                _ => {
                    // Invalid escape — keep the backslash and the escaped
                    // character literal.
                    let ch = self.src[i..].chars().next().unwrap_or('\u{FFFD}');
                    let mut replacement = String::from("\\\\");
                    replacement.push(ch);
                    self.record(
                        i - 1..i + ch.len_utf8(),
                        RepairKind::InvalidEscape,
                        replacement,
                    );
                    out.push('\\');
                    out.push(ch);
                    i += ch.len_utf8() - 1;
                }
            }
            i += 1;
            run_start = i;
        }
        // Unterminated string: report the decoded progress; the strict
        // caller decides whether that is an error.
        self.i = i;
        let value = finish(out, &self.src[run_start..n]);
        let stable = unstable_from.unwrap_or(value.len());
        StringProgress {
            value,
            stable,
            complete: false,
        }
    }

    /// Classify the lookahead after a candidate closing quote: a quote
    /// closes a string only when the next significant byte (past
    /// whitespace and comments) ends a value.
    fn quote_lookahead(&self, from: usize) -> Look {
        let k = skip_insignificant(self.s, from);
        match self.s.get(k) {
            None | Some(b',' | b'}' | b']' | b':') => Look::Closes,
            Some(b'/') if k + 1 == self.s.len() => Look::Undecided,
            Some(_) => Look::Inner,
        }
    }

    /// Read a numeric token.
    ///
    /// # Errors
    /// [`ParseError::InvalidNumber`] on a malformed or non-finite token.
    pub(crate) fn number(&mut self) -> Result<Num, ParseError> {
        let start = self.i;
        while self.i < self.s.len()
            && matches!(
                self.s[self.i],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'x' | b'X' | b'o' | b'O' | b'a'..=b'f' | b'A'..=b'F'
            )
        {
            self.i += 1;
        }
        let token = &self.src[start..self.i];
        match parse_number_token(token) {
            Some(number) => {
                let bytes = token.as_bytes();
                let radix = bytes.len() > 2
                    && bytes[0] == b'0'
                    && matches!(bytes[1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O');
                if radix {
                    self.record(
                        start..self.i,
                        RepairKind::RadixNumber,
                        number_spelling(&number),
                    );
                } else if token.starts_with('+')
                    || token.starts_with('.')
                    || token.ends_with('.')
                    || (token.len() > 1 && token.starts_with('0') && bytes[1].is_ascii_digit())
                {
                    self.record(
                        start..self.i,
                        RepairKind::RelaxedNumber,
                        number_spelling(&number),
                    );
                }
                Ok(number)
            }
            None => Err(ParseError::InvalidNumber(start)),
        }
    }

    /// Match a keyword literal at the cursor; consumes only on success.
    /// Requires a non-identifier boundary so `Truex` / `nullish` are not
    /// misread as a keyword followed by junk.
    pub(crate) fn match_keyword(&mut self) -> Option<Atom> {
        for (word, atom) in KEYWORDS {
            if self.s[self.i..].starts_with(word.as_bytes())
                && !self
                    .s
                    .get(self.i + word.len())
                    .copied()
                    .is_some_and(is_ident_char)
            {
                let start = self.i;
                self.i += word.len();
                if word.as_bytes()[0].is_ascii_uppercase() {
                    let after = match atom {
                        Atom::Bool(true) => "true",
                        Atom::Bool(false) => "false",
                        Atom::Null => "null",
                    };
                    self.record(start..self.i, RepairKind::PythonLiteral, after);
                }
                return Some(atom);
            }
        }
        None
    }

    /// Consume a `null` / `None` literal if present (for `Option` fields).
    /// Never consumes on a non-null keyword such as `true`.
    pub(crate) fn eat_null(&mut self) -> bool {
        for word in ["null", "None"] {
            if self.s[self.i..].starts_with(word.as_bytes())
                && !self
                    .s
                    .get(self.i + word.len())
                    .copied()
                    .is_some_and(is_ident_char)
            {
                let start = self.i;
                self.i += word.len();
                if word == "None" {
                    self.record(start..self.i, RepairKind::PythonLiteral, "null");
                }
                return true;
            }
        }
        false
    }

    /// Read an unquoted object key: everything up to a structural
    /// delimiter or whitespace. May be empty — the caller decides whether
    /// that is an error.
    pub(crate) fn unquoted_key(&mut self) -> &'a str {
        let start = self.i;
        while self.i < self.s.len() {
            let b = self.s[self.i];
            if matches!(b, b':' | b',' | b'}') || is_whitespace(b) {
                break;
            }
            self.i += 1;
        }
        if self.i > start {
            self.record(
                start..self.i,
                RepairKind::UnquotedKey,
                quote_value(&self.src[start..self.i]),
            );
        }
        &self.src[start..self.i]
    }

    /// Recover an unquoted string value, e.g. `{"paths": pkgs/foo/*}`:
    /// consume until `,` / `}` / `]` / newline and trim trailing
    /// whitespace. Recovery fails — never accepts a half-formed or
    /// non-finite argument — when the token:
    /// - hits end of input before a delimiter (truncated value);
    /// - contains a `"`, `{`, `[`, or a key-like `:` — this lexer accepts
    ///   unquoted keys, so a missed comma (`{"a": foo "b": 1}`) would
    ///   otherwise silently swallow the following field. A colon followed
    ///   by `/` or `\` stays literal so URL and Windows-path values
    ///   recover;
    /// - is a non-finite JS atom (`NaN` / `Infinity` / `undefined`).
    ///
    /// # Errors
    /// [`ParseError::UnexpectedToken`] under any of those guards.
    pub(crate) fn bareword(&mut self) -> Result<&'a str, ParseError> {
        let s = self.s;
        let start = self.i;
        let mut i = start;
        while i < s.len() {
            let b = s[i];
            if matches!(b, b',' | b'}' | b']' | b'\n' | b'\r') {
                break;
            }
            if b == b'"'
                || b == b'{'
                || b == b'['
                || (b == b':' && !matches!(s.get(i + 1).copied(), Some(b'/' | b'\\')))
            {
                return Err(ParseError::UnexpectedToken(start));
            }
            i += 1;
        }
        if i >= s.len() {
            return Err(ParseError::UnexpectedToken(start));
        }
        let mut end = i;
        while end > start && is_whitespace(s[end - 1]) {
            end -= 1;
        }
        let word = &self.src[start..end];
        if matches!(
            word,
            "NaN" | "Infinity" | "-Infinity" | "+Infinity" | "undefined"
        ) {
            return Err(ParseError::UnexpectedToken(start));
        }
        self.i = i;
        self.record(start..end, RepairKind::BarewordValue, quote_value(word));
        Ok(word)
    }
}

/// Assemble the final string: borrowed when nothing needed unescaping.
fn finish(owned: Option<String>, tail: &str) -> Cow<'_, str> {
    match owned {
        None => Cow::Borrowed(tail),
        Some(mut out) => {
            out.push_str(tail);
            Cow::Owned(out)
        }
    }
}

/// Canonical spelling of a repaired scalar for the repair log.
fn quote_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Canonical spelling of an inner-quote recovery for the repair log.
fn escaped_quote(quote: u8) -> &'static str {
    if quote == b'"' { "\\\"" } else { "\\'" }
}

/// Canonical spelling of a recovered number for the repair log.
fn number_spelling(number: &Num) -> String {
    match number {
        Num::U64(v) => v.to_string(),
        Num::I64(v) => v.to_string(),
        Num::F64(v) => v.to_string(),
    }
}

/// Parse a relaxed numeric token with JS `Number()` semantics: decimal
/// (optional sign, leading/trailing dot, exponent) plus unsigned `0x` hex
/// and `0b` binary. Integers that fit stay exact integers; everything else
/// is `f64`. `None` for malformed or non-finite tokens — unlike JS, an
/// overflow-to-infinity (`1e999`) is rejected rather than surfaced.
fn parse_number_token(token: &str) -> Option<Num> {
    let bytes = token.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'0' && bytes[1] | 0x20 == b'x' {
        return parse_radix(&bytes[2..], 16);
    }
    if bytes.len() > 2 && bytes[0] == b'0' && bytes[1] | 0x20 == b'b' {
        return parse_radix(&bytes[2..], 2);
    }
    if bytes.len() > 2 && bytes[0] == b'0' && bytes[1] | 0x20 == b'o' {
        return parse_radix(&bytes[2..], 8);
    }
    let signed = token.strip_prefix('+').unwrap_or(token);
    let digits = signed.strip_prefix('-').unwrap_or(signed);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        if signed.starts_with('-') {
            if let Ok(value) = signed.parse::<i64>() {
                return Some(Num::I64(value));
            }
        } else if let Ok(value) = digits.parse::<u64>() {
            return Some(Num::U64(value));
        }
        // Out-of-range integer: fall through to f64 like JS.
    }
    signed
        .parse::<f64>()
        .ok()
        .filter(|f| f.is_finite())
        .map(Num::F64)
}

/// Fold hex/binary digits into an integer, spilling to `f64` on overflow
/// the way JS `Number("0x…")` loses precision instead of failing.
#[expect(
    clippy::cast_precision_loss,
    reason = "radix fold intentionally spills to f64 like JS Number"
)]
fn parse_radix(digits: &[u8], radix: u32) -> Option<Num> {
    if digits.is_empty() {
        return None;
    }
    let mut int = 0u64;
    let mut float = 0f64;
    let mut overflowed = false;
    for &b in digits {
        let d = u64::from((b as char).to_digit(radix)?);
        if !overflowed {
            if let Some(v) = int
                .checked_mul(u64::from(radix))
                .and_then(|v| v.checked_add(d))
            {
                int = v;
                continue;
            }
            overflowed = true;
            float = int as f64;
        }
        float = float.mul_add(f64::from(radix), d as f64);
    }
    if overflowed {
        if float.is_finite() {
            Some(Num::F64(float))
        } else {
            None
        }
    } else {
        Some(Num::U64(int))
    }
}
