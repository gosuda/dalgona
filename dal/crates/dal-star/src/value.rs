//! Strict data codec between host JSON, the script `Value` transport, and
//! Starlark heap values (spec §R05).
//!
//! Host payloads travel as JSON text, decode into the transport [`Value`]
//! through a serde visitor over `sonic_rs::Deserializer`, cross the heap via
//! [`into_starlark`] as immutable [`Record`]/[`Array`] values, and leave
//! through [`from_starlark`] followed by [`to_json`]. Rejections are exactly
//! those of §R05: duplicate object keys, integers beyond the signed 53-bit
//! range, non-finite numbers, containers nesting past [`MAX_VALUE_DEPTH`],
//! and every type the transport cannot name.

use std::collections::BTreeSet;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use starlark::values::{
    Heap, Value as StarlarkValue, ValueLike, dict::DictRef, list::ListRef, tuple::TupleRef,
};

use allocative::Allocative;

use crate::record::{Array, Record, is_missing, revision_of};

/// Maximum container nesting accepted on either direction of the codec.
pub(crate) const MAX_VALUE_DEPTH: usize = 64;
/// The largest payload in bytes `decode` accepts.
const MAX_DATA: usize = 1 << 20;
/// Inclusive bound on every integer crossing the boundary.
const INT_LIMIT: i64 = (1 << 53) - 1;

/// The transport form for every value crossing the script boundary.
///
/// Integer values keep machine precision (signed 53-bit), objects preserve
/// document order with unique keys, and nothing host-side appears here: no
/// functions, records-as-host-types, or provider handles cross as data.
#[derive(Clone, Debug, PartialEq, Allocative)]
pub(crate) enum Value {
    /// `NoneType`.
    Null,
    /// `True`/`False`.
    Bool(bool),
    /// Integer within ±(2^53 − 1).
    Int(i64),
    /// Finite, non-NaN double.
    Num(f64),
    /// UTF-8 text.
    Str(Box<str>),
    /// Ordered elements.
    List(Box<[Value]>),
    /// Insertion-ordered object; keys are unique (enforced on decode).
    Object(Box<[(Box<str>, Value)]>),
}

/// One strict codec violation, phrased for script-facing errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CodecError {
    /// Input exceeded a hard bound (size, depth, integer range, duplicates).
    #[error("{0}")]
    Invalid(String),
    /// The source was not well-formed JSON.
    #[error("invalid json: {0}")]
    Json(String),
    /// A Starlark value outside the transport set reached the codec.
    #[error("cannot return {0} across the script boundary")]
    UnsupportedType(String),
}

