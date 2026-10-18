//! The projection driver: oh-my-pi `normalize.ts` `normalizeSchema` with the
//! option sets that `normalizeSchemaForGoogle` and `normalizeSchemaForCCA` pin.
//!
//! One walk serves both spellings. The modern spelling (`parametersJsonSchema`)
//! keeps combiners, widens a null branch into `nullable`, orders object
//! properties, and keeps only string enums. The legacy spelling (Claude behind
//! Cloud Code Assist, `parameters`) takes no combiner and no `nullable`: object
//! branches merge, same-type and mixed-type branches collapse, a nullable
//! property leaves its `required` list, and a schema that still carries a
//! keyword the wire rejects falls back to an empty object schema.
//!
//! Two departures from the reference are deliberate. `const` and `nullable` are
//! keywords only in a schema slot; under `properties` they are property names,
//! and the reference would swallow a parameter called `const` or `nullable`.
//! The nullable lift also visits each property once where the reference walks
//! it twice, which is exponential in nesting depth and yields the same schema.

use std::borrow::Cow;

use super::{
    Spelling,
    combiners::{
        collapse_mixed_type_combiner_variants, collapse_same_type_combiner_variants,
        drop_foreign_type_keywords, merge_object_combiner_variants, strip_residual_combiners,
    },
    dereference::dereference,
    draft::upgrade,
    equality::{json_equal, push_unique},
    fields::{
        ChildKind, JSON_SCHEMA_COMBINERS, SCHEMA_COMPOSITION_COMBINERS, camel_case_rename,
        classify_schema_child, is_liftable, is_type_specific, is_unsupported, type_specific_keys,
    },
    spill::spill_to_description,
    tree::{Fields, MAX_DEPTH, Node, includes_text, truthy},
    validate::is_valid,
};

/// Projects a parsed tool schema onto the subset the wire accepts; `None`
/// when the schema is too large to inline, which the caller sends as it is.
pub(super) fn normalize(schema: &Node, spelling: Spelling) -> Option<Node> {
    let upgraded = upgrade(schema);
    let inlined = dereference(&upgraded)?;
    let walker = Walker {
        legacy: spelling == Spelling::Legacy,
    };
    let walked = walker.node(&inlined, ChildKind::Schema, 0);
    if !walker.legacy {
        return Some(walked);
    }
    let collapsed = strip_residual_combiners(&walked, ChildKind::Schema, 0);
    let projected = lift_nullable(&collapsed, ChildKind::Schema, 0);
    if has_residual(&projected, ChildKind::Schema, 0) || !is_valid(&projected) {
        return Some(fallback());
    }
    Some(projected)
}

/// The schema a legacy projection falls back to: an object without
/// properties.
fn fallback() -> Node {
    let mut fields = Fields::new();
    fields.push("type", Node::string("object"));
    fields.push("properties", Node::empty_object());
    Node::Object(fields)
}

/// The state of one walk.
#[derive(Clone, Copy)]
struct Walker {
    /// Whether the target is the legacy Cloud Code Assist spelling.
    legacy: bool,
}

impl Walker {
    /// Projects one value found in `slot`.
    fn node(self, value: &Node, slot: ChildKind, depth: usize) -> Node {
        match value {
            Node::Array(_) if depth > MAX_DEPTH => Node::Array(Vec::new()),
            Node::Array(items) => Node::Array(
                items
                    .iter()
                    .map(|item| self.node(item, slot, depth + 1))
                    .collect(),
            ),
            // A bare boolean is a subschema only in a schema slot: `true`
            // accepts anything and `false` accepts nothing.
            Node::Bool(flag) if slot == ChildKind::Schema => {
                let mut schema = Fields::new();
                if !flag {
                    schema.push("not", Node::empty_object());
                }
                Node::Object(schema)
            }
            Node::Object(_) if depth > MAX_DEPTH => Node::empty_object(),
            Node::Object(fields) => Node::Object(self.object(fields, slot, depth)),
            other => other.clone(),
        }
    }

