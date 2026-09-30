#![expect(unsafe_code, reason = "starlark value derives")]

//! Immutable host values that cross into Starlark as data (spec §R05, §P05).
//!
//! [`Record`] is the object shape for decoded args, config, and event
//! payloads: attribute access works for identifier keys and `rec[key]` works
//! for any string key, with no mutators. [`Array`] is the matching immutable
//! sequence. [`Missing`] is the omission sentinel `dal.MISSING`; [`Revision`]
//! is the opaque state proof token minted by `state.read`.
//!
//! These types deliberately do not implement `equals` beyond the rules below:
//! `Missing` compares equal only to `Missing`, and `Revision` compares by
//! serial. `Record` and `Array` compare by content.

use std::{cmp::Ordering, fmt, hash::Hasher};

use allocative::Allocative;
use starlark::{
    coerce::Coerce,
    collections::{SmallMap, StarlarkHasher},
    starlark_complex_value, starlark_simple_value,
    values::{
        Freeze, Heap, NoSerialize, ProvidesStaticType, StarlarkValue, Trace, Value,
        ValueLifetimeless, ValueLike,
    },
};

/// An immutable string-keyed record (`RecordGen` is the allocation shape).
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct RecordGen<V: ValueLifetimeless> {
    fields: SmallMap<V, V>,
}

starlark_complex_value!(pub(crate) Record);

impl<'v> Record<'v> {
    /// Builds a record from `fields`; keys must already be string values.
    ///
    /// The constructor is the only place fields enter; the record then has no
    /// mutators, so scripts cannot reach the underlying storage.
    pub(crate) fn alloc(
        heap: Heap<'v>,
        fields: Vec<(String, Value<'v>)>,
    ) -> starlark::Result<Value<'v>> {
        let mut interned = SmallMap::with_capacity(fields.len());
        for (key, value) in fields {
            let key_value = heap.alloc_str_intern(&key).to_value();
            let hashed = key_value.get_hashed()?;
            interned.insert_hashed(hashed, value);
        }
        Ok(heap.alloc_complex(RecordGen { fields: interned }))
    }

    /// Host-side field iteration in document order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Value<'v>, Value<'v>)> + '_ {
        self.fields.iter().map(|(key, value)| (*key, *value))
    }
}

impl<V: ValueLifetimeless + fmt::Debug> fmt::Display for RecordGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Record{")?;
        for (index, (key, value)) in self.fields.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{key:?}: {value:?}")?;
        }
        f.write_str("}")
    }
}

#[starlark::values::starlark_value(type = "record")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for RecordGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn get_attr(&self, attribute: &str, _heap: Heap<'v>) -> Option<Value<'v>> {
        self.fields
            .iter()
            .find(|(key, _)| key.to_value().unpack_str() == Some(attribute))
            .map(|(_, slot)| slot.to_value())
    }

    fn has_attr(&self, attribute: &str, heap: Heap<'v>) -> bool {
        self.get_attr(attribute, heap).is_some()
    }

    fn dir_attr(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter_map(|(key, _)| key.to_value().unpack_str().map(str::to_owned))
            .collect()
    }

    fn at(&self, index: Value<'v>, heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        let key = index.unpack_str().ok_or_else(|| {
            starlark::Error::new_other(dal_error(format!(
                "record index must be a string, got {}",
                index.get_type()
            )))
        })?;
        self.get_attr(key, heap).ok_or_else(|| {
            starlark::Error::new_other(dal_error(format!("no field `{key}` in record")))
        })
    }

    fn length(&self) -> starlark::Result<i32> {
        Ok(i32::try_from(self.fields.len()).unwrap_or(i32::MAX))
    }

    fn is_in(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(other.unpack_str().is_some_and(|key| {
            self.fields
                .iter()
                .any(|(name, _)| name.to_value().unpack_str() == Some(key))
        }))
    }

    fn iterate_collect(&self, _heap: Heap<'v>) -> starlark::Result<Vec<Value<'v>>> {
        Ok(self.fields.iter().map(|(key, _)| key.to_value()).collect())
    }

    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        let Some(record) = Record::from_value(other) else {
            return Ok(false);
        };
        if record.fields.len() != self.fields.len() {
            return Ok(false);
        }
        for (key, theirs) in &record.fields {
            let Some(name) = key.to_value().unpack_str() else {
                return Ok(false);
            };
            let Some(mine) = self
                .fields
                .iter()
                .find(|(name_key, _)| name_key.to_value().unpack_str() == Some(name))
                .map(|(_, slot)| *slot)
            else {
                return Ok(false);
            };
            if !mine.to_value().equals(theirs.to_value())? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// An immutable ordered sequence (`ArrayGen` is the allocation shape).
#[repr(C)]
#[derive(Debug, Trace, Coerce, Freeze, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct ArrayGen<V: ValueLifetimeless> {
    items: Vec<V>,
}

starlark_complex_value!(pub(crate) Array);

impl<'v> Array<'v> {
    /// Allocates an immutable array from `items`.
    pub(crate) fn alloc(heap: Heap<'v>, items: Vec<Value<'v>>) -> Value<'v> {
        heap.alloc_complex(ArrayGen { items })
    }

    /// The elements for host-side iteration.
    pub(crate) fn items(&self) -> &[Value<'v>] {
        &self.items
    }
}

