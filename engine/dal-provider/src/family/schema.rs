//! Tool-schema lowering for hosts that validate a JSON Schema subset.
//!
//! Lowering that cannot complete is an error, never a silent pass-through of
//! keywords the host rejects: the request fails as `InvalidRequest` with the
//! tool and the limit named.

use std::{borrow::Cow, error::Error, fmt};

use dal_core::RawJson;
use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Object, Value};

/// The schema family understood by the Chat and Responses request bodies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolSchemas {
    /// The schema text travels byte for byte.
    Verbatim,
    /// Moonshot's subset: unsupported keywords are removed at schema nodes.
    Moonshot,
    /// The OpenAI-compatible subset that repairs union roots and removes
    /// unsupported keywords.
    OpenAiCompat,
}

/// A schema lowering failure with the tool name and unsupported limit.
#[derive(Debug)]
pub(crate) struct SchemaError {
    message: String,
}

impl SchemaError {
    fn encode(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SchemaError {}

/// Keywords the Moonshot flavored subset names as unsupported, plus the
/// annotations both pinned references strip, at a schema node. `$id` is
/// supported (recursive definitions) and stays.
const MOONSHOT_UNSUPPORTED: [&str; 14] = [
    "format",
    "examples",
    "readOnly",
    "writeOnly",
    "deprecated",
    "$schema",
    "title",
    "$comment",
    "prefixItems",
    "unevaluatedItems",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minContains",
    "maxContains",
];
/// Keywords whose value is a schema, an array of schemas, or a tuple of them.
const SCHEMA_VALUED: [&str; 11] = [
    "anyOf",
    "oneOf",
    "allOf",
    "items",
    "additionalProperties",
    "contains",
    "propertyNames",
    "if",
    "then",
    "else",
    "not",
];
/// Keywords whose value maps names to schemas; the names are data.
const SCHEMA_MAPS: [&str; 4] = ["properties", "patternProperties", "$defs", "definitions"];
/// The deepest nesting lowered; deeper text is refused.
const MAX_DEPTH: usize = 128;

/// The tool `parameters` as the host's schema subset spells them. Text with
/// nothing to change keeps its exact bytes; a rewrite keeps every number's
/// exact text.
///
/// # Errors
///
/// [`SchemaError`], naming `tool` and the limit, when the subset
/// needs the schema rewritten and the rewrite cannot complete: the parser
/// refuses the text, the nesting passes [`MAX_DEPTH`] (the walk cannot know
/// beyond it whether anything needs stripping), or the result cannot be
/// encoded.
pub(crate) fn lower<'a>(
    tool: &str,
    parameters: &'a RawJson,
    schemas: ToolSchemas,
) -> Result<Cow<'a, RawJson>, SchemaError> {
    match schemas {
        ToolSchemas::Verbatim => Ok(Cow::Borrowed(parameters)),
        ToolSchemas::Moonshot => moonshot(tool, parameters),
        ToolSchemas::OpenAiCompat => openai_compat(tool, parameters),
    }
}

/// Lowers `parameters` to the OpenAI-compatible subset: the keyword the
/// subset does not know is removed, a combiner's parent `type` moves into its
/// branches, a literal union collapses to a typed `enum`, and the root
/// becomes an object schema. The root union of object shapes is merged into
/// one object schema whose `properties` union the branches' — the merge never
/// loses a declared parameter and keeps the root's own `required`. Numbers
/// keep their exact text.
///
/// # Errors
///
/// [`SchemaError`], naming `tool` and the limit, when the rewrite
/// cannot complete: the parser refuses the text, the nesting passes
/// [`MAX_DEPTH`], or the result cannot be encoded.
fn openai_compat<'a>(tool: &str, parameters: &'a RawJson) -> Result<Cow<'a, RawJson>, SchemaError> {
    let refused = |limit: String| {
        SchemaError::encode(format!(
            "tool `{tool}`: its parameters schema cannot be lowered to the OpenAI-compatible subset ({limit})"
        ))
    };
    let mut schema = parse_lossless(parameters.as_str())
        .map_err(|error| refused(format!("the JSON parser refused it: {error}")))?;
    let Some(mut changed) = normalize_root(&mut schema, 0) else {
        return Err(refused(format!(
            "it nests deeper than {MAX_DEPTH} levels; flatten the schema"
        )));
    };
    if !schema.is_object() {
        return Err(refused(String::from(
            "the root schema must be a JSON object",
        )));
    }
    // The root of the wire shape must be an object schema: the root union of
    // object shapes merges into one, a typed root keeps its `type`, and any
    // other typeless root gains `type: "object"` beside the combiner it kept.
    if let Some(merged) = merge_root_object_union(&schema) {
        schema = merged;
        changed = true;
    } else if schema.get("anyOf").is_some_and(Value::is_array)
        || schema.get("oneOf").is_some_and(Value::is_array)
    {
        return Err(refused(String::from(
            "the root union branches must be object schemas",
        )));
    } else if schema.get("type").is_none() {
        schema.insert("type", Value::from("object"));
        changed = true;
    }
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(refused(String::from(
            "the root schema must have type \"object\"",
        )));
    }
    if !changed {
        return Ok(Cow::Borrowed(parameters));
    }
    let text = sonic_rs::to_string(&schema)
        .map_err(|error| refused(format!("the rewrite cannot be encoded: {error}")))?;
    RawJson::parse(&text)
        .map(Cow::Owned)
        .map_err(|error| refused(format!("the rewrite is not valid JSON: {error}")))
}

