//! The ordered JSON tree the Google schema projection edits.
//!
//! JavaScript object semantics are the contract here, because the projection
//! is a port of oh-my-pi's TypeScript: members keep insertion order, assigning
//! an existing key keeps its slot while a new key appends, and a duplicate key
//! in the source text keeps the last value at the first position, exactly
//! what `JSON.parse` yields. No hash map is built for members; lookups scan
//! the ordered pairs, which is cheap for tool schemas.

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use sonic_rs::{JsonContainerTrait, JsonNumberTrait, JsonValueTrait, Number, Value};

/// The deepest nesting any pass of the projection descends into.
///
/// Source text deeper than this is passed through unprojected; a subtree the
/// passes reach beyond it collapses to `{}` (or `[]`), the same stand-in
/// oh-my-pi uses for a cyclic reference.
pub(super) const MAX_DEPTH: usize = 128;

/// One JSON value of a schema tree.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Node {
    /// A JavaScript `undefined` stored under a key.
    ///
    /// JSON text never carries one. It exists because oh-my-pi can assign
    /// `result.type = undefined`: the key then still counts for `in` and
    /// `Object.hasOwn`, reads back as absent, and is omitted on serialization.
    Undefined,
    /// JSON `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number, kept exactly as parsed.
    Number(Number),
    /// A string.
    Text(String),
    /// An array.
    Array(Vec<Node>),
    /// An object whose members keep their order.
    Object(Fields),
}

/// The members of one object, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Fields(Vec<(String, Node)>);

impl Fields {
    /// An object without members.
    pub(super) const fn new() -> Self {
        Self(Vec::new())
    }

    /// Builds an object from parsed pairs, collapsing a duplicate key to its
    /// last value at its first position.
    fn from_entries(entries: Vec<(String, Node)>) -> Self {
        if !has_duplicate_key(&entries) {
            return Self(entries);
        }
        let mut fields = Self(Vec::with_capacity(entries.len()));
        for (key, node) in entries {
            fields.set(&key, node);
        }
        fields
    }

    /// The value under `key`; an absent key and an `undefined` value both
    /// read as `None`, like a JavaScript property read.
    pub(super) fn get(&self, key: &str) -> Option<&Node> {
        self.raw(key).filter(|node| !matches!(node, Node::Undefined))
    }

    /// The slot under `key` including an `undefined` value, like the `in`
    /// operator sees it.
    pub(super) fn raw(&self, key: &str) -> Option<&Node> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, node)| node)
    }

    /// `Object.hasOwn`: whether `key` is a member, whatever its value.
    pub(super) fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(name, _)| name == key)
    }

    /// The object under `key`, when the value is one.
    pub(super) fn object(&self, key: &str) -> Option<&Self> {
        self.get(key).and_then(Node::object)
    }

    /// The array under `key`, when the value is one.
    pub(super) fn array(&self, key: &str) -> Option<&[Node]> {
        self.get(key).and_then(Node::array)
    }

    /// The string under `key`, when the value is one.
    pub(super) fn text(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Node::text)
    }

    /// Whether the value under `key` is exactly the string `expected`.
    pub(super) fn is_text(&self, key: &str, expected: &str) -> bool {
        self.text(key) == Some(expected)
    }

    /// The object under `key`, created (or replacing a non-object value) when
    /// missing: oh-my-pi `getObjectMap`.
    pub(super) fn object_or_insert(&mut self, key: &str) -> Option<&mut Self> {
        if self.object(key).is_none() {
            self.set(key, Node::empty_object());
        }
        match self.0.iter_mut().find(|(name, _)| name == key) {
            Some((_, Node::Object(fields))) => Some(fields),
            _ => None,
        }
    }

    /// JavaScript `obj[key] = node`: replaces in place or appends.
    pub(super) fn set(&mut self, key: &str, node: Node) {
        match self.0.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = node,
            None => self.0.push((key.to_owned(), node)),
        }
    }

    /// Appends a member the caller knows is not present yet.
    pub(super) fn push(&mut self, key: &str, node: Node) {
        self.0.push((key.to_owned(), node));
    }

    /// JavaScript `delete obj[key]`.
    pub(super) fn remove(&mut self, key: &str) {
        self.0.retain(|(name, _)| name != key);
    }

    /// A copy without `key`: oh-my-pi `copySchemaWithout`.
    pub(super) fn without(&self, key: &str) -> Self {
        Self(
            self.0
                .iter()
                .filter(|(name, _)| name != key)
                .cloned()
                .collect(),
        )
    }

    /// The members in order, `undefined` values included.
    pub(super) fn iter(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.0.iter().map(|(name, node)| (name.as_str(), node))
    }

    /// The number of members.
    pub(super) const fn len(&self) -> usize {
        self.0.len()
    }
}