impl<V: ValueLifetimeless + fmt::Debug> fmt::Display for ArrayGen<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Array[")?;
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{item:?}")?;
        }
        f.write_str("]")
    }
}

#[starlark::values::starlark_value(type = "array")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ArrayGen<V>
where
    Self: ProvidesStaticType<'v>,
{
    fn at(&self, index: Value<'v>, _heap: Heap<'v>) -> starlark::Result<Value<'v>> {
        let Some(position) = index.unpack_i32() else {
            return Err(starlark::Error::new_other(dal_error(format!(
                "array index must be an int, got {}",
                index.get_type()
            ))));
        };
        let Ok(position) = usize::try_from(position) else {
            return Err(starlark::Error::new_other(dal_error(format!(
                "array index {position} out of bounds"
            ))));
        };
        self.items
            .get(position)
            .map(|item| item.to_value())
            .ok_or_else(|| {
                starlark::Error::new_other(dal_error(format!(
                    "array index {position} out of bounds"
                )))
            })
    }

    fn length(&self) -> starlark::Result<i32> {
        Ok(i32::try_from(self.items.len()).unwrap_or(i32::MAX))
    }

    fn iterate_collect(&self, _heap: Heap<'v>) -> starlark::Result<Vec<Value<'v>>> {
        Ok(self.items.iter().map(|item| item.to_value()).collect())
    }

    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        let Some(array) = Array::from_value(other) else {
            return Ok(false);
        };
        if array.items.len() != self.items.len() {
            return Ok(false);
        }
        for (mine, theirs) in self.items.iter().zip(array.items.iter()) {
            if !mine.to_value().equals(theirs.to_value())? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The `dal.MISSING` omission sentinel.
///
/// `MISSING` is not `None`, `false`, or an empty string; it names an absent
/// `optional` field. Scripts test it with `args.x == dal.MISSING`. Truthiness
/// is not meaningful for a missing value and the vendor `to_bool` hook cannot
/// report an error, so `MISSING` deliberately evaluates to `false`: the only
/// safe reading, since `if args.x:` then behaves as "present". `MISSING`
/// never survives the value codec or a state write.
#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct Missing;

starlark_simple_value!(Missing);

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MISSING")
    }
}

#[starlark::values::starlark_value(type = "missing")]
impl<'v> StarlarkValue<'v> for Missing {
    fn to_bool(&self) -> bool {
        false
    }

    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(is_missing(other))
    }
}

/// True when `value` is the `Missing` sentinel, frozen or not.
pub(crate) fn is_missing(value: Value<'_>) -> bool {
    if let Some(frozen) = value.unpack_frozen() {
        ValueLike::downcast_ref::<Missing>(frozen.to_value()).is_some()
    } else {
        ValueLike::downcast_ref::<Missing>(value).is_some()
    }
}

/// An opaque state-revision token minted by `state.read(key)` (R08).
///
/// A revision carries `serial`, the monotonically increasing write counter
/// for its cell, plus `generation`, the plugin generation that minted it.
/// Scripts can only compare revisions (`==`); state operations verify the
/// token belongs to the current generation before trusting it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ProvidesStaticType, NoSerialize, Allocative,
)]
pub(crate) struct Revision {
    /// The owning plugin generation.
    pub(crate) generation: u64,
    /// The cell write counter this token was minted at.
    pub(crate) serial: u64,
}

starlark_simple_value!(Revision);

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Revision(g{}, s{})", self.generation, self.serial)
    }
}

#[starlark::values::starlark_value(type = "revision")]
impl<'v> StarlarkValue<'v> for Revision {
    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(revision_of(other) == Some(self))
    }

    fn compare(&self, other: Value<'v>) -> starlark::Result<Ordering> {
        revision_of(other)
            .map(|revisions| revisions.cmp(self))
            .ok_or_else(|| {
                starlark::Error::new_other(dal_error(format!(
                    "cannot compare Revision with {}",
                    other.get_type()
                )))
            })
    }

    fn write_hash(&self, hasher: &mut StarlarkHasher) -> starlark::Result<()> {
        hasher.write_u64(self.generation);
        hasher.write_u64(self.serial);
        Ok(())
    }
}

/// Borrows the `Revision` inside `value`, accepting frozen handles.
pub(crate) fn revision_of(value: Value<'_>) -> Option<&Revision> {
    if let Some(frozen) = value.unpack_frozen() {
        ValueLike::downcast_ref::<Revision>(frozen.to_value())
    } else {
        ValueLike::downcast_ref::<Revision>(value)
    }
}

/// One vendor-facing error shell for record-layer failures.
#[derive(Debug)]
struct DalError(String);

impl fmt::Display for DalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DalError {}

fn dal_error(message: String) -> DalError {
    DalError(message)
}