/// The combiner keys the subset reads.
const COMBINERS: [&str; 3] = ["anyOf", "oneOf", "allOf"];

/// The schema-valued keywords whose recursion [`normalize_at`] visits after
/// the combiner branches and the literal-union collapse; the combiners
/// themselves are walked first, in source order.
const SINGLE_SCHEMA_VALUED: [&str; 8] = [
    "items",
    "additionalProperties",
    "contains",
    "propertyNames",
    "if",
    "then",
    "else",
    "not",
];

/// Where a schema node sits in the tool's parameters. Only a nested node
/// moves its parent `type` into combiner branches; the root's `type` is the
/// wire shape the caller guarantees afterwards.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Position {
    Root,
    Nested,
}

/// Rewrites the root schema in place. `Some(changed)` when the walk
/// completed, `None` when the nesting passes [`MAX_DEPTH`].
fn normalize_root(node: &mut Value, depth: usize) -> Option<bool> {
    normalize_at(node, depth, Position::Root)
}

/// Rewrites one nested schema node in place.
fn normalize_nested(node: &mut Value, depth: usize) -> Option<bool> {
    normalize_at(node, depth, Position::Nested)
}

/// Rewrites the schema at `node` in place to the subset. `Some(changed)` when
/// the walk completed, `None` when the nesting passes [`MAX_DEPTH`].
fn normalize_at(node: &mut Value, depth: usize, position: Position) -> Option<bool> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(tuple) = node.as_array_mut() {
        let mut changed = false;
        for child in tuple.iter_mut() {
            changed |= normalize_nested(child, depth + 1)?;
        }
        return Some(changed);
    }
    let Some(object) = node.as_object_mut() else {
        return Some(false);
    };
    let mut changed = false;
    if object.remove(&"optional").is_some() {
        changed = true;
    }
    if position == Position::Nested {
        changed |= move_type_into_combiner_branches(object);
    }
    for combiner in COMBINERS {
        let Some(branches) = object.get_mut(&combiner).and_then(Value::as_array_mut) else {
            continue;
        };
        for branch in branches.iter_mut() {
            changed |= normalize_nested(branch, depth + 1)?;
        }
    }
    if object.get(&"anyOf").is_some_and(Value::is_array) && object.get(&"type").is_none() {
        changed |= collapse_const_union(object);
    }
    for key in SINGLE_SCHEMA_VALUED {
        if let Some(child) = object.get_mut(&key) {
            changed |= normalize_nested(child, depth + 1)?;
        }
    }
    for key in SCHEMA_MAPS {
        if let Some(map) = object.get_mut(&key).and_then(Value::as_object_mut) {
            for (_, entry) in map.iter_mut() {
                changed |= normalize_nested(entry, depth + 1)?;
            }
        }
    }
    Some(changed)
}

