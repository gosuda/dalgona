//! A closed-subset JSON Schema checker over the pinned Codex fixture.

use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Value};

use crate::codex::schema::fixture;

/// Validates `value` against `schema`, resolving `$ref` against the fixture.
pub(super) fn check(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(allowed) = schema.as_bool() {
        return if allowed {
            Ok(())
        } else {
            Err(format!("{path}: schema false"))
        };
    }
    if let Some(reference) = schema.get("$ref").and_then(|value| value.as_str()) {
        check(resolve(reference), value, path)?;
    }
    if let Some(all) = schema.get("allOf").and_then(|value| value.as_array()) {
        for branch in all {
            check(branch, value, path)?;
        }
    }
    if let Some(any) = schema.get("anyOf").and_then(|value| value.as_array())
        && !any.iter().any(|branch| check(branch, value, path).is_ok())
    {
        return Err(format!("{path}: no anyOf branch matches {value}"));
    }
    if let Some(one) = schema.get("oneOf").and_then(|value| value.as_array()) {
        let matched = one
            .iter()
            .filter(|branch| check(branch, value, path).is_ok())
            .count();
        if matched != 1 {
            return Err(format!("{path}: {matched} oneOf branches match {value}"));
        }
    }
    if let Some(kind) = schema.get("type") {
        let kinds: Vec<&str> = match kind.as_str() {
            Some(kind) => vec![kind],
            None => kind
                .as_array()
                .map(|kinds| kinds.iter().filter_map(|value| value.as_str()).collect())
                .unwrap_or_default(),
        };
        if !kinds.iter().any(|kind| has_type(value, kind)) {
            return Err(format!("{path}: {value} is not {kinds:?}"));
        }
    }
    if let Some(options) = schema.get("enum").and_then(|value| value.as_array())
        && !options.iter().any(|option| option == value)
    {
        return Err(format!("{path}: {value} not in enum"));
    }
    if let Some(constant) = schema.get("const")
        && constant != value
    {
        return Err(format!("{path}: {value} is not const {constant}"));
    }
    if let Some(minimum) = schema
        .get("minimum")
        .and_then(sonic_rs::JsonValueTrait::as_f64)
        && value.as_f64().is_some_and(|number| number < minimum)
    {
        return Err(format!("{path}: {value} below minimum {minimum}"));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(|value| value.as_array()) {
            for name in required.iter().filter_map(|value| value.as_str()) {
                if value.get(name).is_none() {
                    return Err(format!("{path}: missing required {name}"));
                }
            }
        }
        let properties = schema.get("properties");
        for (name, member) in object {
            let child = format!("{path}.{name}");
            match properties.and_then(|properties| properties.get(name)) {
                Some(property) => check(property, member, &child)?,
                None => match schema.get("additionalProperties") {
                    Some(extra) if extra.as_bool() == Some(false) => {
                        return Err(format!("{child}: additional property"));
                    }
                    Some(extra) if extra.is_object() => check(extra, member, &child)?,
                    _ => {}
                },
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (index, item) in array.iter().enumerate() {
            check(items, item, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

pub(super) fn schema() -> &'static Value {
    fixture().expect("the embedded fixture parses")
}

fn has_type(value: &Value, kind: &str) -> bool {
    match kind {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_str(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        _ => false,
    }
}

fn resolve(reference: &str) -> &'static Value {
    let path = reference.strip_prefix("#/").expect("local reference");
    path.split('/').fold(schema(), |node, segment| {
        node.get(segment)
            .unwrap_or_else(|| panic!("unresolved reference {reference}"))
    })
}

pub(super) fn schema_of(table: &str, method: &str, part: &str) -> &'static Value {
    schema()
        .get(table)
        .and_then(|table| table.get(method))
        .and_then(|entry| entry.get(part))
        .unwrap_or_else(|| panic!("fixture has no {table}.{method}.{part}"))
}

pub(super) fn assert_valid(table: &str, method: &str, part: &str, value: &Value) {
    if let Err(error) = check(schema_of(table, method, part), value, method) {
        panic!("{method} {part} fails the pinned schema: {error}\n{value}");
    }
}

#[test]
fn production_encoders_fail_closed_through_pinned_schema() {
    use dal_core::{JournalPart, SessionId, TurnId};

    use crate::codex::ask::server_request;
    use crate::codex::items::TurnStream;

    // A live `TurnStream` emits `item/started` params that pass the pinned
    // schema while intact and fail it once a required field is stripped, so
    // an encoder that drops `threadId` cannot stay green.
    let thread = SessionId::new_v7().to_string();
    let turn = TurnId::new(super::nonzero(3));
    let mut stream = TurnStream::new(thread.clone(), turn);
    let kinds = [super::entries(vec![super::entry(
        1,
        super::EntryKind::User {
            parts: vec![JournalPart::Text { text: "hi".into() }],
        },
    )])];
    let events = super::notifications(&mut stream, &kinds);
    let (_, params) = events
        .iter()
        .find(|(method, _)| *method == "item/started")
        .expect("a user entry starts an item");
    assert_valid("serverNotifications", "item/started", "params", params);
    let mut stripped = params.clone();
    stripped
        .as_object_mut()
        .expect("params is an object")
        .remove(&"threadId");
    let schema = schema_of("serverNotifications", "item/started", "params");
    assert!(
        check(schema, &stripped, "item/started").is_err(),
        "stripped item/started accepted {stripped}"
    );

    // A live `server_request` emits approval params with the same boundary:
    // intact passes, stripped `threadId` fails.
    let request = super::request(super::questions().remove(0));
    let (method, params) = server_request(&thread, turn, std::path::Path::new("/w"), &request)
        .expect("approval maps to a server request");
    assert_valid("serverRequests", method, "params", &params);
    let mut stripped = params.clone();
    stripped
        .as_object_mut()
        .expect("params is an object")
        .remove(&"threadId");
    let schema = schema_of("serverRequests", method, "params");
    assert!(
        check(schema, &stripped, method).is_err(),
        "stripped {method} accepted {stripped}"
    );
}
