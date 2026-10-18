//! Draft upgrade: oh-my-pi `draft.ts` `upgradeJsonSchemaTo202012`.
//!
//! Legacy shapes become draft 2020-12 before the provider passes run:
//! draft-07 `definitions` become `$defs`, a `#/definitions/X` reference
//! becomes `#/$defs/X`, tuple `items` become `prefixItems`, `dependencies`
//! split into `dependentRequired` and `dependentSchemas`, and the `OpenAPI`
//! `nullable: true` marker widens `type` (or wraps the schema) to accept
//! `null`. A cheap pre-check lets a schema that needs none of this pass
//! through untouched.

use std::borrow::Cow;

use super::equality::{json_equal, push_unique};
use super::ref_pointer::{format_local_ref_pointer, parse_local_ref_pointer};
use super::tree::{Fields, MAX_DEPTH, Node, includes_text};

/// The draft 2020-12 meta-schema URI.
const JSON_SCHEMA_DRAFT_2020_12_URI: &str = "https://json-schema.org/draft/2020-12/schema";

/// The draft-07 `$schema` URIs that mark a schema for upgrade.
const DRAFT_07_SCHEMA_URIS: [&str; 4] = [
    "http://json-schema.org/draft-07/schema#",
    "https://json-schema.org/draft-07/schema#",
    "http://json-schema.org/draft-07/schema",
    "https://json-schema.org/draft-07/schema",
];

/// Keys whose value is JSON data, not a nested schema; the upgrade never
/// descends into them.
const NON_SCHEMA_VALUE_KEYS: [&str; 8] = [
    "const",
    "default",
    "enum",
    "example",
    "examples",
    "required",
    "dependentRequired",
    "type",
];

/// Upgrades legacy JSON Schema shapes to draft 2020-12; a schema that needs
/// no upgrade is returned as it is.
pub(super) fn upgrade(schema: &Node) -> Cow<'_, Node> {
    if needs_upgrade(schema, 0) {
        Cow::Owned(upgrade_node(schema, 0))
    } else {
        Cow::Borrowed(schema)
    }
}

/// Pre-check: whether any keyword or value would be rewritten.
fn needs_upgrade(value: &Node, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match value {
        Node::Array(items) => items.iter().any(|item| needs_upgrade(item, depth + 1)),
        Node::Object(fields) => fields
            .iter()
            .any(|(key, entry)| member_needs_upgrade(key, entry, depth)),
        _ => false,
    }
}

/// Pre-check of one object member.
fn member_needs_upgrade(key: &str, entry: &Node, depth: usize) -> bool {
    match key {
        "$schema" => entry
            .text()
            .is_some_and(|uri| DRAFT_07_SCHEMA_URIS.contains(&uri)),
        "definitions" | "dependencies" | "additionalItems" | "nullable" => true,
        "$ref" => entry
            .text()
            .is_some_and(|reference| reference.starts_with("#/definitions/")),
        "items" if entry.array().is_some() => true,
        "$defs" | "properties" | "patternProperties" | "dependentSchemas" => entry
            .object()
            .is_some_and(|map| map.iter().any(|(_, node)| needs_upgrade(node, depth + 2))),
        _ if NON_SCHEMA_VALUE_KEYS.contains(&key) => false,
        _ => needs_upgrade(entry, depth + 1),
    }
}

/// Rewrites one node and everything below it.
fn upgrade_node(value: &Node, depth: usize) -> Node {
    if depth > MAX_DEPTH {
        return value.clone();
    }
    match value {
        Node::Array(items) => Node::Array(
            items
                .iter()
                .map(|item| upgrade_node(item, depth + 1))
                .collect(),
        ),
        Node::Object(fields) => Node::Object(upgrade_object(fields, depth)),
        other => other.clone(),
    }
}

/// Rewrites one schema object.
fn upgrade_object(fields: &Fields, depth: usize) -> Fields {
    let mut result = Fields::new();
    for (key, entry) in fields.iter() {
        upgrade_member(&mut result, key, entry, depth);
    }
    upgrade_tuple_items(fields, &mut result, depth);
    convert_dependencies(fields, &mut result, depth);
    if matches!(fields.get("nullable"), Some(Node::Bool(true))) {
        return make_nullable(result);
    }
    result
}

