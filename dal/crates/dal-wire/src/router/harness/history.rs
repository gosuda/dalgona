//! Router request histories to harness prompt text, images, and digest items.

use super::super::decode::RouterFail;
use crate::router::HarnessMode;
use dal_core::Part;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

/// Rejects client tools, choices, and tool history for harness models.
pub(super) fn reject_harness_tools(
    mode: HarnessMode,
    tools: &[Value],
    choice: &str,
    messages: &[Value],
) -> Result<(), RouterFail> {
    let id = mode.id();
    if !tools.is_empty() {
        return Err(RouterFail::bad(
            "invalid_request",
            format!(r#"field "tools" is not accepted for {id}: dalgon runs its own tools"#),
        ));
    }
    if choice != "auto" && choice != "none" {
        return Err(RouterFail::bad(
            "invalid_request",
            format!(r#"field "tool_choice" is not accepted for {id}: dalgon runs its own tools"#),
        ));
    }
    for message in messages {
        if has_tool_history(message) {
            return Err(RouterFail::bad(
                "invalid_request",
                format!(r#"field "messages" is not accepted for {id}: dalgon runs its own tools"#),
            ));
        }
    }
    Ok(())
}

/// Returns true when one history item carries tool calls or results.
pub(super) fn has_tool_history(message: &Value) -> bool {
    if message.get("tool_calls").is_some() || message.get("tool_call_id").is_some() {
        return true;
    }
    let contents = message
        .get("content")
        .and_then(|content| content.as_array())
        .map(|items| items.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    contents.iter().any(|block| {
        matches!(
            block.get("type").and_then(|kind| kind.as_str()),
            Some("tool_use" | "tool_result" | "function_call" | "function_call_output")
        )
    })
}

/// One normalized history item for prompt construction.
pub(super) struct HistoryText {
    /// True for user items, false for assistant items.
    user: bool,
    /// The item text.
    text: String,
}

/// Extracts chat history texts plus images.
pub(super) fn chat_texts(
    messages: &[Value],
    mode: HarnessMode,
) -> Result<(Vec<HistoryText>, Vec<Part>), RouterFail> {
    let id = mode.id();
    let mut history = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(|role| role.as_str())
            .unwrap_or("");
        let user = match role {
            "user" => true,
            "assistant" => false,
            _ => {
                return Err(RouterFail::bad(
                    "invalid_request",
                    format!(
                        r#"field "messages" is not accepted for {id}: role "{role}" is not supported"#
                    ),
                ));
            }
        };
        history.push(HistoryText {
            user,
            text: chat_content_text(message)?,
        });
    }
    let images = chat_images(messages)?;
    Ok((history, images))
}

/// Extracts one chat message's text content.
pub(super) fn chat_content_text(message: &Value) -> Result<String, RouterFail> {
    match message.get("content") {
        Some(content) if content.as_str().is_some() => {
            Ok(content.as_str().unwrap_or_default().to_owned())
        }
        Some(content) => {
            let mut out = String::new();
            for block in content
                .as_array()
                .into_iter()
                .flat_map(|items| items.iter())
            {
                match block.get("type").and_then(|kind| kind.as_str()) {
                    Some("text") => {
                        out.push_str(
                            block
                                .get("text")
                                .and_then(|text| text.as_str())
                                .unwrap_or(""),
                        );
                    }
                    Some("image_url") | None => {}
                    Some(other) => {
                        return Err(RouterFail::bad(
                            "invalid_request",
                            format!(r#"field "messages" content block "{other}" is not supported"#),
                        ));
                    }
                }
            }
            Ok(out)
        }
        None => Ok(String::new()),
    }
}

/// Extracts image parts from chat image URLs (data URLs only).
pub(super) fn chat_images(messages: &[Value]) -> Result<Vec<Part>, RouterFail> {
    let mut parts = Vec::new();
    for message in messages {
        let Some(content) = message.get("content") else {
            continue;
        };
        let Some(blocks) = content.as_array() else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(|kind| kind.as_str()) != Some("image_url") {
                continue;
            }
            let url = block
                .get("image_url")
                .and_then(|item| item.get("url"))
                .and_then(|url| url.as_str())
                .unwrap_or("");
            parts.push(data_url_image(url)?);
        }
    }
    Ok(parts)
}

/// Converts one `data:<mime>;base64,<data>` URL to an image part.
pub(crate) fn data_url_image(url: &str) -> Result<Part, RouterFail> {
    use base64::Engine as _;
    let rest = url.strip_prefix("data:").ok_or_else(|| {
        RouterFail::bad(
            "invalid_request",
            "dalgon serve does not fetch image URLs: send a data: URL".to_owned(),
        )
    })?;
    let (meta, data) = rest.split_once(',').ok_or_else(|| {
        RouterFail::bad("invalid_request", "image data URL is not valid".to_owned())
    })?;
    let mime = meta.split(';').next().unwrap_or("");
    if mime.is_empty() || data.is_empty() {
        return Err(RouterFail::bad(
            "invalid_request",
            "image data URL is not valid".to_owned(),
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| {
            RouterFail::bad("invalid_request", "image data URL is not valid".to_owned())
        })?;
    Ok(Part::Image {
        mime: mime.into(),
        bytes: bytes.into(),
    })
}

/// Extracts responses texts: instructions prefix plus input items.
pub(super) fn responses_texts(
    input: &Value,
    instructions: &str,
    mode: HarnessMode,
) -> Result<(Vec<HistoryText>, Vec<Part>), RouterFail> {
    let id = mode.id();
    let mut history = Vec::new();
    if !instructions.is_empty() {
        history.push(HistoryText {
            user: true,
            text: instructions.to_owned(),
        });
    }
    if let Some(text) = input.as_str() {
        history.push(HistoryText {
            user: true,
            text: text.to_owned(),
        });
        return Ok((history, Vec::new()));
    }
    let items = input
        .as_array()
        .map(|items| items.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    for item in items {
        let kind = item
            .get("type")
            .and_then(|kind| kind.as_str())
            .unwrap_or("");
        match kind {
            "message" => {
                let role = item
                    .get("role")
                    .and_then(|role| role.as_str())
                    .unwrap_or("");
                if role != "user" {
                    return Err(RouterFail::bad(
                        "invalid_request",
                        format!(
                            r#"field "input" is not accepted for {id}: dalgon runs its own tools"#
                        ),
                    ));
                }
                let content = item.get("content").cloned().unwrap_or(Value::from(""));
                history.push(HistoryText {
                    user: true,
                    text: responses_content(&content),
                });
            }
            "input_text" => history.push(HistoryText {
                user: true,
                text: item
                    .get("text")
                    .and_then(|text| text.as_str())
                    .unwrap_or("")
                    .to_owned(),
            }),
            "input_image" => {
                let url = item
                    .get("image_url")
                    .and_then(|url| url.as_str())
                    .unwrap_or("");
                let _ = data_url_image(url)?;
            }
            "function_call" | "function_call_output" | "reasoning" | "encrypted_content" => {
                return Err(RouterFail::bad(
                    "invalid_request",
                    format!(r#"field "input" is not accepted for {id}: dalgon runs its own tools"#),
                ));
            }
            _ => {
                return Err(RouterFail::bad(
                    "invalid_request",
                    format!(r#"field "input" item "{kind}" is not supported"#),
                ));
            }
        }
    }
    Ok((history, Vec::new()))
}

/// Extracts text from responses message content.
pub(super) fn responses_content(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    let mut out = String::new();
    for block in content
        .as_array()
        .into_iter()
        .flat_map(|items| items.iter())
    {
        if block.get("type").and_then(|kind| kind.as_str()) == Some("input_text") {
            out.push_str(
                block
                    .get("text")
                    .and_then(|text| text.as_str())
                    .unwrap_or(""),
            );
        }
    }
    out
}

/// Extracts Anthropic messages texts.
pub(super) fn messages_texts(
    messages: &[Value],
    system: &Value,
    mode: HarnessMode,
) -> Result<(Vec<HistoryText>, Vec<Part>), RouterFail> {
    let id = mode.id();
    let mut history = Vec::new();
    let system_text = system_text(system);
    if !system_text.is_empty() {
        history.push(HistoryText {
            user: true,
            text: system_text,
        });
    }
    for message in messages {
        let role = message
            .get("role")
            .and_then(|role| role.as_str())
            .unwrap_or("");
        if role != "user" && role != "assistant" {
            return Err(RouterFail::bad(
                "invalid_request",
                format!(r#"field "messages" role "{role}" is not supported"#),
            ));
        }
        if role == "assistant" {
            let _ = id;
        }
        history.push(HistoryText {
            user: role == "user",
            text: anthropic_text(message)?,
        });
    }
    Ok((history, Vec::new()))
}

/// Extracts the system text from a string or block list.
pub(super) fn system_text(system: &Value) -> String {
    if let Some(text) = system.as_str() {
        return text.to_owned();
    }
    system
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|block| block.get("type").and_then(|kind| kind.as_str()) == Some("text"))
                .filter_map(|block| block.get("text").and_then(|text| text.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Extracts text from Anthropic content blocks, dropping tool blocks.
pub(super) fn anthropic_text(message: &Value) -> Result<String, RouterFail> {
    let content = message.get("content").cloned().unwrap_or(Value::from(""));
    if let Some(text) = content.as_str() {
        return Ok(text.to_owned());
    }
    let mut out = String::new();
    for block in content
        .as_array()
        .into_iter()
        .flat_map(|items| items.iter())
    {
        match block.get("type").and_then(|kind| kind.as_str()) {
            Some("text") => {
                out.push_str(
                    block
                        .get("text")
                        .and_then(|text| text.as_str())
                        .unwrap_or(""),
                );
            }
            Some("image" | "tool_use" | "tool_result" | "thinking" | "redacted_thinking")
            | None => {}
            Some(other) => {
                return Err(RouterFail::bad(
                    "invalid_request",
                    format!(r#"field "messages" content block "{other}" is not supported"#),
                ));
            }
        }
    }
    Ok(out)
}

/// Builds the canonical history items for digest continuation.
pub(super) fn canon_items(items: &[HistoryText]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            sonic_rs::json!({"role": if item.user { "user" } else { "assistant" }, "text": item.text})
        })
        .collect()
}

/// Builds the harness prompt text for a new or continued session.
pub(super) fn build_prompt(items: &[HistoryText], fresh: bool) -> Vec<String> {
    if !fresh {
        return items
            .last()
            .map(|item| vec![item.text.clone()])
            .unwrap_or_default();
    }
    let mut out = String::new();
    let mut earlier = Vec::new();
    for item in items.iter().take(items.len().saturating_sub(1)) {
        earlier.push(format!(
            "{}: {}",
            if item.user { "user" } else { "assistant" },
            item.text
        ));
    }
    if let Some(first) = items.first()
        && first.user
        && items.len() == 1
    {
        return vec![first.text.clone()];
    }
    if !earlier.is_empty() {
        out.push_str("Earlier conversation:\n");
        for line in earlier {
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    if let Some(last) = items.last() {
        out.push_str(&last.text);
    }
    vec![out]
}
