//! Client request bodies to provider requests for pass-through relays.
//!
//! The client runs its own tools, so tool calls and tool results in the
//! history are carried verbatim. Image parts must be `data:` URLs or base64
//! blocks; remote image URLs are refused.

use dal_core::{
    AssistantPart, CallId, ContextItem, Family, ModelRequest, ModelRoute, ModelToolSpec, Part,
    Purpose, RawJson, ReplaySource, RequestParams, ThinkingLevel,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::super::decode::{
    ChatRequest, MessagesRequest, ResponsesRequest, RouterFail, budget_level, effort_level,
};
use super::super::harness::data_url_image;

/// Builds the provider request for one chat relay.
pub(super) fn chat_request(
    route: &ModelRoute,
    req: &ChatRequest,
) -> Result<ModelRequest, RouterFail> {
    let source = replay_source(Family::Chat, &req.model);
    let mut system = Vec::new();
    let mut context = Vec::new();
    for message in &req.messages {
        match str_member(message, "role") {
            "system" | "developer" => system.push(chat_text(message.get("content"))?),
            "user" => context.push(ContextItem::User {
                parts: chat_parts(message.get("content"))?,
            }),
            "assistant" => context.push(ContextItem::Assistant {
                source: source.clone(),
                parts: chat_assistant(message)?,
            }),
            "tool" => context.push(ContextItem::ToolResult {
                call: CallId::new(str_member(message, "tool_call_id")),
                name: str_member(message, "name").into(),
                is_error: false,
                parts: vec![Part::Text {
                    text: chat_text(message.get("content"))?.into(),
                }],
            }),
            role => return Err(unsupported_role(role)),
        }
    }
    let tools = req
        .tools
        .iter()
        .filter(|tool| str_member(tool, "type") == "function")
        .filter_map(|tool| tool.get("function").and_then(tool_spec))
        .collect::<Vec<_>>();
    Ok(ModelRequest {
        purpose: Purpose::Turn,
        model: route.clone(),
        system: system.join("\n\n").into(),
        tools: tools.into(),
        context: context.into(),
        params: RequestParams {
            thinking: effort_level(req.reasoning_effort.as_deref(), ThinkingLevel::Off)?,
            effort: req.reasoning_effort.as_deref().map(Into::into),
            temperature: req.temperature,
            max_output_tokens: req.max_tokens,
        },
        cache_key: None,
    })
}

/// Builds the provider request for one responses relay.
pub(super) fn responses_request(
    route: &ModelRoute,
    req: &ResponsesRequest,
) -> Result<ModelRequest, RouterFail> {
    let source = replay_source(Family::Responses, &req.model);
    let mut context = Vec::new();
    if let Some(text) = req.input.as_str() {
        context.push(ContextItem::User {
            parts: vec![Part::Text { text: text.into() }],
        });
    }
    for item in req
        .input
        .as_array()
        .into_iter()
        .flat_map(|items| items.iter())
    {
        context.push(responses_item(item, &source)?);
    }
    let tools = req.tools.iter().filter_map(tool_spec).collect::<Vec<_>>();
    Ok(ModelRequest {
        purpose: Purpose::Turn,
        model: route.clone(),
        system: req.instructions.as_str().into(),
        tools: tools.into(),
        context: context.into(),
        params: RequestParams {
            thinking: effort_level(req.reasoning_effort.as_deref(), ThinkingLevel::Off)?,
            effort: req.reasoning_effort.as_deref().map(Into::into),
            temperature: req.temperature,
            max_output_tokens: req.max_output_tokens,
        },
        cache_key: None,
    })
}

/// Builds the provider request for one Anthropic messages relay.
pub(super) fn messages_request(
    route: &ModelRoute,
    req: &MessagesRequest,
) -> Result<ModelRequest, RouterFail> {
    let source = replay_source(Family::Anthropic, &req.model);
    let mut context = Vec::new();
    for message in &req.messages {
        let body = message.get("content");
        match str_member(message, "role") {
            "user" => anthropic_user(body, &mut context)?,
            "assistant" => context.push(ContextItem::Assistant {
                source: source.clone(),
                parts: anthropic_assistant(body)?,
            }),
            role => return Err(unsupported_role(role)),
        }
    }
    let tools = req
        .tools
        .iter()
        .filter_map(|tool| {
            Some(ModelToolSpec {
                name: tool.get("name")?.as_str()?.into(),
                description: str_member(tool, "description").into(),
                parameters: raw_of(tool.get("input_schema")),
                grammar: None,
            })
        })
        .collect::<Vec<_>>();
    Ok(ModelRequest {
        purpose: Purpose::Turn,
        model: route.clone(),
        system: anthropic_system(&req.system).into(),
        tools: tools.into(),
        context: context.into(),
        params: RequestParams {
            thinking: budget_level(req.thinking_budget, ThinkingLevel::Off),
            effort: None,
            temperature: req.temperature,
            max_output_tokens: req.max_tokens,
        },
        cache_key: None,
    })
}

fn chat_text(content: Option<&Value>) -> Result<String, RouterFail> {
    let parts = chat_parts(content)?;
    Ok(parts
        .into_iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.into_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn chat_parts(content: Option<&Value>) -> Result<Vec<Part>, RouterFail> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    if let Some(text) = content.as_str() {
        return Ok(vec![Part::Text { text: text.into() }]);
    }
    let mut parts = Vec::new();
    for block in content
        .as_array()
        .into_iter()
        .flat_map(|items| items.iter())
    {
        match str_member(block, "type") {
            "text" | "input_text" | "output_text" => parts.push(Part::Text {
                text: str_member(block, "text").into(),
            }),
            "image_url" => {
                let url = block
                    .get("image_url")
                    .map_or("", |image| str_member(image, "url"));
                parts.push(data_url_image(url)?);
            }
            "input_image" => parts.push(data_url_image(str_member(block, "image_url"))?),
            kind => return Err(unsupported_part(kind)),
        }
    }
    Ok(parts)
}

fn chat_assistant(message: &Value) -> Result<Vec<AssistantPart>, RouterFail> {
    let mut parts = Vec::new();
    let text = chat_text(message.get("content"))?;
    if !text.is_empty() {
        parts.push(AssistantPart::Text { text: text.into() });
    }
    for call in message
        .get("tool_calls")
        .and_then(|calls| calls.as_array())
        .into_iter()
        .flat_map(|calls| calls.iter())
    {
        let function = call.get("function");
        parts.push(AssistantPart::ToolCall {
            call: CallId::new(str_member(call, "id")),
            name: function
                .map_or("", |function| str_member(function, "name"))
                .into(),
            args: raw_text(function.map_or("{}", |function| str_member(function, "arguments"))),
        });
    }
    Ok(parts)
}

fn responses_item(item: &Value, source: &ReplaySource) -> Result<ContextItem, RouterFail> {
    let kind = item
        .get("type")
        .and_then(JsonValueTrait::as_str)
        .unwrap_or("message");
    match kind {
        "message" => match str_member(item, "role") {
            "assistant" => Ok(ContextItem::Assistant {
                source: source.clone(),
                parts: vec![AssistantPart::Text {
                    text: chat_text(item.get("content"))?.into(),
                }],
            }),
            "user" | "system" | "developer" => Ok(ContextItem::User {
                parts: chat_parts(item.get("content"))?,
            }),
            role => Err(unsupported_role(role)),
        },
        "function_call" => Ok(ContextItem::Assistant {
            source: source.clone(),
            parts: vec![AssistantPart::ToolCall {
                call: CallId::new(str_member(item, "call_id")),
                name: str_member(item, "name").into(),
                args: raw_text(str_member(item, "arguments")),
            }],
        }),
        "function_call_output" => Ok(ContextItem::ToolResult {
            call: CallId::new(str_member(item, "call_id")),
            name: "".into(),
            is_error: false,
            parts: vec![Part::Text {
                text: str_member(item, "output").into(),
            }],
        }),
        "reasoning" => Ok(ContextItem::Assistant {
            source: source.clone(),
            parts: vec![AssistantPart::Thinking {
                text: "".into(),
                replay: Some(raw_of(Some(item))),
            }],
        }),
        kind => Err(unsupported_part(kind)),
    }
}

fn anthropic_system(system: &Value) -> String {
    if let Some(text) = system.as_str() {
        return text.to_owned();
    }
    system
        .as_array()
        .into_iter()
        .flat_map(|blocks| blocks.iter())
        .map(|block| str_member(block, "text"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn anthropic_user(content: Option<&Value>, items: &mut Vec<ContextItem>) -> Result<(), RouterFail> {
    let Some(content) = content else {
        return Ok(());
    };
    if let Some(text) = content.as_str() {
        items.push(ContextItem::User {
            parts: vec![Part::Text { text: text.into() }],
        });
        return Ok(());
    }
    let mut parts = Vec::new();
    for block in content
        .as_array()
        .into_iter()
        .flat_map(|items| items.iter())
    {
        match str_member(block, "type") {
            "text" => parts.push(Part::Text {
                text: str_member(block, "text").into(),
            }),
            "image" => parts.push(anthropic_image(block)?),
            "tool_result" => items.push(ContextItem::ToolResult {
                call: CallId::new(str_member(block, "tool_use_id")),
                name: "".into(),
                is_error: block
                    .get("is_error")
                    .and_then(JsonValueTrait::as_bool)
                    .unwrap_or(false),
                parts: vec![Part::Text {
                    text: anthropic_block_text(block.get("content")).into(),
                }],
            }),
            kind => return Err(unsupported_part(kind)),
        }
    }
    if !parts.is_empty() {
        items.push(ContextItem::User { parts });
    }
    Ok(())
}

fn anthropic_assistant(content: Option<&Value>) -> Result<Vec<AssistantPart>, RouterFail> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    if let Some(text) = content.as_str() {
        return Ok(vec![AssistantPart::Text { text: text.into() }]);
    }
    let mut parts = Vec::new();
    for block in content
        .as_array()
        .into_iter()
        .flat_map(|items| items.iter())
    {
        match str_member(block, "type") {
            "text" => parts.push(AssistantPart::Text {
                text: str_member(block, "text").into(),
            }),
            "tool_use" => parts.push(AssistantPart::ToolCall {
                call: CallId::new(str_member(block, "id")),
                name: str_member(block, "name").into(),
                args: raw_of(block.get("input")),
            }),
            "thinking" | "redacted_thinking" => parts.push(AssistantPart::Thinking {
                text: str_member(block, "thinking").into(),
                replay: Some(raw_of(Some(block))),
            }),
            kind => return Err(unsupported_part(kind)),
        }
    }
    Ok(parts)
}

fn anthropic_image(block: &Value) -> Result<Part, RouterFail> {
    let source = block.get("source");
    let kind = source.map_or("", |source| str_member(source, "type"));
    if kind != "base64" {
        return Err(RouterFail::bad(
            "invalid_request",
            "dalgon serve does not fetch image URLs: send a data: URL".to_owned(),
        ));
    }
    let mime = source.map_or("", |source| str_member(source, "media_type"));
    let data = source.map_or("", |source| str_member(source, "data"));
    data_url_image(&format!("data:{mime};base64,{data}"))
}

fn anthropic_block_text(content: Option<&Value>) -> String {
    match content {
        Some(content) if content.is_str() => content.as_str().unwrap_or_default().to_owned(),
        Some(content) => content
            .as_array()
            .into_iter()
            .flat_map(|blocks| blocks.iter())
            .map(|block| str_member(block, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

fn tool_spec(tool: &Value) -> Option<ModelToolSpec> {
    Some(ModelToolSpec {
        name: tool.get("name")?.as_str()?.into(),
        description: str_member(tool, "description").into(),
        parameters: raw_of(tool.get("parameters")),
        grammar: None,
    })
}

fn replay_source(family: Family, model: &str) -> ReplaySource {
    ReplaySource {
        family,
        model: if model.is_empty() { "relay" } else { model }.into(),
    }
}

fn str_member<'a>(value: &'a Value, name: &str) -> &'a str {
    value
        .get(name)
        .and_then(JsonValueTrait::as_str)
        .unwrap_or("")
}

fn raw_text(text: &str) -> RawJson {
    RawJson::parse(text).unwrap_or_else(|_| RawJson::null())
}

fn raw_of(value: Option<&Value>) -> RawJson {
    value
        .and_then(|value| sonic_rs::to_string(value).ok())
        .map_or_else(RawJson::null, |text| raw_text(&text))
}

fn unsupported_role(role: &str) -> RouterFail {
    RouterFail::bad(
        "invalid_request",
        format!(r#"role "{role}" is not supported"#),
    )
}

fn unsupported_part(kind: &str) -> RouterFail {
    RouterFail::bad(
        "invalid_request",
        format!(r#"content part "{kind}" is not supported by dalgon serve"#),
    )
}
