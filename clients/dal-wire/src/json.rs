//! Lossless JSON envelope parsing shared by every wire transport.
//!
//! Each envelope here becomes a [`sonic_rs::Value`] DOM, and a plain DOM
//! parse coerces integers outside `i64`/`u64` to `f64`, re-spelling exact
//! numeric text: a status payload or tool argument carrying
//! `18446744073709551617` would arrive as `1.8446744073709552e+19`. These
//! parsers use sonic's raw-number mode so every number keeps its exact
//! text while typed bounded integers still decode through
//! `as_i64`/`as_u64` (raw-number nodes reparse on demand).

use sonic_rs::Value;

/// Encodes one serde value as a JSON envelope value, preserving every
/// number's exact numeric text.
///
/// Typed values carrying raw-JSON members cannot pass through the direct
/// [`sonic_rs::to_value`] adapter: `RawJson` serializes through a
/// lazy-value token the adapter renders as a placeholder object, while
/// this conversion round-trips through exact text and parses back with
/// raw numbers kept. The returned value re-serializes exactly, including
/// through `sonic_rs::json!` leaves, whose raw-number nodes carry the
/// adapter's recognized raw-number token.
///
/// # Errors
///
/// Returns the encode or parse error.
pub(crate) fn encode_value_lossless<T: serde::ser::Serialize>(
    value: &T,
) -> Result<Value, sonic_rs::Error> {
    parse_value_lossless(&sonic_rs::to_string(value)?)
}

/// Parses one JSON envelope preserving every number's exact numeric text.
///
/// Trailing bytes are rejected exactly like [`sonic_rs::from_str`]: the
/// inherent [`sonic_rs::Deserializer::deserialize`] alone is a prefix
/// parse, so [`sonic_rs::Deserializer::end`] must run after it.
///
/// # Errors
///
/// Returns the parse error for malformed JSON or non-whitespace trailing
/// bytes.
pub(crate) fn parse_value_lossless(text: &str) -> Result<Value, sonic_rs::Error> {
    let mut deserializer = sonic_rs::Deserializer::from_str(text).use_rawnumber();
    let value = deserializer.deserialize()?;
    deserializer.end()?;
    Ok(value)
}

/// Parses one frame of JSON envelopes into their values, preserving every
/// number's exact numeric text.
///
/// Trailing bytes are rejected exactly like [`sonic_rs::from_str`].
///
/// # Errors
///
/// Returns the parse error for malformed JSON or non-whitespace trailing
/// bytes.
pub(crate) fn parse_value_seq_lossless(text: &str) -> Result<Vec<Value>, sonic_rs::Error> {
    let mut deserializer = sonic_rs::Deserializer::from_str(text).use_rawnumber();
    let values = deserializer.deserialize()?;
    deserializer.end()?;
    Ok(values)
}