    /// Projects one object: keyword spelling first, then its members, then the
    /// whole-node repairs.
    fn object(self, source: &Fields, slot: ChildKind, depth: usize) -> Fields {
        let prepared = match slot {
            ChildKind::Map => Cow::Borrowed(source),
            ChildKind::Schema => self.prepare(source),
        };
        let obj: &Fields = &prepared;
        let mut result = Fields::new();
        let mut spill: Vec<(&str, &Node)> = Vec::new();
        if self.fold_const_union(obj, &mut result, &mut spill, slot, depth) {
            spill_to_description(&mut result, &spill);
            return self.finish(result, slot);
        }
        let const_value = self.copy_members(obj, &mut result, &mut spill, slot, depth);
        self.refine(&mut result, const_value);
        spill_to_description(&mut result, &spill);
        self.finish(result, slot)
    }

    /// The object with `snake_case` keywords renamed, a multi-type `type`
    /// split into branches, and (modern only) a null branch widened into
    /// `nullable`.
    fn prepare(self, source: &Fields) -> Cow<'_, Fields> {
        let mut current = Cow::Borrowed(source);
        if let Some(renamed) = rename_snake_case(&current) {
            current = Cow::Owned(renamed);
        }
        if let Some(split) = split_type_array(&current) {
            current = Cow::Owned(split);
        }
        if !self.legacy
            && let Some(widened) = collapse_null_branch(&current)
        {
            current = Cow::Owned(widened);
        }
        current
    }

    /// Folds an `anyOf` or `oneOf` whose every branch is a `const` into one
    /// `enum`, then carries the remaining members. `false` when the object has
    /// no such combiner.
    fn fold_const_union<'a>(
        self,
        obj: &'a Fields,
        result: &mut Fields,
        spill: &mut Vec<(&'a str, &'a Node)>,
        slot: ChildKind,
        depth: usize,
    ) -> bool {
        for combiner in JSON_SCHEMA_COMBINERS {
            let Some(variants) = obj
                .array(combiner)
                .filter(|variants| is_const_union(variants))
            else {
                continue;
            };
            self.fold_const_variants(variants, result);
            for (key, entry) in obj.iter() {
                if key != combiner && !result.has(key) {
                    self.carry(key, entry, result, spill, slot, depth);
                }
            }
            return true;
        }
        false
    }

    /// Carries every member of `obj`; a `const` is returned instead.
    fn copy_members<'a>(
        self,
        obj: &'a Fields,
        result: &mut Fields,
        spill: &mut Vec<(&'a str, &'a Node)>,
        slot: ChildKind,
        depth: usize,
    ) -> Option<Node> {
        let mut const_value = None;
        for (key, entry) in obj.iter() {
            if key == "const" && slot == ChildKind::Schema {
                const_value = Some(entry.clone());
                continue;
            }
            self.carry(key, entry, result, spill, slot, depth);
        }
        const_value
    }

    /// Carries one member into `result`: dropped when the wire has no field
    /// for it (its constraint spilled into the description when readable), else
    /// with its subschemas projected.
    fn carry<'a>(
        self,
        key: &'a str,
        entry: &'a Node,
        result: &mut Fields,
        spill: &mut Vec<(&'a str, &'a Node)>,
        slot: ChildKind,
        depth: usize,
    ) {
        let in_schema = slot == ChildKind::Schema;
        if in_schema && is_unsupported(key) {
            if is_liftable(key) {
                spill.push((key, entry));
            }
            return;
        }
        if in_schema && self.legacy && key == "nullable" {
            return;
        }
        if in_schema
            && !self.legacy
            && key == "not"
            && unrepresentable_enum(entry, ChildKind::Schema, depth)
        {
            return;
        }
        let next = match classify_schema_child(key, entry, slot) {
            Some(kind) => self.node(entry, kind, depth + 1),
            None => entry.clone(),
        };
        result.set(key, next);
    }

    /// The whole-node repairs after the members are in: scalar `type`, the
    /// `const` as an enum member, an inferred `type`, `nullable`, property
    /// order, and an empty `properties`.
    fn refine(self, result: &mut Fields, const_value: Option<Node>) {
        self.scalarize_type_array(result);
        if let Some(value) = const_value {
            let value_type = value.json_type();
            let mut members = result
                .array("enum")
                .map(<[Node]>::to_vec)
                .unwrap_or_default();
            push_unique(&mut members, value);
            result.set("enum", Node::Array(members));
            if !truthy(result.get("type")) {
                result.set("type", value_type.map_or(Node::Undefined, Node::string));
            }
        }
        if !truthy(result.get("type"))
            && result.array("anyOf").is_none()
            && result.array("oneOf").is_none()
        {
            let inferred = result
                .array("enum")
                .filter(|members| !members.is_empty())
                .and_then(common_json_type);
            if let Some(name) = inferred {
                result.set("type", Node::string(name));
            }
        }
        if !self.legacy && result.is_text("type", "null") {
            result.remove("type");
            result.set("nullable", Node::Bool(true));
        }
        if !self.legacy && result.is_text("type", "object") && !result.has("propertyOrdering") {
            let names: Vec<Node> = result
                .object("properties")
                .map(|properties| {
                    properties
                        .iter()
                        .map(|(name, _)| Node::string(name))
                        .collect()
                })
                .unwrap_or_default();
            if names.len() > 1 {
                result.set("propertyOrdering", Node::Array(names));
            }
        }
        if result.is_text("type", "object") && !result.has("properties") {
            result.set("properties", Node::empty_object());
        }
    }

    /// Writes the `enum` and the `type` of a const union into `result`.
    fn fold_const_variants(self, variants: &[Node], result: &mut Fields) {
        let mut members: Vec<Node> = Vec::new();
        for variant in variants {
            if let Some(value) = variant.object().and_then(|fields| fields.raw("const")) {
                push_unique(&mut members, value.clone());
            }
        }
        let inferred: Vec<&str> = members.iter().filter_map(Node::json_type).collect();
        result.set("enum", Node::Array(members));
        let explicit: Vec<&str> = variants
            .iter()
            .filter_map(|variant| variant.object()?.text("type"))
            .collect();
        let named = explicit
            .first()
            .copied()
            .filter(|name| !name.is_empty() && explicit.len() == variants.len());
        if let Some(name) = named
            && explicit.iter().all(|other| *other == name)
        {
            result.set("type", Node::string(name));
            return;
        }
        if let Some(name) = single_type(&inferred) {
            result.set("type", Node::string(name));
            return;
        }
        let non_null: Vec<&str> = inferred
            .iter()
            .copied()
            .filter(|name| *name != "null")
            .collect();
        if inferred.contains(&"null")
            && let Some(name) = single_type(&non_null)
        {
            result.set("type", Node::string(name));
            if !self.legacy {
                result.set("nullable", Node::Bool(true));
            }
        }
    }

    /// Collapses a `type` array to its first non-null type; a `null` member
    /// becomes `nullable` unless the target has no such keyword.
    fn scalarize_type_array(self, result: &mut Fields) {
        let Some(listed) = result.array("type") else {
            return;
        };
        let types: Vec<String> = listed
            .iter()
            .filter_map(Node::text)
            .map(str::to_owned)
            .collect();
        let chosen = types
            .iter()
            .find(|name| name.as_str() != "null")
            .or_else(|| types.first())
            .cloned();
        if !self.legacy && types.iter().any(|name| name == "null") {
            result.set("nullable", Node::Bool(true));
        }
        match chosen {
            Some(name) => {
                result.set("type", Node::string(&name));
                drop_foreign_type_keywords(result, &name);
            }
            None => result.set("type", Node::Undefined),
        }
    }

    /// The combiner folds (legacy) or the string-only enum rule (modern).
    fn finish(self, schema: Fields, slot: ChildKind) -> Fields {
        let mut current = schema;
        if !self.legacy {
            return if slot == ChildKind::Schema {
                drop_non_string_enum(current)
            } else {
                current
            };
        }
        for combiner in JSON_SCHEMA_COMBINERS {
            if let Some(next) = merge_object_combiner_variants(&current, combiner) {
                current = next;
            }
            if let Some(next) = collapse_mixed_type_combiner_variants(&current, combiner) {
                current = next;
            }
            if let Some(next) = collapse_same_type_combiner_variants(&current, combiner) {
                current = next;
            }
        }
        current
    }
}

