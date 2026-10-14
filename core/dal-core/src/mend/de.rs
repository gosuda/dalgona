//! One-pass `serde` deserialization over the tolerant grammar.
//!
//! [`from_str`] and [`from_slice`] drive any `T: Deserialize` directly off
//! the [`Lexer`] — no intermediate tree. [`sonic_rs::Value`] is just one
//! visitor: [`parse`](crate::mend::parse) is `from_str::<Value>`.

use std::mem;

use serde::{
    de::{
        self, DeserializeSeed, Unexpected, Visitor,
        value::{BorrowedStrDeserializer, StrDeserializer},
    },
    forward_to_deserialize_any,
};

use crate::mend::{
    ParseError, Repair, RepairLog, Segment,
    lex::{Atom, Lexer, MAX_DEPTH, Num},
};

/// Deserialize `T` from tolerant JSON: strict JSON plus the malformations
/// this module's grammar accepts.
///
/// # Errors
/// [`ParseError`] on truncated input, trailing garbage, non-finite
/// numbers, or structure no recovery applies to.
pub fn from_str<'de, T: serde::Deserialize<'de>>(json: &'de str) -> Result<T, ParseError> {
    let mut de = Deserializer::new(json);
    let value = T::deserialize(&mut de)?;
    de.end()?;
    Ok(value)
}

/// Deserialize `T` from tolerant JSON bytes; the bytes must be UTF-8.
///
/// # Errors
/// [`ParseError::InvalidUtf8`] on non-UTF-8 input, otherwise as
/// [`from_str`].
pub fn from_slice<'de, T: serde::Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, ParseError> {
    let text =
        std::str::from_utf8(bytes).map_err(|error| ParseError::InvalidUtf8(error.valid_up_to()))?;
    from_str(text)
}

/// `serde` deserializer over the tolerant grammar; one pass, no
/// intermediate tree. Construct via [`from_str`] unless composing
/// manually — then call [`end`](Self::end) after the value.
pub struct Deserializer<'de> {
    lex: Lexer<'de>,
    depth: u32,
    /// Whether an unrecognized token may recover as a bareword string —
    /// true only in object/array value position.
    allow_bareword: bool,
    pending_key: Option<Box<str>>,
}

impl<'de> Deserializer<'de> {
    /// Deserializer over `json` positioned at the start; does not verify
    /// trailing content — call [`end`](Self::end) after the value.
    #[must_use]
    pub fn new(json: &'de str) -> Self {
        Self {
            lex: Lexer::new(json),
            depth: 0,
            allow_bareword: false,
            pending_key: None,
        }
    }

    /// Verify nothing but whitespace/comments remains.
    ///
    /// # Errors
    /// [`ParseError::TrailingCharacters`] on leftover input.
    pub fn end(&mut self) -> Result<(), ParseError> {
        self.lex.ws();
        if self.lex.at_end() {
            Ok(())
        } else {
            Err(ParseError::TrailingCharacters(self.lex.pos()))
        }
    }

    /// Repairs recorded so far.
    #[must_use]
    pub fn repairs(&self) -> &[Repair] {
        self.lex.repairs()
    }

    /// Take the full repair record.
    #[must_use]
    pub fn into_repairs(self) -> RepairLog {
        let mut lex = self.lex;
        lex.take_repairs()
    }

    fn peek_some(&mut self) -> Result<u8, ParseError> {
        self.lex.ws();
        match self.lex.peek() {
            Some(byte) => Ok(byte),
            None => Err(ParseError::UnexpectedEnd),
        }
    }

