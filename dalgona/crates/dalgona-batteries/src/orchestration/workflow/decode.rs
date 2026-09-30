// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Workflow validation: ordered checks, DAG order, placeholder rules.

use std::collections::{HashMap, HashSet};

use dal_core::RawJson;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::{
    FORBIDDEN_TOOLS, Isolation, Items, RawStep, STEP_FIELDS, Step, Workflow, WorkflowError,
};

struct DecodedOptions {
    isolation: Vec<Isolation>,
    model: Vec<Option<String>>,
    role: Vec<Option<String>>,
    system: Vec<Option<String>>,
}

/// Decodes and validates the raw `steps` member. Checks run in the carried
/// order, so the first user-facing error is stable.
pub(crate) fn decode_steps(
    steps: &RawJson,
    input: Option<&str>,
    label: &str,
) -> Result<Workflow, WorkflowError> {
    let raw = parse_steps(steps)?;
    let names = validate_names_and_prompts(&raw)?;
    let item_sources = decode_items(&raw)?;
    let workers = decode_workers(&raw, &item_sources)?;
    let (after_names, after) = decode_dependencies(&raw, &names)?;
    validate_item_sources(&raw, &item_sources, &after_names, &names)?;
    for (step, items) in raw.iter().zip(&item_sources) {
        validate_placeholders(step, items, &after_names, input)?;
    }
    let tools = decode_tools(&raw)?;
    let options = decode_options(&raw, &tools)?;

    let steps = raw
        .into_iter()
        .enumerate()
        .map(|(index, step)| Step {
            name: step.name,
            prompt: step.prompt,
            items: item_sources[index].clone(),
            workers: workers[index],
            after: after[index].clone(),
            tools: tools[index].clone(),
            model: options.model[index].clone(),
            role: options.role[index].clone(),
            system: options.system[index].clone(),
            isolation: options.isolation[index],
        })
        .collect();
    Ok(Workflow {
        label: label.to_owned(),
        steps,
    })
}

fn parse_steps(steps: &RawJson) -> Result<Vec<RawStep>, WorkflowError> {
    let raw_steps = sonic_rs::from_str::<Vec<RawJson>>(steps.as_str())
        .map_err(|_| WorkflowError::new("agents: steps must hold 1 to 32 steps."))?;
    if !(1..=32).contains(&raw_steps.len()) {
        return Err(WorkflowError::new("agents: steps must hold 1 to 32 steps."));
    }

    let mut raw = Vec::with_capacity(raw_steps.len());
    for (index, raw_step) in raw_steps.into_iter().enumerate() {
        let value = sonic_rs::from_str::<Value>(raw_step.as_str()).map_err(|_| {
            WorkflowError::new(format!(
                "agents: step {}: name and prompt are required.",
                index + 1
            ))
        })?;
        let Some(object) = value.as_object() else {
            return Err(WorkflowError::new(format!(
                "agents: step {}: name and prompt are required.",
                index + 1
            )));
        };
        let name_hint = object
            .get(&"name")
            .and_then(JsonValueTrait::as_str)
            .map_or_else(|| (index + 1).to_string(), str::to_owned);
        let mut seen = HashSet::with_capacity(STEP_FIELDS.len());
        for (field, _) in object {
            if !seen.insert(field) {
                return Err(WorkflowError::new(format!(
                    "agents: step {name_hint}: duplicate field \"{field}\"."
                )));
            }
            if !STEP_FIELDS.contains(&field) {
                return Err(WorkflowError::new(format!(
                    "agents: step {name_hint}: unknown field \"{field}\"."
                )));
            }
        }
        let has_name = object
            .get(&"name")
            .and_then(JsonValueTrait::as_str)
            .is_some();
        let has_prompt = object
            .get(&"prompt")
            .and_then(JsonValueTrait::as_str)
            .is_some();
        if !has_name || !has_prompt {
            return Err(WorkflowError::new(format!(
                "agents: step {}: name and prompt are required.",
                index + 1
            )));
        }

        let (Some(name), Some(prompt)) = (
            object.get(&"name").and_then(JsonValueTrait::as_str),
            object.get(&"prompt").and_then(JsonValueTrait::as_str),
        ) else {
            return Err(WorkflowError::new(format!(
                "agents: step {}: name and prompt are required.",
                index + 1
            )));
        };
        raw.push(RawStep {
            index,
            name: name.to_owned(),
            prompt: prompt.to_owned(),
            items: object.get(&"items").cloned(),
            items_from: object.get(&"items_from").cloned(),
            workers: object.get(&"workers").cloned(),
            after: object.get(&"after").cloned(),
            tools: object.get(&"tools").cloned(),
            model: object.get(&"model").cloned(),
            role: object.get(&"role").cloned(),
            system: object.get(&"system").cloned(),
            isolation: object.get(&"isolation").cloned(),
        });
    }
    Ok(raw)
}

