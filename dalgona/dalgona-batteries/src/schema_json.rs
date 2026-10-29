// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! JSON text for the tool parameter schemas that `schemars` derives.
//!
//! `serde_json` spells a number as a one-field struct when its
//! `arbitrary_precision` feature is on. This workspace enables that feature
//! through `starlark`, and `sonic_rs` writes the struct as an object, so a
//! numeric keyword would leave as an object and the schema would be invalid.
//! The walk here restores every such struct to the number it carries.

use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Object, Value};

/// The field name `serde_json` gives a number under `arbitrary_precision`.
const ARBITRARY_PRECISION: &str = "$serde_json::private::Number";

/// Encodes `schema` as JSON text with every number written as a number.
///
/// Returns `None` when the schema cannot be encoded or when a number spelling
/// survives the walk, so no caller sends a schema a host may reject.
pub(crate) fn encode(schema: &schemars::Schema) -> Option<String> {
    let mut value = sonic_rs::to_value(schema).ok()?;
    rewrite(&mut value);
    let text = sonic_rs::to_string(&value).ok()?;
    (!text.contains(ARBITRARY_PRECISION)).then_some(text)
}

/// Restores every arbitrary-precision number below `value` to a number.
fn rewrite(value: &mut Value) -> bool {
    let replacement = value.as_object().and_then(carried_number);
    if let Some(number) = replacement {
        *value = number;
        return true;
    }
    if let Some(object) = value.as_object_mut() {
        return object.iter_mut().all(|(_, child)| rewrite(child));
    }
    if let Some(items) = value.as_array_mut() {
        return items.iter_mut().all(rewrite);
    }
    true
}

/// The number one arbitrary-precision struct carries, when it carries one.
fn carried_number(object: &Object) -> Option<Value> {
    if object.len() != 1 {
        return None;
    }
    let spelled = object.get(&ARBITRARY_PRECISION)?.as_str()?;
    parse_number(spelled)
}

/// Parses one number keeping every digit, so a bound wider than `f64` keeps
/// its exact text.
fn parse_number(text: &str) -> Option<Value> {
    let mut deserializer = sonic_rs::Deserializer::from_str(text).use_rawnumber();
    let value: Value = deserializer.deserialize().ok()?;
    deserializer.end().ok()?;
    value.is_number().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[derive(schemars::JsonSchema)]
    #[expect(
        dead_code,
        reason = "the derive reads the fields; the test reads the schema"
    )]
    struct Bounded {
        #[schemars(length(min = 1, max = 400))]
        query: String,
        #[schemars(range(min = 1, max = 20))]
        count: Option<u32>,
    }

    #[test]
    fn numeric_keywords_leave_as_numbers() {
        let text = encode(&schemars::schema_for!(Bounded)).expect("schema text");

        assert!(text.contains(r#""minLength":1"#), "{text}");
        assert!(text.contains(r#""maxLength":400"#), "{text}");
        assert!(text.contains(r#""minimum":1"#), "{text}");
        assert!(text.contains(r#""maximum":20"#), "{text}");
        assert!(!text.contains("$serde_json::private::Number"), "{text}");
    }
}
