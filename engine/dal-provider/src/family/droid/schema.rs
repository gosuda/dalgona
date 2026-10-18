//! The Factory Gemini tool-schema projection.
//!
//! The proxy's Gemini path takes the OpenAPI-style `Schema` proto, not full
//! JSON Schema, so a tool's `parameters` pass two steps before they ride a
//! `functionDeclarations` entry: local `$ref` / `$defs` are inlined
//! ([`dereference`]), then only the keywords the Droid CLI copies survive
//! ([`normalize`]). Ground truth: oh-my-pi `packages/ai/src/utils/schema/`
//! `dereference.ts` (`dereferenceJsonSchema`) and `allowlist.ts`
//! (`normalizeSchemaForFactoryDroid`).
//!
//! The tree keeps object members in document order and never builds a hash
//! map, so two projections of the same schema are equal byte for byte.

use dal_core::RawJson;
use serde::{Serialize, Serializer};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

/// The keywords the CLI copies through verbatim, in output order.
const ALLOWED_KEYS: [&str; 14] = [
    "type",
    "title",
    "description",
    "required",
    "format",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
    "default",
    "example",
];

/// One JSON value of a schema tree.
#[derive(Clone, Debug)]
pub(super) enum Node {
    /// A string; kept apart so enum members and types compare cheaply.
    Text(String),
    /// A boolean.
    Bool(bool),
    /// A number or `null`, kept as parsed.
    Scalar(Value),
    /// An array.
    Array(Vec<Node>),
    /// An object whose members keep their document order.
    Object(Fields),
}

/// The members of one object, in order.
#[derive(Clone, Debug, Default)]
pub(super) struct Fields(Vec<(String, Node)>);

impl Fields {
    fn get(&self, key: &str) -> Option<&Node> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, node)| node)
    }

    fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Replaces a member in place, or appends it.
    fn set(&mut self, key: &str, node: Node) {
        match self.0.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = node,
            None => self.0.push((key.to_owned(), node)),
        }
    }

    fn remove(&mut self, key: &str) {
        self.0.retain(|(name, _)| name != key);
    }
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text(text) => serializer.serialize_str(text),
            Self::Bool(flag) => serializer.serialize_bool(*flag),
            Self::Scalar(value) => value.serialize(serializer),
            Self::Array(items) => serializer.collect_seq(items),
            Self::Object(fields) => {
                serializer.collect_map(fields.0.iter().map(|(name, node)| (name, node)))
            }
        }
    }
}

impl Node {
    fn from_value(value: &Value) -> Self {
        if let Some(text) = value.as_str() {
            return Self::Text(text.to_owned());
        }
        if let Some(flag) = value.as_bool() {
            return Self::Bool(flag);
        }
        if let Some(items) = value.as_array() {
            return Self::Array(items.iter().map(Self::from_value).collect());
        }
        if let Some(map) = value.as_object() {
            let fields = map
                .iter()
                .map(|(name, member)| (name.to_owned(), Self::from_value(member)))
                .collect();
            return Self::Object(Fields(fields));
        }
        Self::Scalar(value.clone())
    }

    fn object(&self) -> Option<&Fields> {
        match self {
            Self::Object(fields) => Some(fields),
            _ => None,
        }
    }

    /// Strict equality for the leaf kinds a `required` or `enum` list holds;
    /// containers are never equal, as with a reference comparison.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(left), Self::Text(right)) => left == right,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Scalar(left), Self::Scalar(right)) => left == right,
            _ => false,
        }
    }
}

/// The projected `parameters` of one tool.
///
/// # Errors
///
/// The parse error when `parameters` is not JSON; the request path maps it
/// to an invalid-request error.
pub(super) fn project(parameters: &RawJson) -> Result<Node, sonic_rs::Error> {
    let parsed: Value = sonic_rs::from_str(parameters.as_str())?;
    let tree = Node::from_value(&parsed);
    let inlined = dereference(&tree);
    Ok(normalize(&inlined))
}

// --- $ref / $defs inlining ---------------------------------------------

/// Inlines every local `$ref` of an object schema and strips `$defs` and
/// `definitions`. A schema without definitions, and any non-object, is
/// returned unchanged; a circular reference becomes `{}`.
fn dereference(schema: &Node) -> Node {
    let Node::Object(root) = schema else {
        return schema.clone();
    };
    if !root.contains("$defs") && !root.contains("definitions") {
        return schema.clone();
    }
    deref_node(schema, root, &mut Vec::new())
}

/// Resolves `#/$defs/<name>` and `#/definitions/<name>` against the root.
/// External and unresolvable references give `None`.
fn resolve_local<'a>(reference: &str, root: &'a Fields) -> Option<&'a Node> {
    let rest = reference.strip_prefix("#/")?;
    let (section, name) = rest.split_once('/')?;
    if section != "$defs" && section != "definitions" {
        return None;
    }
    let resolved = root.get(section)?.object()?.get(name)?;
    resolved.object().map(|_| resolved)
}