fn validate_names_and_prompts(
    raw: &[RawStep],
) -> Result<HashMap<&str, usize>, WorkflowError> {
    let mut names = HashMap::with_capacity(raw.len());
    for step in raw {
        if !valid_name(&step.name) {
            return Err(WorkflowError::new(format!(
                "agents: step name \"{}\" must match [a-z][a-z0-9-]{{0,31}}.",
                step.name
            )));
        }
        if names.insert(step.name.as_str(), step.index).is_some() {
            return Err(WorkflowError::new(format!(
                "agents: step name \"{}\" is used twice.",
                step.name
            )));
        }
    }

    for step in raw {
        let length = step.prompt.chars().count();
        if !(1..=32_000).contains(&length) {
            return Err(WorkflowError::new(format!(
                "agents: step {}: prompt must be 1 to 32000 characters.",
                step.name
            )));
        }
    }
    Ok(names)
}

fn decode_items(raw: &[RawStep]) -> Result<Vec<Items>, WorkflowError> {
    let mut item_sources = Vec::with_capacity(raw.len());
    for step in raw {
        let items = match (&step.items, &step.items_from) {
            (Some(_), Some(_)) => {
                return Err(WorkflowError::new(format!(
                    "agents: step {}: items and items_from cannot both be set.",
                    step.name
                )));
            }
            (Some(value), None) => {
                let Some(array) = value.as_array() else {
                    return Err(items_error(&step.name));
                };
                if !(1..=1024).contains(&array.len()) {
                    return Err(items_error(&step.name));
                }
                let mut items = Vec::with_capacity(array.len());
                for item in array {
                    let Some(item) = item.as_str() else {
                        return Err(items_error(&step.name));
                    };
                    if !(1..=2000).contains(&item.chars().count()) {
                        return Err(items_error(&step.name));
                    }
                    items.push(item.to_owned());
                }
                Items::Literal(items)
            }
            (None, Some(value)) => {
                let Some(name) = value.as_str() else {
                    return Err(WorkflowError::new(format!(
                        "agents: step {}: items_from must name a step in after.",
                        step.name
                    )));
                };
                Items::From(name.to_owned())
            }
            (None, None) => Items::Task,
        };
        item_sources.push(items);
    }
    Ok(item_sources)
}

fn decode_workers(raw: &[RawStep], items: &[Items]) -> Result<Vec<u8>, WorkflowError> {
    let mut workers = Vec::with_capacity(raw.len());
    for (step, items) in raw.iter().zip(items) {
        if let Some(value) = &step.workers {
            if matches!(items, Items::Task) {
                return Err(WorkflowError::new(format!(
                    "agents: step {}: workers applies only to a pool.",
                    step.name
                )));
            }
            let Some(count) = value.as_u64() else {
                return Err(workers_error(&step.name));
            };
            let Ok(count) = u8::try_from(count) else {
                return Err(workers_error(&step.name));
            };
            if !(1..=64).contains(&count) {
                return Err(workers_error(&step.name));
            }
            workers.push(count);
        } else {
            workers.push(4);
        }
    }
    Ok(workers)
}

fn decode_dependencies(
    raw: &[RawStep],
    names: &HashMap<&str, usize>,
) -> Result<(Vec<Vec<String>>, Vec<Vec<usize>>), WorkflowError> {
    let mut after_names = Vec::with_capacity(raw.len());
    let mut after = Vec::with_capacity(raw.len());
    for step in raw {
        let mut resolved = Vec::new();
        let dependency_names = match &step.after {
            None => Vec::new(),
            Some(value) => {
                let Some(array) = value.as_array() else {
                    return Err(after_error(&step.name));
                };
                if array.len() > 31 {
                    return Err(after_error(&step.name));
                }
                let mut collected = Vec::with_capacity(array.len());
                for dependency in array {
                    let Some(name) = dependency.as_str() else {
                        return Err(after_error(&step.name));
                    };
                    let Some(index) = names.get(name).copied() else {
                        return Err(WorkflowError::new(format!(
                            "agents: step {} waits for {}, which does not exist.",
                            step.name, name
                        )));
                    };
                    resolved.push(index);
                    collected.push(name.to_owned());
                }
                collected
            }
        };
        after_names.push(dependency_names);
        after.push(resolved);
    }

    if let Some(cycle) = find_cycle(&after, raw) {
        return Err(WorkflowError::new(format!(
            "agents: steps form a cycle: {}.",
            cycle.join(" -> ")
        )));
    }
    Ok((after_names, after))
}