/// `None` when no keyword needs renaming. A `snake_case` spelling wins over
/// the `camelCase` member it renames onto.
fn rename_snake_case(obj: &Fields) -> Option<Fields> {
    if !obj.iter().any(|(key, _)| camel_case_rename(key).is_some()) {
        return None;
    }
    let mut renamed = Fields::new();
    for (key, value) in obj.iter() {
        match camel_case_rename(key) {
            Some(camel) => renamed.set(camel, value.clone()),
            None if !renamed.has(key) => renamed.push(key, value.clone()),
            None => {}
        }
    }
    Some(renamed)
}

/// Rewrites a multi-type `type` array (`["string", "array"]`) as one `anyOf`
/// branch per non-null type, each keeping only the keywords that constrain its
/// type; a `null` member becomes `nullable`. `None` when the object has fewer
/// than two non-null types or already carries an `anyOf`.
fn split_type_array(obj: &Fields) -> Option<Fields> {
    let listed = obj.array("type")?;
    if obj.array("anyOf").is_some() {
        return None;
    }
    let mut types: Vec<&str> = Vec::new();
    for name in listed.iter().filter_map(Node::text) {
        if name != "null" && !types.contains(&name) {
            types.push(name);
        }
    }
    if types.len() < 2 {
        return None;
    }
    let mut branches: Vec<Fields> = types
        .iter()
        .map(|name| {
            let mut branch = Fields::new();
            branch.push("type", Node::string(name));
            branch
        })
        .collect();
    let mut split = Fields::new();
    for (key, value) in obj.iter() {
        if key == "type" {
            continue;
        }
        if !is_type_specific(key) {
            split.set(key, value.clone());
            continue;
        }
        for (branch, name) in branches.iter_mut().zip(&types) {
            if type_specific_keys(name).is_some_and(|keys| keys.contains(&key)) {
                branch.set(key, value.clone());
            }
        }
    }
    if includes_text(listed, "null") {
        split.set("nullable", Node::Bool(true));
    }
    let variants = branches.into_iter().map(Node::Object).collect();
    split.set("anyOf", Node::Array(variants));
    Some(split)
}