/// Carries one member of a schema object into `result`.
fn upgrade_member(result: &mut Fields, key: &str, entry: &Node, depth: usize) {
    match key {
        "definitions" | "$defs" => {
            if let Node::Object(defs) = entry {
                merge_schema_map(result, "$defs", defs, depth);
            }
        }
        "properties" | "patternProperties" | "dependentSchemas" => {
            copy_schema_map(result, key, entry, depth);
        }
        "dependencies" | "additionalItems" | "nullable" => {}
        "$schema" => result.set("$schema", upgraded_schema_uri(entry)),
        "$ref" => match entry.text() {
            Some(reference) => result.set("$ref", Node::Text(convert_ref(reference))),
            None => result.set(key, upgrade_node(entry, depth + 1)),
        },
        "items" if entry.array().is_some() => {}
        _ if NON_SCHEMA_VALUE_KEYS.contains(&key) => result.set(key, entry.clone()),
        _ => result.set(key, upgrade_node(entry, depth + 1)),
    }
}

/// A draft-07 `$schema` URI becomes the 2020-12 one; any other value stays.
fn upgraded_schema_uri(entry: &Node) -> Node {
    match entry.text() {
        Some(uri) if DRAFT_07_SCHEMA_URIS.contains(&uri) => {
            Node::string(JSON_SCHEMA_DRAFT_2020_12_URI)
        }
        _ => entry.clone(),
    }
}

/// Rewrites `#/definitions/Foo` to `#/$defs/Foo`, decoding URI-fragment
/// spellings first. External references pass through.
fn convert_ref(value: &str) -> String {
    let Some(mut pointer) = parse_local_ref_pointer(value) else {
        return value.to_owned();
    };
    if pointer.first().map(String::as_str) != Some("definitions") {
        return value.to_owned();
    }
    if let Some(head) = pointer.first_mut() {
        "$defs".clone_into(head);
    }
    format_local_ref_pointer(&pointer)
}

/// Upgrades every entry of a schema map and merges the entries into the
/// object at `target[key]`.
fn merge_schema_map(target: &mut Fields, key: &str, value: &Fields, depth: usize) {
    let Some(map) = target.object_or_insert(key) else {
        return;
    };
    for (name, entry) in value.iter() {
        map.set(name, upgrade_node(entry, depth + 2));
    }
}

/// Copies a schema-map member with its entries upgraded; a non-object value
/// passes through.
fn copy_schema_map(target: &mut Fields, key: &str, value: &Node, depth: usize) {
    match value {
        Node::Object(map) => merge_schema_map(target, key, map, depth),
        other => target.set(key, other.clone()),
    }
}

/// Intersects two schemas: `true` or absent is the identity, `false` absorbs,
/// equal schemas collapse, anything else wraps in `allOf`.
fn combine_schemas(left: Option<&Node>, right: &Node) -> Node {
    let Some(left) = left else {
        return right.clone();
    };
    if matches!(left, Node::Bool(true)) {
        return right.clone();
    }
    if matches!(right, Node::Bool(true)) {
        return left.clone();
    }
    if matches!(left, Node::Bool(false)) || matches!(right, Node::Bool(false)) {
        return Node::Bool(false);
    }
    if json_equal(left, right) {
        return left.clone();
    }
    let mut wrapper = Fields::new();
    wrapper.push("allOf", Node::Array(vec![left.clone(), right.clone()]));
    Node::Object(wrapper)
}

/// Unions two arrays, deduplicating by deep equality.
fn merge_array_values(left: &[Node], right: Vec<Node>) -> Vec<Node> {
    let mut merged = left.to_vec();
    for value in right {
        push_unique(&mut merged, value);
    }
    merged
}

/// Merges converted tuple items into an existing `prefixItems` array,
/// intersecting an index both sides define.
fn merge_prefix_items(existing: Option<&Node>, converted: Vec<Node>) -> Vec<Node> {
    let Some(Node::Array(existing)) = existing else {
        return converted;
    };
    let mut merged = existing.clone();
    for (index, item) in converted.into_iter().enumerate() {
        match merged.get_mut(index) {
            Some(slot) => {
                let combined = combine_schemas(Some(&*slot), &item);
                *slot = combined;
            }
            None => merged.push(item),
        }
    }
    merged
}