fn validate_item_sources(
    raw: &[RawStep],
    item_sources: &[Items],
    after_names: &[Vec<String>],
    names: &HashMap<&str, usize>,
) -> Result<(), WorkflowError> {
    for (index, (step, items)) in raw.iter().zip(item_sources).enumerate() {
        if let Items::From(source) = items {
            if !after_names[index]
                .iter()
                .any(|dependency| dependency == source)
            {
                return Err(WorkflowError::new(format!(
                    "agents: step {}: items_from names {}, which is not in after.",
                    step.name, source
                )));
            }
            let Some(source_index) = names.get(source.as_str()).copied() else {
                return Err(WorkflowError::new(format!(
                    "agents: step {}: items_from names {}, which is not in after.",
                    step.name, source
                )));
            };
            if !matches!(item_sources[source_index], Items::Task) {
                return Err(WorkflowError::new(format!(
                    "agents: step {}: items_from names {}, which is a pool; use a step without items.",
                    step.name, source
                )));
            }
        }
    }
    Ok(())
}

fn decode_tools(raw: &[RawStep]) -> Result<Vec<Vec<String>>, WorkflowError> {
    let mut tools = Vec::with_capacity(raw.len());
    for step in raw {
        let selected = match &step.tools {
            None => vec!["read".to_owned(), "search".to_owned()],
            Some(value) => {
                let Some(array) = value.as_array() else {
                    return Err(tool_error(&step.name, "<invalid>"));
                };
                let mut selected = Vec::with_capacity(array.len().min(16));
                for (index, tool) in array.iter().enumerate() {
                    let Some(tool) = tool.as_str() else {
                        return Err(tool_error(&step.name, "<invalid>"));
                    };
                    if index >= 16 || FORBIDDEN_TOOLS.contains(&tool) {
                        return Err(tool_error(&step.name, tool));
                    }
                    selected.push(tool.to_owned());
                }
                selected
            }
        };
        tools.push(selected);
    }
    Ok(tools)
}

fn decode_options(
    raw: &[RawStep],
    tools: &[Vec<String>],
) -> Result<DecodedOptions, WorkflowError> {
    let mut isolation = Vec::with_capacity(raw.len());
    let mut model = Vec::with_capacity(raw.len());
    let mut role = Vec::with_capacity(raw.len());
    let mut system = Vec::with_capacity(raw.len());
    for (step, selected_tools) in raw.iter().zip(tools) {
        let selected = match &step.isolation {
            Some(value) => match value.as_str() {
                Some("worktree") => Isolation::Worktree,
                Some("shared") => Isolation::Shared,
                _ => return Err(isolation_error(&step.name)),
            },
            None if selected_tools
                .iter()
                .any(|tool| tool == "patch" || tool == "exec") =>
            {
                Isolation::Worktree
            }
            None => Isolation::Shared,
        };
        isolation.push(selected);
        model.push(optional_string(step.model.as_ref(), &step.name, "model")?);
        role.push(optional_string(step.role.as_ref(), &step.name, "role")?);
        system.push(optional_string(step.system.as_ref(), &step.name, "system")?);
        if role.last().is_some_and(Option::is_some) && system.last().is_some_and(Option::is_some) {
            return Err(WorkflowError::new(format!(
                "agents: step {}: role and system cannot both be set.",
                step.name
            )));
        }
    }
    Ok(DecodedOptions {
        isolation,
        model,
        role,
        system,
    })
}

