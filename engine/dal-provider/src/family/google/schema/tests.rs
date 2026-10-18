//! Witnesses for the Google tool-schema projection: each case feeds a raw
//! schema through [`project`] and pins the exact text the wire would carry.

use std::borrow::Cow;

use dal_core::RawJson;

use super::{Spelling, project};

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).unwrap()
}

fn modern(schema: &str) -> String {
    project(&raw(schema), Spelling::Modern).as_str().to_owned()
}

fn legacy(schema: &str) -> String {
    project(&raw(schema), Spelling::Legacy).as_str().to_owned()
}

#[test]
fn unsupported_keywords_drop_and_readable_constraints_spill() {
    assert_eq!(
        modern(
            r#"{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","properties":{"path":{"type":"string","minLength":1}},"required":["path"],"additionalProperties":false}"#
        ),
        r#"{"type":"object","properties":{"path":{"type":"string","description":"{minLength: 1}"}},"required":["path"]}"#
    );
}

#[test]
fn null_types_widen_into_nullable() {
    assert_eq!(
        modern(
            r#"{"type":"object","properties":{"a":{"type":["string","null"]},"b":{"anyOf":[{"type":"string"},{"type":"null"}]},"c":{"type":"null"}}}"#
        ),
        r#"{"type":"object","properties":{"a":{"type":"string","nullable":true},"b":{"nullable":true,"type":"string"},"c":{"nullable":true}},"propertyOrdering":["a","b","c"]}"#
    );
}

#[test]
fn const_unions_and_enums_keep_only_string_members() {
    assert_eq!(
        modern(
            r#"{"type":"object","properties":{"mode":{"anyOf":[{"const":"fast"},{"const":"slow"}]},"level":{"const":3},"tags":{"enum":["a",1]},"kind":{"enum":["x","y"]}}}"#
        ),
        r#"{"type":"object","properties":{"mode":{"enum":["fast","slow"],"type":"string"},"level":{"type":"number"},"tags":{},"kind":{"enum":["x","y"],"type":"string"}},"propertyOrdering":["mode","level","tags","kind"]}"#
    );
}

#[test]
fn local_references_inline_and_cycles_end_in_an_empty_schema() {
    assert_eq!(
        modern(
            r##"{"type":"object","properties":{"item":{"$ref":"#/$defs/Item"},"tree":{"$ref":"#/$defs/Tree"}},"$defs":{"Item":{"type":"string","description":"An item"},"Tree":{"type":"object","properties":{"child":{"$ref":"#/$defs/Tree"}}}}}"##
        ),
        r#"{"type":"object","properties":{"item":{"type":"string","description":"An item"},"tree":{"type":"object","properties":{"child":{}}}},"propertyOrdering":["item","tree"]}"#
    );
}

#[test]
fn draft_seven_definitions_upgrade_before_they_inline() {
    assert_eq!(
        modern(
            r##"{"type":"object","properties":{"a":{"$ref":"#/definitions/A"}},"definitions":{"A":{"type":"integer","minimum":0}}}"##
        ),
        r#"{"type":"object","properties":{"a":{"type":"integer","description":"{minimum: 0}"}}}"#
    );
}

#[test]
fn boolean_subschemas_coerce_but_property_names_stay_names() {
    // A property literally named `const` or `nullable` is a property, and a
    // bare boolean under `properties` is a subschema.
    assert_eq!(
        modern(
            r#"{"type":"object","properties":{"const":{"type":"string"},"nullable":{"type":"boolean"},"flag":true,"never":false}}"#
        ),
        r#"{"type":"object","properties":{"const":{"type":"string"},"nullable":{"type":"boolean"},"flag":{},"never":{"not":{}}},"propertyOrdering":["const","nullable","flag","never"]}"#
    );
}

#[test]
fn snake_case_keywords_rename_and_type_arrays_split_by_type() {
    assert_eq!(
        modern(
            r#"{"type":"object","properties":{"a":{"any_of":[{"type":"string"},{"type":"integer"}]}}}"#
        ),
        r#"{"type":"object","properties":{"a":{"anyOf":[{"type":"string"},{"type":"integer"}]}}}"#
    );
    assert_eq!(
        modern(
            r#"{"type":"object","properties":{"v":{"type":["string","array"],"items":{"type":"string"},"minLength":2}}}"#
        ),
        r#"{"type":"object","properties":{"v":{"anyOf":[{"type":"string","description":"{minLength: 2}"},{"type":"array","items":{"type":"string"}}]}}}"#
    );
}

#[test]
fn legacy_spelling_scalarizes_types_and_keeps_required() {
    // `nullable` is a keyword on a schema and a name under `properties`; the
    // legacy wire takes neither a null type nor the keyword.
    assert_eq!(
        legacy(
            r#"{"type":"object","properties":{"nullable":{"type":"boolean"},"value":{"type":["string","null"],"nullable":true}},"required":["value"]}"#
        ),
        r#"{"type":"object","properties":{"nullable":{"type":"boolean"},"value":{"type":"string"}},"required":["value"]}"#
    );
}

#[test]
fn legacy_spelling_collapses_mixed_type_unions() {
    assert_eq!(
        legacy(
            r#"{"type":"object","properties":{"lines":{"anyOf":[{"type":"string","description":"one"},{"type":"null"}]}}}"#
        ),
        r#"{"type":"object","properties":{"lines":{"type":"string","description":"one"}}}"#
    );
}

#[test]
fn legacy_spelling_falls_back_when_a_rejected_keyword_survives() {
    assert_eq!(
        legacy(r#"{"type":"object","properties":{"x":{"not":{"type":"string"}}}}"#),
        r#"{"type":"object","properties":{}}"#
    );
}

#[test]
fn definitions_that_would_inline_past_the_budget_pass_through_unprojected() {
    // Every level references the next one twice, so inlining doubles the
    // tree per level: 2^25 objects, far past the budget.
    let mut defs = Vec::new();
    for level in 0..25 {
        let next = level + 1;
        defs.push(format!(
            r##""L{level}":{{"type":"object","properties":{{"a":{{"$ref":"#/$defs/L{next}"}},"b":{{"$ref":"#/$defs/L{next}"}}}}}}"##
        ));
    }
    defs.push(String::from(r#""L25":{"type":"string"}"#));
    let schema = format!(
        r##"{{"type":"object","properties":{{"x":{{"$ref":"#/$defs/L0"}}}},"$defs":{{{}}}}}"##,
        defs.join(",")
    );
    let parameters = raw(&schema);
    let projected = project(&parameters, Spelling::Modern);
    assert!(matches!(projected, Cow::Borrowed(_)));
    assert_eq!(projected.as_str(), schema);
}