/// Moves a nested node's parent `type` into the combiner branches that do not
/// already declare one, then drops the parent `type`. The subset rejects a
/// sibling `type` beside a combiner, and the rewrite keeps the schema
/// equivalent. Returns whether the node changed.
fn move_type_into_combiner_branches(object: &mut Object) -> bool {
    let has_combiner = COMBINERS
        .iter()
        .any(|key| object.get(key).is_some_and(Value::is_array));
    if !has_combiner {
        return false;
    }
    let Some(parent_type) = object.get(&"type").cloned() else {
        return false;
    };
    for combiner in COMBINERS {
        let Some(branches) = object.get_mut(&combiner).and_then(Value::as_array_mut) else {
            continue;
        };
        for branch in branches.iter_mut() {
            let Some(branch) = branch.as_object_mut() else {
                continue;
            };
            if branch.get(&"type").is_none() {
                branch.insert("type", parent_type.clone());
            }
        }
    }
    object.remove(&"type");
    true
}

/// Collapses a homogeneous all-`const` `anyOf` union into a typed `enum`.
/// Returns whether the node changed.
fn collapse_const_union(object: &mut Object) -> bool {
    let Some(branches) = object.get(&"anyOf").and_then(Value::as_array) else {
        return false;
    };
    if branches.len() < 2 {
        return false;
    }
    let mut values: Vec<Value> = Vec::with_capacity(branches.len());
    let mut shared_type: Option<String> = None;
    for branch in branches {
        let Some(branch) = branch.as_object() else {
            return false;
        };
        if branch.len() != 2 {
            return false;
        }
        let Some(spelled) = branch.get(&"type").and_then(Value::as_str) else {
            return false;
        };
        if !matches!(spelled, "string" | "number" | "integer" | "boolean") {
            return false;
        }
        match &shared_type {
            None => shared_type = Some(spelled.to_owned()),
            Some(shared) if shared != spelled => return false,
            Some(_) => {}
        }
        let Some(constant) = branch.get(&"const") else {
            return false;
        };
        values.push(constant.clone());
    }
    let Some(shared_type) = shared_type else {
        return false;
    };
    object.remove(&"anyOf");
    object.insert("type", shared_type.as_str());
    object.insert("enum", values);
    true
}

/// Merges a root-level `anyOf`/`oneOf` union of object shapes into one object
/// schema, in the wire shape the subset requires: the union's `properties`
/// are merged per name and the union requires only branch-common names. A
/// root whose branches are not object shapes, or whose `properties`/`required`
/// have the wrong shape, is not merged and stays as it is.
fn merge_root_object_union(schema: &Value) -> Option<Value> {
    let selected = if schema.get("anyOf").is_some_and(Value::is_array) {
        "anyOf"
    } else if schema.get("oneOf").is_some_and(Value::is_array) {
        "oneOf"
    } else {
        return None;
    };
    let branches = schema.get(selected).and_then(Value::as_array)?;
    if branches.is_empty() {
        return None;
    }
    let root_properties = schema.get("properties");
    if root_properties.is_some_and(|value| !value.is_object()) {
        return None;
    }
    let root_required = schema.get("required");
    if root_required.is_some_and(|value| !value.is_array()) {
        return None;
    }
    // An untyped branch is a constraint-only variant over the root's own
    // properties, so it merges like an object branch.
    let mut object_branches: Vec<&Object> = Vec::with_capacity(branches.len());
    for branch in branches {
        let branch = branch.as_object()?;
        match branch.get(&"type").map(Value::as_str) {
            None | Some(Some("object")) => {}
            _ => return None,
        }
        if branch
            .get(&"properties")
            .is_some_and(|value| !value.is_object())
        {
            return None;
        }
        if branch
            .get(&"required")
            .is_some_and(|value| !value.is_array())
        {
            return None;
        }
        object_branches.push(branch);
    }
    let mut merged_properties = Object::new();
    if let Some(properties) = root_properties.and_then(Value::as_object) {
        for (name, property) in properties {
            merged_properties.insert(name, property.clone());
        }
    }
    for branch in &object_branches {
        let Some(branch_properties) = branch.get(&"properties").and_then(Value::as_object) else {
            continue;
        };
        for (name, property) in branch_properties {
            let merged = match merged_properties.get(&name).cloned() {
                None => property.clone(),
                Some(existing) if json_text(&existing) == json_text(property) => property.clone(),
                Some(existing) => {
                    let mut fork = Object::new();
                    fork.insert("anyOf", vec![existing, property.clone()]);
                    Value::from(fork)
                }
            };
            merged_properties.insert(name, merged);
        }
    }
    // A union requires only branch-common names; the root's own `required`
    // always applies.
    let mut required_names: Vec<&str> = string_names(root_required);
    let mut branch_required: Vec<Vec<&str>> = Vec::with_capacity(object_branches.len());
    for branch in &object_branches {
        branch_required.push(string_names(branch.get(&"required")));
    }
    if let Some(first) = branch_required.first() {
        for name in first {
            if !required_names.contains(name)
                && branch_required.iter().all(|names| names.contains(name))
            {
                required_names.push(name);
            }
        }
    }
    let mut merged = Object::new();
    if let Some(rest) = schema.as_object() {
        for (key, value) in rest {
            if key != "anyOf" && key != "oneOf" {
                merged.insert(key, value.clone());
            }
        }
    }
    merged.insert("type", "object");
    merged.insert("properties", merged_properties);
    if !required_names.is_empty() {
        merged.insert("required", required_names);
    }
    Some(Value::from(merged))
}