fn deref_node(node: &Node, root: &Fields, visiting: &mut Vec<String>) -> Node {
    match node {
        Node::Array(items) => Node::Array(
            items
                .iter()
                .map(|item| deref_node(item, root, visiting))
                .collect(),
        ),
        Node::Object(fields) => deref_object(node, fields, root, visiting),
        Node::Text(_) | Node::Bool(_) | Node::Scalar(_) => node.clone(),
    }
}

fn deref_object(node: &Node, fields: &Fields, root: &Fields, visiting: &mut Vec<String>) -> Node {
    if let Some(Node::Text(reference)) = fields.get("$ref") {
        return deref_reference(node, fields, reference, root, visiting);
    }
    let mut result = Fields::default();
    for (name, member) in &fields.0 {
        if name == "$defs" || name == "definitions" {
            continue;
        }
        result
            .0
            .push((name.clone(), deref_node(member, root, visiting)));
    }
    Node::Object(result)
}

fn deref_reference(
    node: &Node,
    fields: &Fields,
    reference: &str,
    root: &Fields,
    visiting: &mut Vec<String>,
) -> Node {
    if visiting.iter().any(|open| open == reference) {
        return Node::Object(Fields::default());
    }
    let Some(resolved) = resolve_local(reference, root) else {
        return node.clone();
    };
    visiting.push(reference.to_owned());
    let inlined = deref_node(resolved, root, visiting);
    visiting.pop();
    let has_siblings = fields.0.iter().any(|(name, _)| name != "$ref");
    let Node::Object(inlined_fields) = &inlined else {
        return inlined;
    };
    if !has_siblings {
        return inlined;
    }
    // Draft 2020-12 allows siblings beside `$ref`; they override the target.
    let mut merged = inlined_fields.clone();
    for (name, member) in &fields.0 {
        if name != "$ref" {
            merged.set(name, member.clone());
        }
    }
    Node::Object(merged)
}

// --- allowlist projection ----------------------------------------------

/// How `required` combines when two schemas merge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequiredMerge {
    /// Conjunctions (`allOf`, a parent absorbing its collapsed union): every
    /// side's constraints hold.
    Union,
    /// Alternatives (`anyOf` / `oneOf` branches): a field is guaranteed only
    /// when every branch requires it.
    Intersection,
}

/// Projects a dereferenced schema onto the fields the proxy accepts. A
/// non-object passes through; an object that projects to nothing is `{}`.
fn normalize(schema: &Node) -> Node {
    match schema {
        Node::Object(_) => Node::Object(copy_schema(schema).unwrap_or_default()),
        other => other.clone(),
    }
}

/// A string member as the CLI writes it: strings stay, anything else is its
/// JSON text.
fn stringified(node: &Node) -> Node {
    match node {
        Node::Text(_) => node.clone(),
        other => Node::Text(sonic_rs::to_string(other).unwrap_or_default()),
    }
}

fn type_includes_null(fields: &Fields) -> bool {
    match fields.get("type") {
        Some(Node::Text(text)) => text == "null",
        Some(Node::Array(types)) => types
            .iter()
            .any(|entry| matches!(entry, Node::Text(text) if text == "null")),
        _ => false,
    }
}

fn dedupe(values: Vec<Node>) -> Vec<Node> {
    let mut unique: Vec<Node> = Vec::with_capacity(values.len());
    for value in values {
        if !unique.iter().any(|seen| seen.same(&value)) {
            unique.push(value);
        }
    }
    unique
}

