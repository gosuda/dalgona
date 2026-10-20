//! `anyOf` and `oneOf` folding for the legacy Cloud Code Assist spelling:
//! oh-my-pi `normalize.ts` `mergeObjectCombinerVariants`,
//! `collapseMixedTypeCombinerVariants`, `collapseSameTypeCombinerVariants`,
//! `mergeSchemaDescriptions`, `dropForeignTypeKeywords`, and
//! `stripResidualCombiners`.
//!
//! Claude behind Cloud Code Assist takes no combiner, so object branches
//! merge into one object, same-type branches collapse onto one, and branches
//! of mixed type collapse onto the first non-null type. Every fold returns
//! `None` when it does not apply, which is oh-my-pi's "returns the schema
//! unchanged".

use super::equality::{json_equal, merge_compatible_enum_schemas, merge_property_schemas};
use super::fields::{
    ChildKind, JSON_SCHEMA_COMBINERS, classify_schema_child, is_shared_schema_key,
    is_type_specific, type_specific_keys,
};
use super::tree::{Fields, MAX_DEPTH, Node};

/// Deletes the keywords that constrain a JSON type other than `json_type`,
/// such as `items` on a string node.
pub(super) fn drop_foreign_type_keywords(schema: &mut Fields, json_type: &str) {
    let allowed = type_specific_keys(json_type).unwrap_or(&[]);
    let foreign: Vec<String> = schema
        .iter()
        .filter(|(key, _)| is_type_specific(key) && !allowed.contains(key))
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in foreign {
        schema.remove(&key);
    }
}

/// The string entries of an array value, or none.
fn string_entries(value: Option<&Node>) -> Vec<&str> {
    value
        .and_then(Node::array)
        .map(|items| items.iter().filter_map(Node::text).collect())
        .unwrap_or_default()
}

/// Whether one branch is an object schema the merge can fold; the branch
/// itself when so.
fn mergeable_object_branch(entry: &Node) -> Option<&Fields> {
    let fields = entry.object()?;
    let has_object_shape = fields.object("properties").is_some()
        || fields.array("required").is_some()
        || fields.has("additionalProperties");
    let object_type = match fields.get("type") {
        None => has_object_shape,
        Some(Node::Text(name)) => name == "object",
        Some(_) => false,
    };
    let properties_ok = fields.get("properties").is_none() || fields.object("properties").is_some();
    let required_ok = fields.get("required").is_none() || fields.array("required").is_some();
    (object_type && properties_ok && required_ok).then_some(fields)
}

/// Merges object-shaped `anyOf`/`oneOf` branches into the node's own
/// `properties`, keeping as `required` only what every branch requires.
pub(super) fn merge_object_combiner_variants(schema: &Fields, combiner: &str) -> Option<Fields> {
    let branches = schema.array(combiner).filter(|items| !items.is_empty())?;
    let variants = branches
        .iter()
        .map(mergeable_object_branch)
        .collect::<Option<Vec<&Fields>>>()?;

    let own_properties = schema.object("properties");
    let mut merged = own_properties.cloned().unwrap_or_default();
    for variant in &variants {
        let Some(properties) = variant.object("properties") else {
            continue;
        };
        for (name, property) in properties.iter() {
            let next = match merged.get(name) {
                None => property.clone(),
                Some(existing) => merge_property_schemas(existing, property),
            };
            merged.set(name, next);
        }
    }

    let required = merged_required(schema, &variants, own_properties, &merged);
    let mut next = schema.without(combiner);
    next.set("type", Node::string("object"));
    next.set("properties", Node::Object(merged));
    if required.is_empty() {
        next.remove("required");
    } else {
        next.set(
            "required",
            Node::Array(required.into_iter().map(Node::Text).collect()),
        );
    }
    Some(next)
}