/// Draft-07 tuple form `items: [a, b]` (with `additionalItems`) becomes
/// `prefixItems: [a, b]` (with `items` for the rest).
fn upgrade_tuple_items(source: &Fields, result: &mut Fields, depth: usize) {
    let Some(items) = source.array("items") else {
        return;
    };
    let converted: Vec<Node> = items
        .iter()
        .map(|item| upgrade_node(item, depth + 2))
        .collect();
    let merged = merge_prefix_items(result.get("prefixItems"), converted);
    result.set("prefixItems", Node::Array(merged));
    match source.get("additionalItems") {
        Some(extra) if !matches!(extra, Node::Bool(true)) => {
            result.set("items", upgrade_node(extra, depth + 1));
        }
        _ => result.remove("items"),
    }
}

/// Splits draft-07 `dependencies`: an array value becomes
/// `dependentRequired`, a schema value `dependentSchemas`.
fn convert_dependencies(source: &Fields, target: &mut Fields, depth: usize) {
    let Some(dependencies) = source.object("dependencies") else {
        return;
    };
    for (key, dependency) in dependencies.iter() {
        match upgrade_node(dependency, depth + 2) {
            Node::Array(items) => merge_dependent_required(target, key, items),
            schema => merge_dependent_schema(target, key, &schema),
        }
    }
}

/// Records `key -> deps` in `dependentRequired`, unioning with an existing
/// array.
fn merge_dependent_required(target: &mut Fields, key: &str, deps: Vec<Node>) {
    let Some(map) = target.object_or_insert("dependentRequired") else {
        return;
    };
    let merged = match map.get(key) {
        None => Some(deps),
        Some(Node::Array(existing)) => Some(merge_array_values(existing, deps)),
        Some(_) => None,
    };
    if let Some(items) = merged {
        map.set(key, Node::Array(items));
    }
}

/// Records `key -> schema` in `dependentSchemas`, intersecting with an
/// existing entry.
fn merge_dependent_schema(target: &mut Fields, key: &str, schema: &Node) {
    let Some(map) = target.object_or_insert("dependentSchemas") else {
        return;
    };
    let combined = combine_schemas(map.get(key), schema);
    map.set(key, combined);
}

/// Whether some `anyOf` branch already declares a null type.
fn has_null_variant(variants: &[Node]) -> bool {
    variants.iter().any(|variant| {
        let Some(fields) = variant.object() else {
            return false;
        };
        match fields.get("type") {
            Some(Node::Text(name)) => name == "null",
            Some(Node::Array(names)) => includes_text(names, "null"),
            _ => false,
        }
    })
}

/// Makes a schema accept `null`: a scalar `type` widens to `[type, "null"]`, a
/// type array gains `"null"`, an `anyOf` gains a null branch, and anything
/// else is wrapped in `anyOf: [schema, {type: "null"}]`.
fn make_nullable(mut schema: Fields) -> Fields {
    if let Some(Node::Text(name)) = schema.get("type") {
        if name != "null" {
            let widened = type_pair(name);
            schema.set("type", widened);
        }
        return schema;
    }
    if let Some(Node::Array(names)) = schema.get("type") {
        if !includes_text(names, "null") {
            let mut widened = names.clone();
            widened.push(Node::string("null"));
            schema.set("type", Node::Array(widened));
        }
        return schema;
    }
    if let Some(Node::Array(variants)) = schema.get("anyOf") {
        if !has_null_variant(variants) {
            let mut extended = variants.clone();
            extended.push(null_type_schema());
            schema.set("anyOf", Node::Array(extended));
        }
        return schema;
    }
    let mut wrapper = Fields::new();
    wrapper.push(
        "anyOf",
        Node::Array(vec![Node::Object(schema), null_type_schema()]),
    );
    wrapper
}

/// `[name, "null"]`.
fn type_pair(name: &str) -> Node {
    Node::Array(vec![Node::string(name), Node::string("null")])
}

/// `{"type": "null"}`.
fn null_type_schema() -> Node {
    let mut schema = Fields::new();
    schema.push("type", Node::string("null"));
    Node::Object(schema)
}
