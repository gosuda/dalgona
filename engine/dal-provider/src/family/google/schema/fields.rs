//! Keyword tables the projection consults, copied from oh-my-pi
//! `packages/ai/src/utils/schema/fields.ts` and `normalize.ts`, plus the
//! classifier that tells schema-valued children from literal payloads.

use super::tree::Node;

/// Google Generative AI unsupported schema fields: oh-my-pi
/// `UNSUPPORTED_SCHEMA_FIELDS`, stripped by both spellings.
const UNSUPPORTED_SCHEMA_FIELDS: &[&str] = &[
    "$schema",
    "$id",
    "$anchor",
    "$ref",
    "$defs",
    "$dynamicRef",
    "$dynamicAnchor",
    "examples",
    "prefixItems",
    "unevaluatedProperties",
    "unevaluatedItems",
    "patternProperties",
    "additionalProperties",
    "propertyNames",
    "minItems",
    "maxItems",
    "uniqueItems",
    "contains",
    "minContains",
    "maxContains",
    "additionalItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "pattern",
    "format",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
    "dependencies",
    "dependentSchemas",
    "dependentRequired",
    // MCP transport annotation, not a JSON Schema keyword.
    "x-mcp-header",
    // Annotation keywords the Google wire schema has no field for.
    "deprecated",
    "readOnly",
    "writeOnly",
    "$comment",
];

/// Keywords whose stripped value is kept as text in the sibling description:
/// oh-my-pi `LIFTABLE_TO_DESCRIPTION_FIELDS`.
const LIFTABLE_TO_DESCRIPTION_FIELDS: &[&str] = &[
    "pattern",
    "format",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "default",
    "examples",
];

/// Cloud Code Assist keys per JSON Schema type: oh-my-pi
/// `CLOUD_CODE_ASSIST_TYPE_SPECIFIC_KEYS`.
const CLOUD_CODE_ASSIST_TYPE_SPECIFIC_KEYS: &[(&str, &[&str])] = &[
    (
        "array",
        &[
            "items",
            "prefixItems",
            "contains",
            "minContains",
            "maxContains",
            "minItems",
            "maxItems",
            "uniqueItems",
            "unevaluatedItems",
        ],
    ),
    (
        "object",
        &[
            "properties",
            "required",
            "additionalProperties",
            "patternProperties",
            "propertyNames",
            "minProperties",
            "maxProperties",
            "dependentRequired",
            "dependentSchemas",
            "unevaluatedProperties",
        ],
    ),
    (
        "string",
        &[
            "minLength",
            "maxLength",
            "pattern",
            "format",
            "contentEncoding",
            "contentMediaType",
        ],
    ),
    (
        "number",
        &[
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
        ],
    ),
    (
        "integer",
        &[
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
        ],
    ),
    ("boolean", &[]),
    ("null", &[]),
];

/// Keys allowed on any type when Cloud Code Assist combiners collapse: oh-my-pi
/// `CLOUD_CODE_ASSIST_SHARED_SCHEMA_KEYS`.
const CLOUD_CODE_ASSIST_SHARED_SCHEMA_KEYS: &[&str] = &["title", "description", "default", "examples"];

/// The `snake_case` spellings Google's SDK rewrites, with their `camelCase`
/// form: oh-my-pi `SNAKE_TO_CAMEL_RENAMES`.
const SNAKE_TO_CAMEL_RENAMES: &[(&str, &str)] = &[
    ("additional_properties", "additionalProperties"),
    ("any_of", "anyOf"),
    ("prefix_items", "prefixItems"),
    ("property_ordering", "propertyOrdering"),
];

/// Keywords whose value is a single subschema: oh-my-pi `SUBSCHEMA_VALUE_KEYS`.
const SUBSCHEMA_VALUE_KEYS: &[&str] = &[
    "items",
    "additionalItems",
    "unevaluatedItems",
    "not",
    "if",
    "then",
    "else",
    "contains",
    "propertyNames",
    "contentSchema",
];

/// Keywords whose value is a boolean or an object subschema: oh-my-pi
/// `BOOLEAN_OR_SCHEMA_VALUE_KEYS`.
const BOOLEAN_OR_SCHEMA_VALUE_KEYS: &[&str] = &["additionalProperties", "unevaluatedProperties"];