/// Widens a `type: "null"` or an `anyOf` null branch into `nullable: true`:
/// the null branch goes, and a lone object branch merges into the node. `None`
/// when the object has neither.
fn collapse_null_branch(obj: &Fields) -> Option<Fields> {
    if obj.is_text("type", "null") {
        let mut widened = obj.without("type");
        widened.set("nullable", Node::Bool(true));
        return Some(widened);
    }
    let variants = obj.array("anyOf")?;
    let kept: Vec<&Node> = variants
        .iter()
        .filter(|variant| {
            !variant
                .object()
                .is_some_and(|fields| fields.is_text("type", "null"))
        })
        .collect();
    if kept.len() == variants.len() {
        return None;
    }
    let mut widened = obj.clone();
    widened.set("nullable", Node::Bool(true));
    if let [only] = kept.as_slice()
        && let Some(fields) = only.object()
    {
        widened.remove("anyOf");
        for (key, value) in fields.iter() {
            if !widened.has(key) {
                widened.push(key, value.clone());
            }
        }
    } else if kept.is_empty() {
        widened.remove("anyOf");
    } else {
        let remaining = kept.into_iter().cloned().collect();
        widened.set("anyOf", Node::Array(remaining));
    }
    Some(widened)
}

/// Whether every branch of a combiner is a `const` schema.
fn is_const_union(variants: &[Node]) -> bool {
    !variants.is_empty()
        && variants.iter().all(|variant| {
            variant
                .object()
                .is_some_and(|fields| fields.has("const"))
        })
}

/// The one type every entry names, when they agree.
fn single_type<'a>(types: &[&'a str]) -> Option<&'a str> {
    let first = *types.first()?;
    types.iter().all(|name| *name == first).then_some(first)
}