fn validate_placeholders(
    step: &RawStep,
    items: &Items,
    after: &[Vec<String>],
    input: Option<&str>,
) -> Result<(), WorkflowError> {
    let index = step.index;
    let is_pool = !matches!(items, Items::Task);
    let has_item = contains_placeholder(&step.prompt, "item");
    if is_pool && !has_item {
        return Err(WorkflowError::new(format!(
            "agents: step {} is a pool, so its prompt must contain {{{{item}}}}.",
            step.name
        )));
    }
    if !is_pool && has_item {
        return Err(WorkflowError::new(format!(
            "agents: step {}: {{{{item}}}} needs items or items_from.",
            step.name
        )));
    }

    let dependencies = &after[index];
    let mut cursor = 0;
    while let Some(relative_open) = step.prompt[cursor..].find("{{") {
        let open = cursor + relative_open;
        let Some(relative_close) = step.prompt[open + 2..].find("}}") else {
            let unknown = &step.prompt[open + 2..];
            return Err(unknown_placeholder(&step.name, unknown));
        };
        let close = open + 2 + relative_close;
        let value = &step.prompt[open + 2..close];
        if value == "item" || value == "input" {
            if value == "input" && input.is_none() {
                return Err(WorkflowError::new(format!(
                    "agents: step {}: {{{{input}}}} needs a saved workflow and input.",
                    step.name
                )));
            }
        } else if let Some(name) = value.strip_prefix("step:") {
            if !dependencies.iter().any(|dependency| dependency == name) {
                return Err(WorkflowError::new(format!(
                    "agents: step {}: {{{{step:{name}}}}} needs {name} in after.",
                    step.name
                )));
            }
        } else {
            return Err(unknown_placeholder(&step.name, value));
        }
        cursor = close + 2;
    }
    Ok(())
}

fn contains_placeholder(prompt: &str, expected: &str) -> bool {
    let mut offset = 0;
    while let Some(open_rel) = prompt[offset..].find("{{") {
        let open = offset + open_rel;
        let Some(close_rel) = prompt[open + 2..].find("}}") else {
            return false;
        };
        let close = open + 2 + close_rel;
        if &prompt[open + 2..close] == expected {
            return true;
        }
        offset = close + 2;
    }
    false
}

fn unknown_placeholder(step: &str, placeholder: &str) -> WorkflowError {
    WorkflowError::new(format!(
        "agents: step {step}: unknown placeholder {{{{{placeholder}}}}}."
    ))
}

fn find_cycle(after: &[Vec<usize>], steps: &[RawStep]) -> Option<Vec<String>> {
    fn visit(
        node: usize,
        after: &[Vec<usize>],
        steps: &[RawStep],
        state: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<String>> {
        state[node] = 1;
        stack.push(node);
        for &dependency in &after[node] {
            if state[dependency] == 0 {
                if let Some(cycle) = visit(dependency, after, steps, state, stack) {
                    return Some(cycle);
                }
            } else if state[dependency] == 1 {
                let start = stack.iter().position(|&member| member == dependency)?;
                let mut cycle = stack[start..]
                    .iter()
                    .map(|&member| steps[member].name.clone())
                    .collect::<Vec<_>>();
                cycle.push(steps[dependency].name.clone());
                return Some(cycle);
            }
        }
        stack.pop();
        state[node] = 2;
        None
    }

    let mut state = vec![0; after.len()];
    let mut stack = Vec::with_capacity(after.len());
    for node in 0..after.len() {
        if state[node] == 0
            && let Some(cycle) = visit(node, after, steps, &mut state, &mut stack)
        {
            return Some(cycle);
        }
    }
    None
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn optional_string(
    value: Option<&Value>,
    step: &str,
    field: &str,
) -> Result<Option<String>, WorkflowError> {
    let Some(value) = value else {
        return Ok(None);
    };
    value
        .as_str()
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| {
            WorkflowError::new(format!(
                "agents: step {step}: {field} must be a string."
            ))
        })
}

fn items_error(step: &str) -> WorkflowError {
    WorkflowError::new(format!(
        "agents: step {step}: items must hold 1 to 1024 items of 1 to 2000 characters."
    ))
}

fn workers_error(step: &str) -> WorkflowError {
    WorkflowError::new(format!(
        "agents: step {step}: workers must be from 1 to 64."
    ))
}

fn after_error(step: &str) -> WorkflowError {
    WorkflowError::new(format!(
        "agents: step {step}: after must hold at most 31 step names."
    ))
}

fn tool_error(step: &str, tool: &str) -> WorkflowError {
    WorkflowError::new(format!(
        "agents: step {step}: tool {tool} is not for subagents."
    ))
}

fn isolation_error(step: &str) -> WorkflowError {
    WorkflowError::new(format!(
        "agents: step {step}: isolation must be \"worktree\" or \"shared\"."
    ))
}