    const fn descend(&mut self) -> Result<(), ParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(ParseError::DepthExceeded);
        }
        self.depth += 1;
        Ok(())
    }

    /// Deserialize an object key (quoted or unquoted) into `seed`.
    fn map_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<K::Value, ParseError> {
        let start = self.lex.pos();
        if let Some(quote @ (b'"' | b'\'')) = self.lex.peek() {
            return match self.lex.string(quote)? {
                std::borrow::Cow::Borrowed(key) => {
                    self.pending_key = Some(key.into());
                    seed.deserialize(BorrowedStrDeserializer::<ParseError>::new(key))
                }
                std::borrow::Cow::Owned(key) => {
                    self.pending_key = Some(key.as_str().into());
                    seed.deserialize(StrDeserializer::<ParseError>::new(key.as_str()))
                }
            };
        }
        let key = self.lex.unquoted_key();
        if key.is_empty() {
            return Err(ParseError::ExpectedKey(start));
        }
        self.pending_key = Some(key.into());
        seed.deserialize(BorrowedStrDeserializer::<ParseError>::new(key))
    }

    /// Run `f` with bareword recovery enabled (value position).
    fn in_value_position<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = mem::replace(&mut self.allow_bareword, true);
        let result = f(self);
        self.allow_bareword = prev;
        result
    }
}

