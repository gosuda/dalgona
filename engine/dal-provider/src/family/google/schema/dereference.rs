//! Reference inlining: oh-my-pi `dereference.ts` `dereferenceJsonSchema`.
//!
//! Every local `#/$defs/X` or `#/definitions/X` reference is replaced by its
//! definition, so no consumer needs a resolver. A reference back into itself
//! becomes `{}`, siblings of a `$ref` merge over the inlined definition, an
//! external or unresolvable reference is left in place, and `$defs` and
//! `definitions` disappear from the tree. Only schema-valued keywords are
//! descended into; literal JSON such as `default` is opaque.
//!
//! Inlining copies a shared definition at every use, so a hostile schema whose
//! definitions reference each other twice per level would grow exponentially.
//! The walk therefore owns an object budget and gives up when it runs out; the
//! caller then sends the schema unprojected.

use std::borrow::Cow;

use super::ref_pointer::parse_local_ref_pointer;
use super::tree::{Fields, MAX_DEPTH, Node};

/// Keywords whose object value maps names to schemas.
const SCHEMA_MAP_KEYS: [&str; 4] = [
    "properties",
    "patternProperties",
    "dependencies",
    "dependentSchemas",
];

/// Keywords whose array value lists schemas (`items` joins them when it is
/// an array).
const SCHEMA_ARRAY_KEYS: [&str; 4] = ["anyOf", "oneOf", "allOf", "prefixItems"];

/// Keywords whose value is one schema.
const SCHEMA_VALUE_KEYS: [&str; 12] = [
    "items",
    "additionalItems",
    "contains",
    "contentSchema",
    "propertyNames",
    "if",
    "then",
    "else",
    "not",
    "additionalProperties",
    "unevaluatedItems",
    "unevaluatedProperties",
];

/// The references being inlined above the current node, innermost last.
type Visiting = Vec<String>;

/// The most objects one inlining walk may emit; real tool schemas stay orders
/// of magnitude below it.
const MAX_INLINED_OBJECTS: usize = 50_000;

/// The schema a walk resolves references against, and the walk's state.
struct Inliner<'a> {
    /// The root schema whose `$defs` and `definitions` hold the targets.
    root: &'a Fields,
    /// The references being inlined above the current node.
    visiting: Visiting,
    /// How many more objects the walk may emit.
    budget: usize,
    /// Whether the budget ran out.
    exhausted: bool,
}

/// Inlines the local references of an object schema that has a root `$defs`
/// or `definitions`; any other schema is returned as it is. `None` when the
/// inlined tree would exceed the object budget.
pub(super) fn dereference(schema: &Node) -> Option<Cow<'_, Node>> {
    let Node::Object(root) = schema else {
        return Some(Cow::Borrowed(schema));
    };
    if root.get("$defs").is_none() && root.get("definitions").is_none() {
        return Some(Cow::Borrowed(schema));
    }
    let mut inliner = Inliner {
        root,
        visiting: Visiting::new(),
        budget: MAX_INLINED_OBJECTS,
        exhausted: false,
    };
    let fields = inliner.fields(root, 0);
    (!inliner.exhausted).then_some(Cow::Owned(fields))
}

impl Inliner<'_> {
    /// Inlines the references of one keyword value.
    fn keyword(&mut self, key: &str, value: &Node, depth: usize) -> Node {
        match value {
            Node::Object(map) if SCHEMA_MAP_KEYS.contains(&key) => {
                let mut result = Fields::new();
                for (name, entry) in map.iter() {
                    result.push(name, self.node(entry, depth + 2));
                }
                Node::Object(result)
            }
            Node::Array(items) if SCHEMA_ARRAY_KEYS.contains(&key) || key == "items" => {
                Node::Array(items.iter().map(|item| self.node(item, depth + 2)).collect())
            }
            _ if SCHEMA_VALUE_KEYS.contains(&key) => self.node(value, depth + 1),
            _ => value.clone(),
        }
    }

    /// Inlines every local reference at and below `node`.
    fn node(&mut self, node: &Node, depth: usize) -> Node {
        match node {
            Node::Object(fields) => self.fields(fields, depth),
            other => other.clone(),
        }
    }

    /// Inlines every local reference in one schema object.
    fn fields(&mut self, fields: &Fields, depth: usize) -> Node {
        if depth > MAX_DEPTH {
            return Node::empty_object();
        }
        let Some(left) = self.budget.checked_sub(1) else {
            self.exhausted = true;
            return Node::empty_object();
        };
        self.budget = left;
        if let Some(Node::Text(reference)) = fields.get("$ref") {
            return self.reference(fields, reference, depth);
        }
        let mut result = Fields::new();
        for (key, value) in fields.iter() {
            // `$defs` and `definitions` are inlined into their consumers.
            if key != "$defs" && key != "definitions" {
                result.push(key, self.keyword(key, value, depth));
            }
        }
        Node::Object(result)
    }

    /// Inlines one `$ref` node: breaks a cycle with `{}`, leaves an
    /// unresolvable reference as it is, and merges the node's sibling keywords
    /// over the inlined definition.
    fn reference(&mut self, fields: &Fields, reference: &str, depth: usize) -> Node {
        if self.visiting.iter().any(|active| active == reference) {
            return Node::empty_object();
        }
        let Some(resolved) = resolve_local_ref(reference, self.root) else {
            return Node::Object(fields.clone());
        };
        self.visiting.push(reference.to_owned());
        let inlined = self.fields(resolved, depth + 1);
        self.visiting.pop();

        let mut siblings = fields
            .iter()
            .filter(|(key, _)| !is_reference_bookkeeping(key))
            .peekable();
        if siblings.peek().is_none() {
            return inlined;
        }
        match inlined {
            Node::Object(mut merged) => {
                for (key, value) in siblings {
                    merged.set(key, self.keyword(key, value, depth));
                }
                Node::Object(merged)
            }
            other => other,
        }
    }
}

/// Resolves `#/$defs/<name>` and `#/definitions/<name>` against the root
/// schema; `None` for an external or unresolvable reference.
fn resolve_local_ref<'a>(reference: &str, root: &'a Fields) -> Option<&'a Fields> {
    let pointer = parse_local_ref_pointer(reference)?;
    let [defs_key, name] = pointer.as_slice() else {
        return None;
    };
    if defs_key != "$defs" && defs_key != "definitions" {
        return None;
    }
    root.object(defs_key)?.object(name)
}

/// Whether `key` stays out of the merged siblings of a `$ref`.
fn is_reference_bookkeeping(key: &str) -> bool {
    matches!(key, "$ref" | "$defs" | "definitions")
}