/// The `required` names of a merged object, in property order: the names every
/// branch requires, plus the node's own required names that it declared
/// itself.
fn merged_required(
    schema: &Fields,
    variants: &[&Fields],
    own_properties: Option<&Fields>,
    merged: &Fields,
) -> Vec<String> {
    let mut intersection: Option<Vec<&str>> = None;
    for variant in variants {
        let required = string_entries(variant.get("required"));
        intersection = Some(match intersection {
            None => required,
            Some(common) => common
                .into_iter()
                .filter(|name| required.contains(name))
                .collect(),
        });
    }
    let combined = intersection.unwrap_or_default();
    let parent = string_entries(schema.get("required"));
    merged
        .iter()
        .map(|(name, _)| name)
        .filter(|name| {
            combined.contains(name)
                || (parent.contains(name) && own_properties.is_some_and(|own| own.has(name)))
        })
        .map(str::to_owned)
        .collect()
}

/// Joins two descriptions with a blank line, tolerating non-strings:
/// oh-my-pi `mergeSchemaDescriptions`.
fn merge_schema_descriptions(existing: &Node, incoming: &Node) -> String {
    let Some(existing) = existing.text() else {
        return incoming.text().unwrap_or("").to_owned();
    };
    match incoming.text() {
        Some(incoming) if !incoming.is_empty() && incoming != existing => {
            if existing.is_empty() {
                incoming.to_owned()
            } else {
                format!("{existing}\n\n{incoming}")
            }
        }
        _ => existing.to_owned(),
    }
}

/// The type-specific fields of every branch, when every branch has a distinct
/// known `type` and only keys that type (or any type) allows.
fn gather_mixed_branches(branches: &[Node]) -> Option<(Vec<&str>, Fields)> {
    let mut types: Vec<&str> = Vec::new();
    let mut merged = Fields::new();
    for entry in branches {
        let fields = entry.object()?;
        let variant_type = fields.text("type")?;
        if types.contains(&variant_type) {
            return None;
        }
        let allowed = type_specific_keys(variant_type)?;
        for (key, value) in fields.iter() {
            if key == "type" {
                continue;
            }
            if !allowed.contains(&key) && !is_shared_schema_key(key) {
                return None;
            }
            absorb_field(&mut merged, key, value)?;
        }
        types.push(variant_type);
    }
    Some((types, merged))
}

/// Records `key: value` in `target`; a differing value is only acceptable for
/// `description`, whose texts join. `None` is the structural incompatibility.
fn absorb_field(target: &mut Fields, key: &str, value: &Node) -> Option<()> {
    let merged = match target.get(key) {
        Some(existing) if !json_equal(existing, value) => {
            if key != "description" {
                return None;
            }
            Node::Text(merge_schema_descriptions(existing, value))
        }
        _ => value.clone(),
    };
    target.set(key, merged);
    Some(())
}

/// Collapses `anyOf`/`oneOf` branches of distinct types onto the first
/// non-null type, keeping the fields that type allows.
pub(super) fn collapse_mixed_type_combiner_variants(
    schema: &Fields,
    combiner: &str,
) -> Option<Fields> {
    let branches = schema.array(combiner).filter(|items| !items.is_empty())?;
    let (types, merged) = gather_mixed_branches(branches)?;
    if types.len() < 2 || types.iter().all(|name| *name == "object") {
        return None;
    }
    let chosen = types
        .iter()
        .find(|name| **name != "null")
        .or(types.first())
        .copied()?;
    let allowed = type_specific_keys(chosen).unwrap_or(&[]);
    let mut next = schema.without(combiner);
    next.set("type", Node::string(chosen));
    // Sibling keys copied from the parent may belong to a different type.
    drop_foreign_type_keywords(&mut next, chosen);
    for (key, value) in merged.iter() {
        if !allowed.contains(&key) && !is_shared_schema_key(key) {
            continue;
        }
        match next.get(key) {
            Some(existing) if !json_equal(existing, value) => {
                if key != "description" {
                    return None;
                }
                let joined = merge_schema_descriptions(existing, value);
                next.set(key, Node::Text(joined));
            }
            Some(_) => {}
            None => next.set(key, value.clone()),
        }
    }
    Some(next)
}