/// The JSON type every value shares, when they agree.
fn common_json_type(values: &[Node]) -> Option<&'static str> {
    let first = values.first()?.json_type()?;
    values
        .iter()
        .all(|value| value.json_type() == Some(first))
        .then_some(first)
}

/// Google's `enum` field takes strings only: any other enum goes, and the
/// node keeps its `type`.
fn drop_non_string_enum(schema: Fields) -> Fields {
    let Some(members) = schema.array("enum") else {
        return schema;
    };
    if !members.is_empty() && members.iter().all(|member| member.text().is_some()) {
        return schema;
    }
    schema.without("enum")
}

/// Whether a `not` subschema constrains an enum or a const to something other
/// than strings, which the modern wire cannot state.
fn unrepresentable_enum(value: &Node, slot: ChildKind, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match value {
        Node::Array(items) => items
            .iter()
            .any(|item| unrepresentable_enum(item, ChildKind::Schema, depth + 1)),
        Node::Object(fields) if slot == ChildKind::Map => fields
            .iter()
            .any(|(_, member)| unrepresentable_enum(member, ChildKind::Schema, depth + 1)),
        Node::Object(fields) => {
            let bad_enum = fields
                .array("enum")
                .is_some_and(|members| {
                    members.is_empty() || members.iter().any(|member| member.text().is_none())
                });
            let bad_const =
                fields.has("const") && fields.get("const").and_then(Node::text).is_none();
            bad_enum
                || bad_const
                || fields.iter().any(|(key, entry)| {
                    classify_schema_child(key, entry, ChildKind::Schema)
                        .is_some_and(|kind| unrepresentable_enum(entry, kind, depth + 1))
                })
        }
        _ => false,
    }
}

/// Lifts nullability out of property schemas: a property that accepts `null`
/// through `nullable`, a `["T", "null"]` type, or a null branch is no longer
/// required, and its schema keeps only `T`.
fn lift_nullable(value: &Node, slot: ChildKind, depth: usize) -> Node {
    match value {
        Node::Array(_) if depth > MAX_DEPTH => Node::Array(Vec::new()),
        Node::Array(items) => Node::Array(
            items
                .iter()
                .map(|item| lift_nullable(item, ChildKind::Schema, depth + 1))
                .collect(),
        ),
        Node::Object(_) if depth > MAX_DEPTH => Node::empty_object(),
        Node::Object(fields) => Node::Object(lift_fields(fields, slot, depth)),
        other => other.clone(),
    }
}

/// [`lift_nullable`] for the schema of one property, which also reports
/// whether the property accepts `null`.
fn lift_property(value: &Node, depth: usize) -> (Node, bool) {
    let Node::Object(fields) = value else {
        return (lift_nullable(value, ChildKind::Schema, depth), false);
    };
    if depth > MAX_DEPTH {
        return (Node::empty_object(), false);
    }
    let lifted = lift_fields(fields, ChildKind::Schema, depth);
    match extract_nullable_union(&lifted) {
        Some(extracted) => (Node::Object(extracted), true),
        None => (Node::Object(lifted), false),
    }
}

/// [`lift_nullable`] for the members of one object.
fn lift_fields(fields: &Fields, slot: ChildKind, depth: usize) -> Fields {
    let in_schema = slot == ChildKind::Schema;
    let mut nullable_names: Vec<&str> = Vec::new();
    let mut lifted = Fields::new();
    for (key, entry) in fields.iter() {
        let next = match entry {
            Node::Object(properties) if in_schema && key == "properties" => {
                lift_properties(properties, depth, &mut nullable_names)
            }
            _ => match classify_schema_child(key, entry, slot) {
                Some(kind) => lift_nullable(entry, kind, depth + 1),
                None => entry.clone(),
            },
        };
        lifted.set(key, next);
    }
    if in_schema {
        drop_nullable_required(&mut lifted, &nullable_names);
    }
    lifted
}