/// The string names of a `required` array; anything else has none.
fn string_names(required: Option<&Value>) -> Vec<&str> {
    required
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The JSON text of one value; two values are the same property schema when
/// their texts match.
fn json_text(value: &Value) -> String {
    sonic_rs::to_string(value).unwrap_or_default()
}

fn moonshot<'a>(tool: &str, parameters: &'a RawJson) -> Result<Cow<'a, RawJson>, SchemaError> {
    let refused = |limit: String| {
        SchemaError::encode(format!(
            "tool `{tool}`: its parameters schema cannot be lowered to the Moonshot subset ({limit})"
        ))
    };
    let mut schema = parse_lossless(parameters.as_str())
        .map_err(|error| refused(format!("the JSON parser refused it: {error}")))?;
    match strip(&mut schema, 0) {
        Some(false) => return Ok(Cow::Borrowed(parameters)),
        Some(true) => {}
        None => {
            return Err(refused(format!(
                "it nests deeper than {MAX_DEPTH} levels; flatten the schema"
            )));
        }
    }
    let text = sonic_rs::to_string(&schema)
        .map_err(|error| refused(format!("the rewrite cannot be encoded: {error}")))?;
    RawJson::parse(&text)
        .map(Cow::Owned)
        .map_err(|error| refused(format!("the rewrite is not valid JSON: {error}")))
}

/// Parses one JSON value keeping every number's exact text. A plain
/// `sonic_rs::from_str::<Value>` coerces integers past `u64` to `f64` and
/// re-spells `1e3` or `1.0`; the raw-number mode keeps the digits and
/// re-serializes them exactly. `end` rejects trailing bytes like `from_str`.
/// This is the recipe of `dal-wire`'s `parse_value_lossless`, which that
/// crate keeps private and which cannot be imported here (`dal-wire`
/// depends on this crate).
fn parse_lossless(text: &str) -> Result<Value, sonic_rs::Error> {
    let mut deserializer = sonic_rs::Deserializer::from_str(text).use_rawnumber();
    let value = deserializer.deserialize()?;
    deserializer.end()?;
    Ok(value)
}

/// Removes the unsupported keywords from the schema at `node` and below.
/// `Some(true)` when anything was removed, `None` when the nesting passes
/// [`MAX_DEPTH`].
fn strip(node: &mut Value, depth: usize) -> Option<bool> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(tuple) = node.as_array_mut() {
        let mut changed = false;
        for child in tuple.iter_mut() {
            changed |= strip(child, depth + 1)?;
        }
        return Some(changed);
    }
    let Some(object) = node.as_object_mut() else {
        return Some(false);
    };
    let mut changed = false;
    let mut too_deep = false;
    let before = object.len();
    object.retain(|key, child| {
        if MOONSHOT_UNSUPPORTED.contains(&key) {
            return false;
        }
        if SCHEMA_VALUED.contains(&key) {
            match strip(child, depth + 1) {
                Some(removed) => changed |= removed,
                None => too_deep = true,
            }
        } else if SCHEMA_MAPS.contains(&key)
            && let Some(map) = child.as_object_mut()
        {
            for (_, entry) in map.iter_mut() {
                match strip(entry, depth + 1) {
                    Some(removed) => changed |= removed,
                    None => too_deep = true,
                }
            }
        }
        true
    });
    // The `retain` closure is a temporary: its borrows of `changed` and
    // `too_deep` ended with the call.
    changed |= object.len() != before;
    (!too_deep).then_some(changed)
}
