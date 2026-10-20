//! Deep equality and schema merging: oh-my-pi `equality.ts`
//! (`areJsonValuesEqual`, `mergeCompatibleEnumSchemas`,
//! `mergePropertySchemas`).

use super::tree::{Fields, Node};

/// oh-my-pi `areJsonValuesEqual`: deep equality where object member order is
/// irrelevant and numbers compare as JavaScript numbers.
pub(super) fn json_equal(left: &Node, right: &Node) -> bool {
    match (left, right) {
        (Node::Undefined, Node::Undefined) | (Node::Null, Node::Null) => true,
        (Node::Bool(a), Node::Bool(b)) => a == b,
        (Node::Number(_), Node::Number(_)) => left.number() == right.number(),
        (Node::Text(a), Node::Text(b)) => a == b,
        (Node::Array(a), Node::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| json_equal(x, y))
        }
        (Node::Object(a), Node::Object(b)) => fields_equal(a, b),
        _ => false,
    }
}

/// Object equality: same member count, and every member of `left` is in
/// `right` with an equal value.
fn fields_equal(left: &Fields, right: &Fields) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .all(|(key, x)| right.raw(key).is_some_and(|y| json_equal(x, y)))
}

/// [`json_equal`] over possibly absent values; two absent values are equal.
pub(super) fn json_equal_opt(left: Option<&Node>, right: Option<&Node>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => json_equal(a, b),
        _ => false,
    }
}

/// Appends `value` to `values` unless a deep-equal entry is already there:
/// oh-my-pi `pushEnumValue`.
pub(super) fn push_unique(values: &mut Vec<Node>, value: Node) {
    if !values.iter().any(|existing| json_equal(existing, &value)) {
        values.push(value);
    }
}

/// oh-my-pi `mergeCompatibleEnumSchemas`: unions the `enum` members of two
/// schemas that agree on `type` and on every other member; `None` otherwise.
pub(super) fn merge_compatible_enum_schemas(
    existing: &Fields,
    incoming: &Fields,
) -> Option<Fields> {
    let (existing_enum, incoming_enum) = (existing.array("enum")?, incoming.array("enum")?);
    if !json_equal_opt(existing.get("type"), incoming.get("type")) {
        return None;
    }
    // Both sides carry `enum`, so equal member counts mean equal non-`enum` counts.
    let others_match = existing.len() == incoming.len()
        && existing
            .iter()
            .filter(|(key, _)| *key != "enum")
            .all(|(key, value)| {
                incoming
                    .raw(key)
                    .is_some_and(|other| json_equal(value, other))
            });
    if !others_match {
        return None;
    }
    let mut merged = existing_enum.to_vec();
    for value in incoming_enum {
        push_unique(&mut merged, value.clone());
    }
    let mut result = existing.clone();
    result.set("enum", Node::Array(merged));
    Some(result)
}

/// The branches of a schema's `anyOf`, or the schema itself as the only
/// branch.
fn any_of_variants(schema: &Node) -> Vec<Node> {
    match schema.object().and_then(|fields| fields.array("anyOf")) {
        Some(variants) => variants.to_vec(),
        None => vec![schema.clone()],
    }
}

/// oh-my-pi `mergePropertySchemas`: the schema that accepts what either
/// property schema accepts.
pub(super) fn merge_property_schemas(existing: &Node, incoming: &Node) -> Node {
    if json_equal(existing, incoming) {
        return existing.clone();
    }
    if let (Node::Object(left), Node::Object(right)) = (existing, incoming)
        && let Some(merged) = merge_compatible_enum_schemas(left, right)
    {
        return Node::Object(merged);
    }
    let mut merged = any_of_variants(existing);
    for variant in any_of_variants(incoming) {
        push_unique(&mut merged, variant);
    }
    if merged.len() == 1
        && let Some(only) = merged.pop()
    {
        return only;
    }
    let mut wrapper = Fields::new();
    wrapper.push("anyOf", Node::Array(merged));
    Node::Object(wrapper)
}