/// Whether two parsed pairs share a key. Sorting keeps this `O(n log n)`
/// without a hash map.
fn has_duplicate_key(entries: &[(String, Node)]) -> bool {
    if entries.len() < 2 {
        return false;
    }
    let mut keys: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    keys.sort_unstable();
    keys.windows(2).any(|pair| pair.first() == pair.get(1))
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Undefined | Self::Null => serializer.serialize_unit(),
            Self::Bool(flag) => serializer.serialize_bool(*flag),
            Self::Number(number) => number.serialize(serializer),
            Self::Text(text) => serializer.serialize_str(text),
            Self::Array(items) => serializer.collect_seq(items),
            Self::Object(fields) => {
                let mut map = serializer.serialize_map(None)?;
                for (name, node) in fields.iter() {
                    // `JSON.stringify` omits an `undefined` member.
                    if !matches!(node, Self::Undefined) {
                        map.serialize_entry(name, node)?;
                    }
                }
                map.end()
            }
        }
    }
}

impl Node {
    /// Copies a parsed value; `None` when it nests deeper than [`MAX_DEPTH`].
    pub(super) fn from_value(value: &Value, depth: usize) -> Option<Self> {
        if depth > MAX_DEPTH {
            return None;
        }
        if value.is_null() {
            return Some(Self::Null);
        }
        if let Some(flag) = value.as_bool() {
            return Some(Self::Bool(flag));
        }
        if let Some(number) = value.as_number() {
            return Some(Self::Number(number));
        }
        if let Some(text) = value.as_str() {
            return Some(Self::Text(text.to_owned()));
        }
        if let Some(items) = value.as_array() {
            let nodes = items
                .iter()
                .map(|item| Self::from_value(item, depth + 1))
                .collect::<Option<Vec<_>>>()?;
            return Some(Self::Array(nodes));
        }
        let map = value.as_object()?;
        let entries = map
            .iter()
            .map(|(name, member)| {
                Self::from_value(member, depth + 1).map(|node| (name.to_owned(), node))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self::Object(Fields::from_entries(entries)))
    }

    /// An object without members.
    pub(super) const fn empty_object() -> Self {
        Self::Object(Fields::new())
    }

    /// A string node.
    pub(super) fn string(text: &str) -> Self {
        Self::Text(text.to_owned())
    }

    /// `isJsonObject`: the members, when this is a non-array object.
    pub(super) const fn object(&self) -> Option<&Fields> {
        match self {
            Self::Object(fields) => Some(fields),
            _ => None,
        }
    }

    /// `Array.isArray`: the items, when this is an array.
    pub(super) fn array(&self) -> Option<&[Node]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The string, when this is one (`typeof value === "string"`).
    pub(super) fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The number as the `f64` JavaScript would hold, when this is a number.
    pub(super) fn number(&self) -> Option<f64> {
        match self {
            Self::Number(number) => number.as_f64(),
            _ => None,
        }
    }

    /// JavaScript truthiness: `undefined`, `null`, `false`, `0`, and `""` are
    /// falsy; every array and object is truthy.
    pub(super) fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(flag) => *flag,
            Self::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
            Self::Text(text) => !text.is_empty(),
            Self::Array(_) | Self::Object(_) => true,
        }
    }

    /// `inferJsonSchemaTypeFromValue`: the JSON Schema type name of a value.
    pub(super) const fn json_type(&self) -> Option<&'static str> {
        match self {
            Self::Undefined => None,
            Self::Null => Some("null"),
            Self::Bool(_) => Some("boolean"),
            Self::Number(_) => Some("number"),
            Self::Text(_) => Some("string"),
            Self::Array(_) => Some("array"),
            Self::Object(_) => Some("object"),
        }
    }
}

/// Truthiness of a possibly absent value: an absent one is falsy.
pub(super) fn truthy(node: Option<&Node>) -> bool {
    node.is_some_and(Node::truthy)
}

/// Whether an array holds the string `needle` (`Array.prototype.includes`).
pub(super) fn includes_text(items: &[Node], needle: &str) -> bool {
    items.iter().any(|item| item.text() == Some(needle))
}