/// Lifts every property schema of one `properties` map and records the names
/// of the nullable ones.
fn lift_properties<'a>(
    properties: &'a Fields,
    depth: usize,
    nullable_names: &mut Vec<&'a str>,
) -> Node {
    let mut lifted = Fields::new();
    for (name, property) in properties.iter() {
        let (schema, nullable) = lift_property(property, depth + 2);
        if nullable {
            nullable_names.push(name);
        }
        lifted.set(name, schema);
    }
    Node::Object(lifted)
}

/// Rewrites `required` to the distinct names it lists that are not nullable.
/// A node without `properties` or without a `required` list is untouched.
fn drop_nullable_required(schema: &mut Fields, nullable_names: &[&str]) {
    if schema.object("properties").is_none() {
        return;
    }
    let Some(listed) = schema.array("required") else {
        return;
    };
    let mut kept: Vec<Node> = Vec::new();
    for name in listed.iter().filter_map(Node::text) {
        let seen = kept.iter().any(|entry| entry.text() == Some(name));
        if !seen && !nullable_names.contains(&name) {
            kept.push(Node::string(name));
        }
    }
    schema.set("required", Node::Array(kept));
}

/// The schema without its null alternative, when it has exactly one.
fn extract_nullable_union(fields: &Fields) -> Option<Fields> {
    if matches!(fields.get("nullable"), Some(Node::Bool(true))) {
        return Some(fields.without("nullable"));
    }
    if let Some(scalar) = scalar_without_null(fields) {
        return Some(scalar);
    }
    for combiner in JSON_SCHEMA_COMBINERS {
        let Some(variants) = fields.array(combiner) else {
            continue;
        };
        let Some(only) = sole_non_null_branch(variants) else {
            continue;
        };
        return merge_branch(fields, combiner, only);
    }
    None
}

/// `{type: ["T", "null"]}` as `{type: "T"}`; `None` for any other `type`.
fn scalar_without_null(fields: &Fields) -> Option<Fields> {
    let names: Vec<&str> = fields.array("type")?.iter().filter_map(Node::text).collect();
    let mut others = names.iter().filter(|name| **name != "null");
    let only = others.next()?;
    if others.next().is_some() || !names.contains(&"null") {
        return None;
    }
    let mut scalar = fields.clone();
    scalar.set("type", Node::string(only));
    Some(scalar)
}

/// The one non-null branch of a combiner that also has a bare `{type: "null"}`
/// branch.
fn sole_non_null_branch(variants: &[Node]) -> Option<&Fields> {
    let (nulls, others): (Vec<&Node>, Vec<&Node>) = variants.iter().partition(|variant| {
        variant
            .object()
            .is_some_and(|fields| fields.len() == 1 && fields.is_text("type", "null"))
    });
    match (nulls.is_empty(), others.as_slice()) {
        (false, [only]) => (*only).object(),
        _ => None,
    }
}

/// The schema with `combiner` replaced by the members of `only`; `None` when a
/// member disagrees with one the schema already has.
fn merge_branch(fields: &Fields, combiner: &str, only: &Fields) -> Option<Fields> {
    let mut merged = fields.without(combiner);
    for (key, value) in only.iter() {
        match merged.get(key) {
            Some(existing) if !json_equal(existing, value) => return None,
            Some(_) => {}
            None => merged.set(key, value.clone()),
        }
    }
    Some(merged)
}

/// Whether a keyword the legacy wire rejects is still in the schema: a `type`
/// array, a null type, `nullable`, `not`, or a combiner.
fn has_residual(value: &Node, slot: ChildKind, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match value {
        Node::Array(items) => items
            .iter()
            .any(|item| has_residual(item, ChildKind::Schema, depth + 1)),
        Node::Object(fields) => {
            let rejected = slot == ChildKind::Schema
                && (fields.array("type").is_some()
                    || fields.is_text("type", "null")
                    || fields.has("nullable")
                    || fields.has("not")
                    || SCHEMA_COMPOSITION_COMBINERS
                        .iter()
                        .any(|combiner| fields.array(combiner).is_some()));
            rejected
                || fields.iter().any(|(key, entry)| {
                    classify_schema_child(key, entry, slot)
                        .is_some_and(|kind| has_residual(entry, kind, depth + 1))
                })
        }
        _ => false,
    }
}
