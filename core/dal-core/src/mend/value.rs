//! Value-level entry points: decode into [`sonic_rs::Value`], canonical
//! [`RawJson`] emission, and the never-failing partial read.

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use sonic_rs::{Array, Object, Value};

use crate::{
    RawJson,
    mend::{Deserializer, ParseError, RepairLog, lex::Atom, lex::Lexer, lex::MAX_DEPTH, lex::Num},
};

/// Final-parse a JSON value, accepting and normalizing the accepted
/// malformations.
///
/// Equivalent to [`from_str`](crate::mend::from_str) with `Value` as the
/// target. Strict JSON parses unchanged; truncated input and trailing
/// garbage fail.
///
/// # Errors
/// [`ParseError`] as [`from_str`](crate::mend::from_str).
pub fn parse(text: &str) -> Result<Value, ParseError> {
    let mut de = Deserializer::new(text);
    let value = DomSeed.deserialize(&mut de)?;
    de.end()?;
    Ok(value)
}

/// Read a possibly-incomplete document during streaming.
///
/// Total: empty or unrecoverable input yields an empty object, truncated
/// containers auto-close, and an incomplete trailing atom (a
/// half-streamed keyword, number, or bareword) rolls back to the last
/// valid prefix of its enclosing container rather than committing junk.
#[must_use]
pub fn parse_partial(text: &str) -> Value {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Value::new_object();
    }
    Partial {
        lex: Lexer::new(trimmed),
    }
    .root()
    .unwrap_or_else(Value::new_object)
}

/// Decode `text` into the canonical carrier plus its repair evidence.
///
/// `Some` only when the tolerant parse yields a complete document that
/// re-serializes to byte-valid JSON; the [`RepairLog`] is empty iff the
/// input was already strict JSON, so callers can tell "parsed" from
/// "repaired".
#[must_use]
pub fn canonicalize(text: &str) -> Option<(RawJson, RepairLog)> {
    let mut de = Deserializer::new(text);
    let value = DomSeed.deserialize(&mut de).ok()?;
    de.end().ok()?;
    let repairs = de.into_repairs();
    let canonical = sonic_rs::to_string(&value).ok()?;
    let raw = RawJson::parse(&canonical).ok()?;
    Some((raw, repairs))
}

/// Seed producing [`Value`] through the tolerant grammar. `sonic_rs`'s
/// own `Deserialize for Value` only accepts its private raw-token fast
/// path, so the DOM is built by a visitor here — the same visitor shape
/// every `T: Deserialize` exercises.
struct DomSeed;

impl<'de> DeserializeSeed<'de> for DomSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(Dom)
    }
}

struct Dom;

impl<'de> Visitor<'de> for Dom {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a valid JSON value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::new_null())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::new_null())
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(Value::from(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(Value::from(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(Value::from(v))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Value::new_f64(v).ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(Value::from(v))
    }

    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E> {
        Ok(Value::from(v))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(Value::from(v.as_str()))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut out = Array::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(value) = seq.next_element_seed(DomSeed)? {
            out.push(value);
        }
        Ok(Value::from(out))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut out = Object::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((key, value)) = map.next_entry_seed(KeySeed, DomSeed)? {
            out.insert(&key, value);
        }
        Ok(Value::from(out))
    }
}

struct KeySeed;

impl<'de> DeserializeSeed<'de> for KeySeed {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_string(KeyVisitor)
    }
}

struct KeyVisitor;

impl Visitor<'_> for KeyVisitor {
    type Value = String;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an object key")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(v.to_owned())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(v)
    }
}

/// Total tree builder for [`parse_partial`]. Unlike the visitor-driven
/// strict path this materializes containers directly: an incomplete
/// trailing atom rolls the enclosing object/array back to its last valid
/// prefix — something a one-pass visitor cannot undo.
struct Partial<'a> {
    lex: Lexer<'a>,
}

impl Partial<'_> {
    fn root(&mut self) -> Option<Value> {
        self.lex.ws();
        self.lex.peek()?;
        self.value(0)
    }

    /// `None` marks an incomplete atom (or a too-deep container) at the
    /// edge; the enclosing container retains its prefix instead.
    fn value(&mut self, depth: u32) -> Option<Value> {
        match self.lex.peek()? {
            b'{' | b'[' if depth >= MAX_DEPTH => {
                self.lex.skip_to_end();
                None
            }
            b'{' => Some(self.object(depth + 1)),
            b'[' => Some(self.array(depth + 1)),
            quote @ (b'"' | b'\'') => {
                let progress = self.lex.string_progress(quote);
                Some(Value::from(progress.value.as_ref()))
            }
            b'-' | b'+' | b'.' | b'0'..=b'9' => {
                let number = self.lex.number().ok()?;
                // A number ending at the buffer edge can still extend
                // (more digits, a fraction, an exponent): roll it back.
                if self.lex.at_end() {
                    return None;
                }
                Some(match number {
                    Num::U64(v) => Value::from(v),
                    Num::I64(v) => Value::from(v),
                    Num::F64(v) => Value::new_f64(v).unwrap_or_else(Value::new_null),
                })
            }
            _ => match self.lex.match_keyword() {
                Some(Atom::Bool(b)) => Some(Value::from(b)),
                Some(Atom::Null) => Some(Value::new_null()),
                None => {
                    // Incomplete or unrecognized token at the edge.
                    self.lex.skip_to_end();
                    None
                }
            },
        }
    }

    fn object(&mut self, depth: u32) -> Value {
        self.lex.bump();
        let mut out = Object::new();
        loop {
            self.lex.ws();
            let Some(b) = self.lex.peek() else { break };
            match b {
                b'}' => {
                    self.lex.bump();
                    break;
                }
                // Tolerate leading / doubled / trailing commas.
                b',' => {
                    self.lex.bump();
                    continue;
                }
                _ => {}
            }
            let key = match self.lex.peek() {
                Some(quote @ (b'"' | b'\'')) => self.lex.string_progress(quote).value.into_owned(),
                _ => self.lex.unquoted_key().to_string(),
            };
            self.lex.ws();
            if self.lex.peek() == Some(b':') {
                self.lex.bump();
            } else {
                break;
            }
            self.lex.ws();
            if self.lex.at_end() {
                break;
            }
            let Some(value) = self.value(depth) else {
                break;
            };
            out.insert(&key, value);
            self.lex.ws();
            match self.lex.peek() {
                Some(b',') => self.lex.bump(),
                Some(b'}') => {
                    self.lex.bump();
                    break;
                }
                _ => break,
            }
        }
        Value::from(out)
    }

    fn array(&mut self, depth: u32) -> Value {
        self.lex.bump();
        let mut out = Array::new();
        loop {
            self.lex.ws();
            let Some(b) = self.lex.peek() else { break };
            match b {
                b']' => {
                    self.lex.bump();
                    break;
                }
                b',' => {
                    self.lex.bump();
                    continue;
                }
                _ => {}
            }
            let Some(value) = self.value(depth) else {
                break;
            };
            out.push(value);
            self.lex.ws();
            match self.lex.peek() {
                Some(b',') => self.lex.bump(),
                Some(b']') => {
                    self.lex.bump();
                    break;
                }
                _ => break,
            }
        }
        Value::from(out)
    }
}