fn array_items(node: Option<&Node>) -> Vec<Node> {
    match node {
        Some(Node::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

fn merge(left: &Fields, right: &Fields, required: RequiredMerge) -> Fields {
    let mut merged = left.clone();
    for (key, value) in &right.0 {
        match key.as_str() {
            "properties" => merge_properties(&mut merged, value, required),
            "required" => {}
            "enum" if merges_enums(left, value) => {
                let mut members = array_items(left.get("enum"));
                members.extend(array_items(Some(value)));
                merged.set("enum", Node::Array(dedupe(members)));
            }
            other if !merged.contains(other) => merged.set(other, value.clone()),
            _ => {}
        }
    }
    merge_required(&mut merged, left, right, required);
    merged
}

fn merges_enums(left: &Fields, right: &Node) -> bool {
    matches!(left.get("enum"), Some(Node::Array(_))) && matches!(right, Node::Array(_))
}

fn merge_properties(merged: &mut Fields, right: &Node, required: RequiredMerge) {
    let left_properties = match merged.get("properties") {
        Some(Node::Object(fields)) => fields.clone(),
        _ => Fields::default(),
    };
    let right_properties = right.object().cloned().unwrap_or_default();
    let mut properties = left_properties.clone();
    for (name, schema) in &right_properties.0 {
        let both = left_properties
            .get(name)
            .and_then(Node::object)
            .zip(schema.object());
        match both {
            Some((left_schema, right_schema)) => {
                properties.set(
                    name,
                    Node::Object(merge(left_schema, right_schema, required)),
                );
            }
            None => properties.set(name, schema.clone()),
        }
    }
    merged.set("properties", Node::Object(properties));
}

fn merge_required(merged: &mut Fields, left: &Fields, right: &Fields, required: RequiredMerge) {
    let left_required = array_items(left.get("required"));
    let right_required = array_items(right.get("required"));
    match required {
        RequiredMerge::Union => {
            let listed = matches!(left.get("required"), Some(Node::Array(_)))
                || matches!(right.get("required"), Some(Node::Array(_)));
            if listed {
                let mut all = left_required;
                all.extend(right_required);
                merged.set("required", Node::Array(dedupe(all)));
            }
        }
        RequiredMerge::Intersection => {
            let shared = dedupe(
                left_required
                    .into_iter()
                    .filter(|entry| right_required.iter().any(|other| other.same(entry)))
                    .collect(),
            );
            if shared.is_empty() {
                merged.remove("required");
            } else {
                merged.set("required", Node::Array(shared));
            }
        }
    }
}

/// Copies one schema node onto the allowed keywords, collapsing unions,
/// `allOf`, `const`, `enum`, and type arrays the way the CLI's copier does.
fn copy_schema(node: &Node) -> Option<Fields> {
    let source = node.object()?;
    let mut out = Fields::default();
    for key in ALLOWED_KEYS {
        if let Some(value) = source.get(key) {
            out.set(key, value.clone());
        }
    }
    if let Some(constant) = source.get("const") {
        out.set("enum", Node::Array(vec![stringified(constant)]));
    }
    if let Some(Node::Array(members)) = source.get("enum") {
        out.set(
            "enum",
            Node::Array(members.iter().map(stringified).collect()),
        );
    }
    copy_children(source, &mut out);
    collapse_union(source, &mut out);
    if let Some(Node::Array(branches)) = source.get("allOf") {
        for branch in branches {
            if let Some(copied) = copy_schema(branch) {
                out = merge(&out, &copied, RequiredMerge::Union);
            }
        }
    }
    collapse_type_array(&mut out);
    infer_type(&mut out);
    Some(out)
}

fn copy_children(source: &Fields, out: &mut Fields) {
    if let Some(Node::Object(properties)) = source.get("properties") {
        let mut copied = Fields::default();
        for (name, schema) in &properties.0 {
            if let Some(fields) = copy_schema(schema) {
                copied.set(name, Node::Object(fields));
            }
        }
        out.set("properties", Node::Object(copied));
    }
    if let Some(items) = source.get("items").filter(|items| items.object().is_some())
        && let Some(fields) = copy_schema(items)
    {
        out.set("items", Node::Object(fields));
    }
}

/// Merges the non-null branches of `anyOf` / `oneOf`, marking the result
/// nullable when a `type: "null"` branch exists, then folds it into `out`.
fn collapse_union(source: &Fields, out: &mut Fields) {
    let union = [source.get("anyOf"), source.get("oneOf")]
        .into_iter()
        .flatten()
        .find_map(|node| match node {
            Node::Array(branches) => Some(branches),
            _ => None,
        });
    let Some(raw) = union else {
        return;
    };
    let branches: Vec<Fields> = raw.iter().filter_map(copy_schema).collect();
    let non_null: Vec<&Fields> = branches
        .iter()
        .filter(|branch| !type_includes_null(branch))
        .collect();
    let mut collapsed: Option<Fields> = None;
    for branch in &non_null {
        collapsed = Some(match collapsed {
            Some(current) => merge(&current, branch, RequiredMerge::Intersection),
            None => (*branch).clone(),
        });
    }
    if let Some(mut collapsed) = collapsed {
        collapsed.set("nullable", Node::Bool(non_null.len() < branches.len()));
        *out = merge(out, &collapsed, RequiredMerge::Union);
    }
}

/// The Schema proto takes one string `type`: a null member becomes
/// `nullable: true` and the first non-null type wins.
fn collapse_type_array(out: &mut Fields) {
    let Some(Node::Array(entries)) = out.get("type") else {
        return;
    };
    let types: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Node::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    if types.iter().any(|text| text == "null") {
        out.set("nullable", Node::Bool(true));
    }
    let chosen = types
        .iter()
        .find(|text| text.as_str() != "null")
        .or_else(|| types.first());
    match chosen {
        Some(text) => out.set("type", Node::Text(text.clone())),
        None => out.remove("type"),
    }
}

/// The proxy requires a `type`; infer one when the source omitted it.
fn infer_type(out: &mut Fields) {
    if out.contains("type") {
        return;
    }
    let inferred = if matches!(out.get("properties"), Some(Node::Object(_))) {
        "object"
    } else if out.contains("items") {
        "array"
    } else if out.contains("enum") {
        "string"
    } else {
        return;
    };
    out.set("type", Node::Text(inferred.to_owned()));
}
