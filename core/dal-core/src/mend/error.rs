//! Parse and deserialization errors.

use std::fmt::Display;

use serde::de;

use crate::mend::MAX_DEPTH;

/// Failure modes of [`from_str`](crate::mend::from_str),
/// [`parse`](crate::mend::parse), and [`pull`](crate::mend::pull).
/// Positions are byte offsets into the input text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Input ended (or was empty) before a value started.
    #[error("unexpected end of JSON input")]
    UnexpectedEnd,
    /// Non-whitespace content after a complete top-level value.
    #[error("unexpected trailing characters at position {0}")]
    TrailingCharacters(usize),
    /// Input ended inside an object.
    #[error("unterminated object")]
    UnterminatedObject,
    /// Input ended inside an array.
    #[error("unterminated array")]
    UnterminatedArray,
    /// Input ended inside a string literal.
    #[error("unterminated string")]
    UnterminatedString,
    /// An object member started with something that cannot be a key.
    #[error("expected object key at position {0}")]
    ExpectedKey(usize),
    /// Missing `:` between an object key and its value.
    #[error("expected ':' in object at position {0}")]
    ExpectedColon(usize),
    /// Input ended where an object member's value was required.
    #[error("expected value after ':' at position {0}")]
    ExpectedValue(usize),
    /// Missing separator after an object member.
    #[error("expected ',' or '}}' in object at position {0}")]
    ExpectedCommaOrBrace(usize),
    /// Missing separator after an array element.
    #[error("expected ',' or ']' in array at position {0}")]
    ExpectedCommaOrBracket(usize),
    /// Malformed or non-finite numeric token (`1..2`, `1e999`, bare `-`).
    #[error("invalid number at position {0}")]
    InvalidNumber(usize),
    /// A token no recovery applies to (`NaN`, `undefined`, a bareword
    /// where none is allowed, or one that would swallow structure).
    #[error("unexpected token at position {0}")]
    UnexpectedToken(usize),
    /// The input is not UTF-8; the value is `str::from_utf8`'s
    /// `valid_up_to` byte offset.
    #[error("input is not valid UTF-8 before position {0}")]
    InvalidUtf8(usize),
    /// Container nesting exceeded the recursion limit.
    #[error("nesting deeper than {} levels", MAX_DEPTH)]
    DepthExceeded,
    /// Typed-deserialization mismatch surfaced through [`serde::de::Error`]
    /// (a string where a `u32` field was expected, and so on).
    #[error("{0}")]
    Custom(Box<str>),
}

impl de::Error for ParseError {
    fn custom<T: Display>(msg: T) -> Self {
        Self::Custom(msg.to_string().into())
    }
}