impl<'de> de::Deserializer<'de> for &mut Deserializer<'de> {
    type Error = ParseError;

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
        identifier ignored_any
    }

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ParseError> {
        match self.peek_some()? {
            b'{' => {
                self.descend()?;
                self.lex.bump();
                let out = visitor.visit_map(Separated {
                    de: self,
                    first: true,
                    index: 0,
                });
                self.depth -= 1;
                out
            }
            b'[' => {
                self.descend()?;
                self.lex.bump();
                let out = visitor.visit_seq(Separated {
                    de: self,
                    first: true,
                    index: 0,
                });
                self.depth -= 1;
                out
            }
            quote @ (b'"' | b'\'') => match self.lex.string(quote)? {
                std::borrow::Cow::Borrowed(text) => visitor.visit_borrowed_str(text),
                std::borrow::Cow::Owned(text) => visitor.visit_str(text.as_str()),
            },
            // JS-only NaN / Infinity are deliberately not accepted: JSON
            // cannot represent non-finite numbers.
            b'-' | b'+' | b'.' | b'0'..=b'9' => match self.lex.number()? {
                Num::U64(v) => visitor.visit_u64(v),
                Num::I64(v) => visitor.visit_i64(v),
                Num::F64(v) => visitor.visit_f64(v),
            },
            _ => match self.lex.match_keyword() {
                Some(Atom::Bool(b)) => visitor.visit_bool(b),
                Some(Atom::Null) => visitor.visit_unit(),
                None if self.allow_bareword => visitor.visit_borrowed_str(self.lex.bareword()?),
                None => Err(ParseError::UnexpectedToken(self.lex.pos())),
            },
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ParseError> {
        self.peek_some()?;
        if self.lex.eat_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    /// A newtype struct is transparent over the tolerant grammar.
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ParseError> {
        visitor.visit_newtype_struct(self)
    }

    /// Enums as `serde_json` encodes them: a bare string is a unit
    /// variant, `{"Variant": value}` carries data.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ParseError> {
        match self.peek_some()? {
            b'{' => {
                self.descend()?;
                self.lex.bump();
                self.lex.ws();
                let value = visitor.visit_enum(VariantAccess { de: self })?;
                self.depth -= 1;
                self.lex.ws();
                if self.lex.peek() == Some(b'}') {
                    self.lex.bump();
                    Ok(value)
                } else {
                    Err(ParseError::ExpectedCommaOrBrace(self.lex.pos()))
                }
            }
            b'"' | b'\'' => visitor.visit_enum(UnitVariantAccess { de: self }),
            _ => Err(ParseError::UnexpectedToken(self.lex.pos())),
        }
    }
}

/// Object/array walker with the tolerant comma rules: leading, doubled,
/// and trailing commas are skipped and recorded; a missing comma between
/// entries still fails.
struct Separated<'a, 'de> {
    de: &'a mut Deserializer<'de>,
    first: bool,
    index: usize,
}

impl Separated<'_, '_> {
    /// Advance past separators to the next member.
    ///
    /// Returns `true` when a member follows, `false` when `close` ended
    /// the container. `close` selects the closer byte (`}`/`]`) whose
    /// absence at end of input produces the matching unterminated error.
    fn member(&mut self, close: u8) -> Result<bool, ParseError> {
        self.de.lex.ws();
        let mut saw_comma = false;
        loop {
            match self.de.lex.peek() {
                None => {
                    return Err(if close == b'}' {
                        ParseError::UnterminatedObject
                    } else {
                        ParseError::UnterminatedArray
                    });
                }
                Some(b) if b == close => {
                    self.de.lex.bump();
                    return Ok(false);
                }
                Some(b',') => {
                    let comma = self.de.lex.pos();
                    self.de.lex.bump();
                    self.de.lex.ws();
                    let trailing = self.de.lex.peek() == Some(close);
                    if trailing || self.first || saw_comma {
                        self.de.lex.record_comma(comma, trailing);
                    }
                    saw_comma = true;
                }
                Some(_) => break,
            }
        }
        if !self.first && !saw_comma {
            return Err(if close == b'}' {
                ParseError::ExpectedCommaOrBrace(self.de.lex.pos())
            } else {
                ParseError::ExpectedCommaOrBracket(self.de.lex.pos())
            });
        }
        self.first = false;
        Ok(true)
    }
}

impl<'de> de::MapAccess<'de> for Separated<'_, 'de> {
    type Error = ParseError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, ParseError> {
        if !self.member(b'}')? {
            return Ok(None);
        }
        self.de.map_key_seed(seed).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, ParseError> {
        self.de.lex.ws();
        if self.de.lex.peek() == Some(b':') {
            self.de.lex.bump();
        } else {
            return Err(ParseError::ExpectedColon(self.de.lex.pos()));
        }
        self.de.lex.ws();
        if self.de.lex.at_end() {
            return Err(ParseError::ExpectedValue(self.de.lex.pos()));
        }
        let key = self.de.pending_key.take().unwrap_or_default();
        self.de.lex.push_path(Segment::Key(key));
        let result = self.de.in_value_position(|de| seed.deserialize(&mut *de));
        self.de.lex.pop_path();
        result
    }
}

impl<'de> de::SeqAccess<'de> for Separated<'_, 'de> {
    type Error = ParseError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, ParseError> {
        if !self.member(b']')? {
            return Ok(None);
        }
        let index = self.index;
        self.index += 1;
        self.de.lex.push_path(Segment::Index(index));
        let result = self
            .de
            .in_value_position(|de| seed.deserialize(&mut *de))
            .map(Some);
        self.de.lex.pop_path();
        result
    }
}

/// `{"Variant": value}` enum form.
struct VariantAccess<'a, 'de> {
    de: &'a mut Deserializer<'de>,
}

impl<'de> de::EnumAccess<'de> for VariantAccess<'_, 'de> {
    type Error = ParseError;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self), ParseError> {
        let variant = self.de.map_key_seed(seed)?;
        self.de.lex.ws();
        if self.de.lex.peek() == Some(b':') {
            self.de.lex.bump();
            Ok((variant, self))
        } else {
            Err(ParseError::ExpectedColon(self.de.lex.pos()))
        }
    }
}

impl<'de> de::VariantAccess<'de> for VariantAccess<'_, 'de> {
    type Error = ParseError;

    fn unit_variant(self) -> Result<(), ParseError> {
        serde::Deserialize::deserialize(&mut *self.de)
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, ParseError> {
        self.de.in_value_position(|de| seed.deserialize(&mut *de))
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ParseError> {
        de::Deserializer::deserialize_seq(&mut *self.de, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ParseError> {
        de::Deserializer::deserialize_map(&mut *self.de, visitor)
    }
}

/// Bare-string enum form (`"Variant"`).
struct UnitVariantAccess<'a, 'de> {
    de: &'a mut Deserializer<'de>,
}

impl<'de> de::EnumAccess<'de> for UnitVariantAccess<'_, 'de> {
    type Error = ParseError;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self), ParseError> {
        let variant = seed.deserialize(&mut *self.de)?;
        Ok((variant, self))
    }
}

impl<'de> de::VariantAccess<'de> for UnitVariantAccess<'_, 'de> {
    type Error = ParseError;

    fn unit_variant(self) -> Result<(), ParseError> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        _seed: T,
    ) -> Result<T::Value, ParseError> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"newtype variant",
        ))
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value, ParseError> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"tuple variant",
        ))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, ParseError> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"struct variant",
        ))
    }
}