impl Value {
    /// Builds an object from named fields in order; the names must be
    /// distinct.
    pub(crate) fn object(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Self {
        Self::Object(
            fields
                .into_iter()
                .map(|(key, item)| (Box::from(key), item))
                .collect(),
        )
    }

    /// Decodes JSON text into the transport form under §R05 bounds.
    pub(crate) fn decode(text: &str) -> Result<Self, CodecError> {
        if text.len() > MAX_DATA {
            return Err(CodecError::Invalid(format!(
                "payload exceeds {MAX_DATA} bytes"
            )));
        }
        let value: Value = sonic_rs::from_str(text).map_err(|e| CodecError::Json(e.to_string()))?;
        value.check_depth(0)?;
        Ok(value)
    }

    /// Rejects trees nesting deeper than [`MAX_VALUE_DEPTH`].
    ///
    /// Input beyond sonic's own 255-level cap already fails at parse, so a
    /// post-decode walk enforces the smaller v1 bound exactly.
    fn check_depth(&self, depth: usize) -> Result<(), CodecError> {
        if depth > MAX_VALUE_DEPTH {
            return Err(CodecError::Invalid(format!(
                "payload nests deeper than {MAX_VALUE_DEPTH}"
            )));
        }
        let children: &[Value] = match self {
            Self::List(items) => items,
            Self::Object(fields) => {
                for (_, item) in fields {
                    item.check_depth(depth + 1)?;
                }
                return Ok(());
            }
            _ => return Ok(()),
        };
        for item in children {
            item.check_depth(depth + 1)?;
        }
        Ok(())
    }

    /// Enforces the signed 53-bit integer bound.
    fn integer(number: i64) -> Result<Self, CodecError> {
        if !(-INT_LIMIT..=INT_LIMIT).contains(&number) {
            return Err(CodecError::Invalid(
                "integer exceeds signed 53-bit range".into(),
            ));
        }
        Ok(Self::Int(number))
    }

    /// Serializes the transport form to canonical JSON text.
    ///
    /// Non-finite numbers cannot occur: `decode` rejects them on entry and
    /// `from_starlark` rejects them on exit, so this writer never emits
    /// `NaN`/`Infinity`.
    pub(crate) fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            Self::Int(number) => out.push_str(&number.to_string()),
            Self::Num(number) => {
                out.push_str(&sonic_rs::to_string(number).unwrap_or_default());
            }
            Self::Str(text) => out.push_str(&sonic_rs::to_string(text).unwrap_or_default()),
            Self::List(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            Self::Object(fields) => {
                out.push('{');
                for (index, (key, item)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&sonic_rs::to_string(key).unwrap_or_default());
                    out.push(':');
                    item.write_json(out);
                }
                out.push('}');
            }
        }
    }

    /// Allocates the transport form onto `heap` as immutable records/arrays.
    pub(crate) fn into_starlark(self, heap: Heap<'_>) -> Result<StarlarkValue<'_>, CodecError> {
        self.alloc(heap, 0)
    }

    fn alloc(self, heap: Heap<'_>, depth: usize) -> Result<StarlarkValue<'_>, CodecError> {
        if depth > MAX_VALUE_DEPTH {
            return Err(CodecError::Invalid(format!(
                "value nests deeper than {MAX_VALUE_DEPTH}"
            )));
        }
        match self {
            Self::Null => Ok(StarlarkValue::new_none()),
            Self::Bool(flag) => Ok(heap.alloc(flag)),
            Self::Int(number) => Ok(heap.alloc(number)),
            Self::Num(number) => Ok(heap.alloc(number)),
            Self::Str(text) => Ok(heap.alloc(text.as_ref())),
            Self::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items.into_vec() {
                    values.push(item.alloc(heap, depth + 1)?);
                }
                Ok(Array::alloc(heap, values))
            }
            Self::Object(fields) => {
                let mut pairs = Vec::with_capacity(fields.len());
                for (key, item) in fields.into_vec() {
                    pairs.push((key.into_string(), item.alloc(heap, depth + 1)?));
                }
                Record::alloc(heap, pairs).map_err(|e| CodecError::Invalid(e.to_string()))
            }
        }
    }

    /// Projects a Starlark value back into the transport form.
    ///
    /// `NoneType`, `bool`, `int`, `float`, `string`, `list`, `tuple`,
    /// string-keyed `dict`, `Record`, and `Array` survive; every other type —
    /// `Missing`, `Revision`, `Context`, `Task`, `Scope`, functions,
    /// host-capable handles — is rejected by name (§R05 reject list).
    pub(crate) fn from_starlark(value: StarlarkValue<'_>) -> Result<Self, CodecError> {
        Self::encode(value, 0)
    }

    fn encode(value: StarlarkValue<'_>, depth: usize) -> Result<Self, CodecError> {
        if depth > MAX_VALUE_DEPTH {
            return Err(CodecError::Invalid(format!(
                "result nests deeper than {MAX_VALUE_DEPTH}"
            )));
        }
        if value.is_none() {
            return Ok(Self::Null);
        }
        if let Some(flag) = value.unpack_bool() {
            return Ok(Self::Bool(flag));
        }
        if is_missing(value) {
            return Err(CodecError::UnsupportedType("missing".into()));
        }
        if revision_of(value).is_some() {
            return Err(CodecError::UnsupportedType("revision".into()));
        }
        if value.get_type() == "int" {
            if let Some(small) = value.unpack_i32() {
                return Ok(Self::Int(i64::from(small)));
            }
            let text = value.to_str();
            let number = text
                .parse::<i64>()
                .map_err(|_| CodecError::Invalid("integer out of i64 range".into()))?;
            return Self::integer(number);
        }
        if let Some(float) =
            ValueLike::downcast_ref::<starlark::values::float::StarlarkFloat>(value)
        {
            let number = float.0;
            if !number.is_finite() {
                return Err(CodecError::Invalid(
                    "non-finite number cannot cross the boundary".into(),
                ));
            }
            return Ok(Self::Num(number));
        }
        if let Some(text) = value.unpack_str() {
            return Ok(Self::Str(text.into()));
        }
        if let Some(record) = Record::from_value(value) {
            let mut fields = Vec::new();
            for (key, item) in record.iter() {
                let name = key.unpack_str().ok_or_else(|| {
                    CodecError::UnsupportedType("record with non-string key".into())
                })?;
                fields.push((name.into(), Self::encode(item, depth + 1)?));
            }
            return Ok(Self::Object(fields.into_boxed_slice()));
        }
        if let Some(array) = Array::from_value(value) {
            let mut items = Vec::with_capacity(array.items().len());
            for item in array.items() {
                items.push(Self::encode(*item, depth + 1)?);
            }
            return Ok(Self::List(items.into_boxed_slice()));
        }
        if let Some(list) = ListRef::from_value(value) {
            let mut items = Vec::with_capacity(list.len());
            for item in list.iter() {
                items.push(Self::encode(item, depth + 1)?);
            }
            return Ok(Self::List(items.into_boxed_slice()));
        }
        if let Some(tuple) = TupleRef::from_value(value) {
            let mut items = Vec::with_capacity(tuple.len());
            for item in tuple.iter() {
                items.push(Self::encode(item, depth + 1)?);
            }
            return Ok(Self::List(items.into_boxed_slice()));
        }
        if let Some(dict) = DictRef::from_value(value) {
            let mut fields = Vec::with_capacity(dict.len());
            for (key, item) in dict.iter() {
                let name = key.unpack_str().ok_or_else(|| {
                    CodecError::UnsupportedType(format!("dict key of type {}", key.get_type()))
                })?;
                fields.push((name.into(), Self::encode(item, depth + 1)?));
            }
            return Ok(Self::Object(fields.into_boxed_slice()));
        }
        Err(CodecError::UnsupportedType(value.get_type().to_owned()))
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, flag: bool) -> Result<Value, E> {
        Ok(Value::Bool(flag))
    }

    fn visit_i64<E>(self, number: i64) -> Result<Value, E>
    where
        E: de::Error,
    {
        Value::integer(number).map_err(|e| E::custom(e.to_string()))
    }

    fn visit_u64<E>(self, number: u64) -> Result<Value, E>
    where
        E: de::Error,
    {
        let signed =
            i64::try_from(number).map_err(|_| E::custom("integer exceeds signed 53-bit range"))?;
        Value::integer(signed).map_err(|e| E::custom(e.to_string()))
    }

    fn visit_f64<E>(self, number: f64) -> Result<Value, E>
    where
        E: de::Error,
    {
        if !number.is_finite() {
            return Err(E::custom("non-finite number is not valid JSON"));
        }
        Ok(Value::Num(number))
    }

    fn visit_str<E>(self, text: &str) -> Result<Value, E> {
        Ok(Value::Str(text.into()))
    }

    fn visit_string<E>(self, text: String) -> Result<Value, E> {
        Ok(Value::Str(text.into_boxed_str()))
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::List(items.into_boxed_slice()))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = BTreeSet::new();
        let mut fields = Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(format!("duplicate key `{key}`")));
            }
            fields.push((key.into_boxed_str(), map.next_value::<Value>()?));
        }
        Ok(Value::Object(fields.into_boxed_slice()))
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor)
    }
}