/// The branch a same-type collapse keeps, or `None` when the `enum` branches
/// disagree and the schema must stay as it is.
fn same_type_survivor(variants: &[&Fields]) -> Option<Fields> {
    let first = variants.first().copied()?;
    let enum_branches = variants
        .iter()
        .filter(|variant| variant.array("enum").is_some())
        .count();
    if enum_branches == variants.len() {
        // Every branch is an `enum` schema: union the members, but only when the
        // branches agree on `type` and on every other field.
        let mut merged = first.clone();
        for variant in variants.iter().skip(1) {
            merged = merge_compatible_enum_schemas(&merged, variant)?;
        }
        return Some(merged);
    }
    if enum_branches > 0 {
        // A branch without `enum` is broader: collapse onto it, so the result
        // never narrows to an enum branch's members.
        let broader = variants
            .iter()
            .find(|variant| variant.array("enum").is_none())
            .copied()
            .unwrap_or(first);
        return Some(broader.clone());
    }
    Some(first.clone())
}

/// Collapses `anyOf`/`oneOf` branches that all share one `type` onto a
/// single branch.
pub(super) fn collapse_same_type_combiner_variants(
    schema: &Fields,
    combiner: &str,
) -> Option<Fields> {
    let branches = schema.array(combiner).filter(|items| !items.is_empty())?;
    let mut common: Option<&str> = None;
    let mut variants: Vec<&Fields> = Vec::with_capacity(branches.len());
    for entry in branches {
        let fields = entry.object()?;
        let variant_type = fields.text("type")?;
        if common.is_some_and(|seen| seen != variant_type) {
            return None;
        }
        common = Some(variant_type);
        variants.push(fields);
    }
    let collapsed = same_type_survivor(&variants)?;
    let mut next = schema.without(combiner);
    for (key, value) in collapsed.iter() {
        if !next.has(key) {
            next.push(key, value.clone());
        }
    }
    Some(next)
}

/// Strips every residual `anyOf`/`oneOf` that a same-type or mixed-type
/// collapse can handle, to a fixpoint at each node. Object merging can
/// synthesize a fresh combiner after the child walk already ran, which is why
/// this pass exists.
pub(super) fn strip_residual_combiners(value: &Node, slot: ChildKind, depth: usize) -> Node {
    match value {
        Node::Array(_) if depth > MAX_DEPTH => Node::Array(Vec::new()),
        Node::Array(items) => Node::Array(
            items
                .iter()
                .map(|item| strip_residual_combiners(item, ChildKind::Schema, depth + 1))
                .collect(),
        ),
        Node::Object(_) if depth > MAX_DEPTH => Node::empty_object(),
        Node::Object(fields) => Node::Object(strip_residual_object(fields, slot, depth)),
        other => other.clone(),
    }
}

/// One object of [`strip_residual_combiners`].
fn strip_residual_object(fields: &Fields, slot: ChildKind, depth: usize) -> Fields {
    let mut result = Fields::new();
    for (key, entry) in fields.iter() {
        let next = match classify_schema_child(key, entry, slot) {
            Some(kind) => strip_residual_combiners(entry, kind, depth + 1),
            None => entry.clone(),
        };
        result.push(key, next);
    }
    if slot == ChildKind::Map {
        return result;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for combiner in JSON_SCHEMA_COMBINERS {
            if let Some(collapsed) = collapse_same_type_combiner_variants(&result, combiner) {
                result = collapsed;
                changed = true;
            }
            if let Some(collapsed) = collapse_mixed_type_combiner_variants(&result, combiner) {
                result = collapsed;
                changed = true;
            }
        }
    }
    result
}
