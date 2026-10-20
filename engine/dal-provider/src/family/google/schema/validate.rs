//! Structural validation of a projected schema: oh-my-pi `meta-validator.ts`
//! `isValidJsonSchema`, the check that sends a malformed Cloud Code Assist
//! schema to the permissive fallback instead of the wire.
//!
//! Only keywords that survive the Cloud Code Assist projection are checked.
//! The projection strips every other keyword the reference validator knows
//! (`pattern`, `minimum`, `additionalItems`, and the rest) before it validates,
//! so those branches could never fire.

use super::equality::json_equal;
use super::tree::{Fields, MAX_DEPTH, Node};

/// The JSON Schema type names.
const TYPE_NAMES: [&str; 7] = [
    "string", "number", "integer", "boolean", "object", "array", "null",
];

/// Whether `schema` is a structurally valid JSON Schema.
pub(super) fn is_valid(schema: &Node) -> bool {
    schema_ok(schema, 0)
}

/// Whether one node is a boolean schema or a valid schema object.
fn schema_ok(node: &Node, depth: usize) -> bool {
    match node {
        Node::Bool(_) => true,
        Node::Object(_) if depth > MAX_DEPTH => false,
        Node::Object(fields) => fields_ok(fields, depth),
        _ => false,
    }
}

/// Whether every entry of an array value is a valid schema.
fn schema_list_ok(value: &Node, depth: usize) -> bool {
    value
        .array()
        .is_some_and(|items| items.iter().all(|item| schema_ok(item, depth + 1)))
}

/// Whether every member of an object value is a valid schema.
fn schema_map_ok(value: &Node, depth: usize) -> bool {
    value
        .object()
        .is_some_and(|map| map.iter().all(|(_, member)| schema_ok(member, depth + 1)))
}

/// Whether a `type` value is one type name or a non-empty list of distinct
/// ones.
fn type_ok(value: &Node) -> bool {
    if let Some(name) = value.text() {
        return TYPE_NAMES.contains(&name);
    }
    let Some(names) = value.array() else {
        return false;
    };
    !names.is_empty()
        && names.iter().enumerate().all(|(index, entry)| {
            entry.text().is_some_and(|name| TYPE_NAMES.contains(&name))
                && !names[..index].iter().any(|seen| json_equal(seen, entry))
        })
}

/// Whether a `required` value lists distinct strings.
fn required_ok(value: &Node) -> bool {
    let Some(names) = value.array() else {
        return false;
    };
    names.iter().enumerate().all(|(index, entry)| {
        entry.text().is_some() && !names[..index].iter().any(|seen| json_equal(seen, entry))
    })
}

/// Whether an `enum` value is a non-empty list of distinct values.
fn enum_ok(value: &Node) -> bool {
    let Some(members) = value.array() else {
        return false;
    };
    !members.is_empty()
        && members
            .iter()
            .enumerate()
            .all(|(index, entry)| !members[..index].iter().any(|seen| json_equal(seen, entry)))
}

/// Whether a value is a non-negative integer.
fn count_ok(value: &Node) -> bool {
    value
        .number()
        .is_some_and(|number| number >= 0.0 && number.fract() == 0.0)
}

/// Whether every present keyword of one schema object is well formed.
fn fields_ok(fields: &Fields, depth: usize) -> bool {
    let present = |key: &str| fields.raw(key);
    if present("type").is_some_and(|value| !type_ok(value)) {
        return false;
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if present(key).is_some_and(|value| !schema_list_ok(value, depth)) {
            return false;
        }
    }
    for key in ["not", "if", "then", "else"] {
        if present(key).is_some_and(|value| !schema_ok(value, depth + 1)) {
            return false;
        }
    }
    if present("properties").is_some_and(|value| !schema_map_ok(value, depth)) {
        return false;
    }
    if present("required").is_some_and(|value| !required_ok(value)) {
        return false;
    }
    // Tuple `items` belongs to the older draft the upgrade already rewrote.
    if present("items").is_some_and(|value| !schema_ok(value, depth + 1)) {
        return false;
    }
    if present("enum").is_some_and(|value| !enum_ok(value)) {
        return false;
    }
    ["minProperties", "maxProperties"]
        .into_iter()
        .all(|key| present(key).is_none_or(count_ok))
}
