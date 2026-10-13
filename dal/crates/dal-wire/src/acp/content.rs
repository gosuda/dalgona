//! ACP content blocks to session parts, and slash-command detection.

use dal_core::Part;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use crate::jsonrpc::ErrorObject;

/// Converts one ACP content block to a session part.
fn prompt_block(block: &Value) -> Result<Part, ErrorObject> {
    let kind = block
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match kind {
        "text" => {
            let text = block
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            Ok(Part::Text { text: text.into() })
        }
        "image" => {
            let data = block
                .get("data")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let mime = block
                .get("mimeType")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if mime.is_empty() || data.is_empty() {
                return Err(crate::rpc::invalid_params(
                    "session/prompt",
                    "image block needs mimeType and data",
                ));
            }
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                .map_err(|_| {
                    crate::rpc::invalid_params("session/prompt", "image data is not base64")
                })?;
            Ok(Part::Image {
                mime: mime.into(),
                bytes: bytes.into(),
            })
        }
        "resource_link" => {
            let name = block
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let uri = block
                .get("uri")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            Ok(Part::Text {
                text: format!("Referenced resource: {name} {uri}").into(),
            })
        }
        "resource" => prompt_resource(block),
        "audio" => Err(crate::rpc::invalid_params(
            "session/prompt",
            "dalgon does not accept audio",
        )),
        _ => Err(crate::rpc::invalid_params(
            "session/prompt",
            format!(r#"unknown prompt block type "{kind}""#),
        )),
    }
}

/// Converts one ACP `resource` block to a session part.
fn prompt_resource(block: &Value) -> Result<Part, ErrorObject> {
    let resource = block.get("resource").cloned().unwrap_or(Value::from(false));
    let uri = resource
        .get("uri")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let mime = resource
        .get("mimeType")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let text = resource
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let blob = resource
        .get("blob")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if mime.starts_with("image/") && (!blob.is_empty() || !text.is_empty()) {
        use base64::Engine as _;
        let raw = if blob.is_empty() { text } else { blob };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|_| {
                crate::rpc::invalid_params("session/prompt", "image data is not base64")
            })?;
        return Ok(Part::Image {
            mime: mime.into(),
            bytes: bytes.into(),
        });
    }
    if mime == "text" || mime.is_empty() || mime.starts_with("text/") {
        return Ok(Part::Text {
            text: format!("<resource uri=\"{uri}\">\n{text}\n</resource>").into(),
        });
    }
    Err(crate::rpc::invalid_params(
        "session/prompt",
        format!("dalgon accepts embedded text resources and image blobs: {mime} is not supported"),
    ))
}

/// Converts ACP content blocks to session parts.
pub(crate) fn prompt_parts(blocks: &Value) -> Result<Vec<Part>, ErrorObject> {
    let items = blocks
        .as_array()
        .ok_or_else(|| crate::rpc::invalid_params("session/prompt", "prompt is not an array"))?;
    let mut parts = Vec::with_capacity(items.len());
    for block in items {
        parts.push(prompt_block(block)?);
    }
    Ok(parts)
}

/// Detects a `/command [args]` prompt for slash-command conversion.
pub(crate) fn slash_command(parts: &[Part]) -> Option<(String, String)> {
    if parts.len() != 1 {
        return None;
    }
    let Part::Text { text } = &parts[0] else {
        return None;
    };
    let line = text.trim();
    if !line.starts_with('/') || line.contains('\n') {
        return None;
    }
    let mut words = line[1..].splitn(2, ' ');
    let name = words.next().unwrap_or("").to_owned();
    if name.is_empty() {
        return None;
    }
    Some((name, words.next().unwrap_or("").to_owned()))
}