/// Keywords whose value is an array of subschemas: oh-my-pi
/// `SUBSCHEMA_ARRAY_KEYS`.
const SUBSCHEMA_ARRAY_KEYS: &[&str] = &["anyOf", "oneOf", "allOf", "prefixItems"];

/// Keywords whose object value maps names to subschemas: oh-my-pi
/// `SUBSCHEMA_MAP_KEYS`.
const SUBSCHEMA_MAP_KEYS: &[&str] = &[
    "properties",
    "patternProperties",
    "dependencies",
    "dependentSchemas",
    "$defs",
    "definitions",
];

/// The two combiners the passes fold: oh-my-pi `JSON_SCHEMA_COMBINERS`.
pub(super) const JSON_SCHEMA_COMBINERS: [&str; 2] = ["anyOf", "oneOf"];

/// All three composition keywords: oh-my-pi `SCHEMA_COMPOSITION_COMBINERS`.
pub(super) const SCHEMA_COMPOSITION_COMBINERS: [&str; 3] = ["allOf", "anyOf", "oneOf"];

/// Whether the Google wire has no field for `key`: oh-my-pi
/// `isGoogleUnsupportedSchemaField`.
pub(super) fn is_unsupported(key: &str) -> bool {
    UNSUPPORTED_SCHEMA_FIELDS.contains(&key)
}

/// Whether a stripped `key` is kept in the description:
/// `isDefaultLiftableToDescriptionField`.
pub(super) fn is_liftable(key: &str) -> bool {
    LIFTABLE_TO_DESCRIPTION_FIELDS.contains(&key)
}

/// The keys that constrain `json_type`, or `None` for a type name Cloud Code
/// Assist does not know.
pub(super) fn type_specific_keys(json_type: &str) -> Option<&'static [&'static str]> {
    CLOUD_CODE_ASSIST_TYPE_SPECIFIC_KEYS
        .iter()
        .find(|(name, _)| *name == json_type)
        .map(|(_, keys)| *keys)
}

/// Whether `key` constrains some JSON type: oh-my-pi
/// `ALL_CCA_TYPE_SPECIFIC_KEYS`.
pub(super) fn is_type_specific(key: &str) -> bool {
    CLOUD_CODE_ASSIST_TYPE_SPECIFIC_KEYS
        .iter()
        .any(|(_, keys)| keys.contains(&key))
}

/// Whether `key` is allowed on any type.
pub(super) fn is_shared_schema_key(key: &str) -> bool {
    CLOUD_CODE_ASSIST_SHARED_SCHEMA_KEYS.contains(&key)
}

/// The `camelCase` spelling of a `snake_case` schema key, when it has one.
pub(super) fn camel_case_rename(key: &str) -> Option<&'static str> {
    SNAKE_TO_CAMEL_RENAMES
        .iter()
        .find(|(snake, _)| *snake == key)
        .map(|(_, camel)| *camel)
}

/// What a schema-valued value is: a subschema or a map of them.
///
/// It also names the slot an object is walked in: an object in a `Schema`
/// slot has keywords for keys, one in a `Map` slot has property names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChildKind {
    /// A subschema (or an array of them).
    Schema,
    /// An object mapping names to subschemas, such as `properties`.
    Map,
}

/// Classifies only JSON Schema-valued children of an object walked in the
/// `parent` slot; instance payloads such as `default` and `enum` stay opaque:
/// oh-my-pi `classifySchemaChild`.
pub(super) fn classify_schema_child(
    key: &str,
    value: &Node,
    parent: ChildKind,
) -> Option<ChildKind> {
    if parent == ChildKind::Map {
        return Some(ChildKind::Schema);
    }
    let key = camel_case_rename(key).unwrap_or(key);
    if SUBSCHEMA_MAP_KEYS.contains(&key) {
        return Some(ChildKind::Map);
    }
    if SUBSCHEMA_VALUE_KEYS.contains(&key) || SUBSCHEMA_ARRAY_KEYS.contains(&key) {
        return Some(ChildKind::Schema);
    }
    if BOOLEAN_OR_SCHEMA_VALUE_KEYS.contains(&key) && value.object().is_some() {
        return Some(ChildKind::Schema);
    }
    None
}
