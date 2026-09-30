// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Saved workflows from the `[plugin.orchestration.workflows]` table.
//! Invalid entries are never a startup error; every failure carries the
//! exact model-facing text. Inner reasons are message fragments; the public
//! fallible type stays [`WorkflowError`].

use dal_core::RawJson;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::{Workflow, WorkflowError, decode::decode_steps};

#[cfg(test)]
mod tests;
/// Wraps one invalidation reason in the exact saved-workflow text. A
/// trailing period on the reason is folded into the template's own.
fn invalid(name: &str, reason: &str) -> WorkflowError {
    let reason = reason.trim_end_matches('.');
    WorkflowError::new(format!(
        "agents: saved workflow \"{name}\" is invalid: {reason}. Fix [plugin.orchestration.workflows.{name}] in config.toml."
    ))
}

/// Decodes the `steps` member of one saved workflow.
fn decode_member(steps: &Value, name: &str, input: Option<&str>) -> Result<Workflow, String> {
    let text = sonic_rs::to_string(steps).map_err(|_| "steps must hold 1 to 32 steps.")?;
    let raw = RawJson::parse(&text).map_err(|_| "steps must hold 1 to 32 steps.")?;
    decode_steps(&raw, input, name).map_err(|error| error.to_string())
}

/// Loads one table member or returns its invalidation reason.
fn load_member(name: &str, member: &Value, input: Option<&str>) -> Result<Workflow, String> {
    if member.as_object().is_none() {
        return Err("the workflow must be an object with steps.".to_owned());
    }
    match member.as_object().and_then(|object| object.get(&"description")) {
        None => {}
        Some(description) if description.is_null() => {}
        Some(description) => match description.as_str() {
            Some(text) if text.chars().count() <= 200 => {}
            _ => return Err("description must be a string of at most 200 characters.".to_owned()),
        },
    }
    let Some(steps) = member.as_object().and_then(|object| object.get(&"steps")) else {
        return Err("steps must hold 1 to 32 steps.".to_owned());
    };
    if input.is_none() {
        let text = sonic_rs::to_string(steps).unwrap_or_default();
        if text.contains("{{input}}") {
            return Err("the workflow needs input.".to_owned());
        }
    }
    decode_member(steps, name, input)
}

/// Sorted member names of the table; empty when the table is not an object.
fn table_names(table: &RawJson) -> Vec<String> {
    let Ok(value) = sonic_rs::from_str::<Value>(table.as_str()) else {
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut names: Vec<String> = object.iter().map(|(name, _)| name.to_owned()).collect();
    names.sort();
    names
}

/// Looks one member up by name.
fn table_member<'a>(table: &'a Value, name: &str) -> Option<&'a Value> {
    table.as_object()?.get(&name)
}

/// Lists the saved workflows sorted by name with the invalidation reason.
/// A workflow that only needs `input` lists as valid here; the input check
/// runs when the workflow is actually used.
pub(crate) fn saved_names(table: &RawJson) -> Vec<(String, Option<String>)> {
    let Ok(value) = sonic_rs::from_str::<Value>(table.as_str()) else {
        return Vec::new();
    };
    table_names(table)
        .into_iter()
        .map(|name| {
            let reason = table_member(&value, &name)
                .and_then(|member| load_member(&name, member, Some("")).err());
            (name, reason)
        })
        .collect()
}

/// Finds one saved workflow by name. The label of the decoded workflow is
/// the saved name.
pub(crate) fn find_saved(
    table: &RawJson,
    name: &str,
    input: Option<&str>,
) -> Result<Workflow, WorkflowError> {
    let parsed = sonic_rs::from_str::<Value>(table.as_str()).ok();
    let member = parsed.as_ref().and_then(|value| table_member(value, name));
    let Some(member) = member else {
        let names = table_names(table);
        let listed = if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        };
        return Err(WorkflowError::new(format!(
            "agents: no saved workflow \"{name}\". Saved workflows: {listed}."
        )));
    };
    if input.is_none() {
        let text = member
            .as_object()
            .and_then(|object| object.get(&"steps"))
            .and_then(|steps| sonic_rs::to_string(steps).ok())
            .unwrap_or_default();
        if text.contains("{{input}}") {
            return Err(WorkflowError::new(format!(
                "agents: saved workflow \"{name}\" needs input."
            )));
        }
    }
    load_member(name, member, input).map_err(|reason| invalid(name, &reason))
}
