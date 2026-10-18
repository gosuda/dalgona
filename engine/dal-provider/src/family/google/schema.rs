//! Google tool-schema projection.
//!
//! The Google wire takes a smaller JSON Schema than tools declare: no `$ref`,
//! no validation keywords, no combiner on one spelling, `nullable` instead of a
//! null type. [`project`] rewrites one tool's raw schema onto that subset the
//! way oh-my-pi's `normalizeSchemaForGoogle` (modern spelling) and
//! `normalizeSchemaForCCA` (legacy spelling) do, and spills a dropped
//! constraint into the node's description so the model still reads it.
//!
//! A schema the projection cannot read (invalid text, nesting beyond 128
//! levels, or definitions that would inline past the object budget) is
//! returned as it is; the wire then reports it.

use std::borrow::Cow;

use dal_core::RawJson;

use self::tree::Node;

mod combiners;
mod dereference;
mod draft;
mod equality;
mod fields;
mod normalize;
mod ref_pointer;
mod spill;
mod tree;
mod validate;

#[cfg(test)]
mod tests;

/// Which Google wire spelling a tool schema is projected for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Spelling {
    /// `parametersJsonSchema`: oh-my-pi `normalizeSchemaForGoogle`.
    Modern,
    /// Legacy `parameters` (Claude behind Cloud Code Assist): oh-my-pi
    /// `normalizeSchemaForCCA`.
    Legacy,
}

/// Projects one tool's raw JSON Schema onto the subset the Google wire
/// accepts.
#[must_use]
pub(crate) fn project(parameters: &RawJson, spelling: Spelling) -> Cow<'_, RawJson> {
    let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(parameters.as_str()) else {
        return Cow::Borrowed(parameters);
    };
    let Some(tree) = Node::from_value(&value, 0) else {
        return Cow::Borrowed(parameters);
    };
    let Some(projected) = normalize::normalize(&tree, spelling) else {
        return Cow::Borrowed(parameters);
    };
    match sonic_rs::to_string(&projected)
        .ok()
        .and_then(|text| RawJson::parse(&text).ok())
    {
        Some(raw) => Cow::Owned(raw),
        None => Cow::Borrowed(parameters),
    }
}
