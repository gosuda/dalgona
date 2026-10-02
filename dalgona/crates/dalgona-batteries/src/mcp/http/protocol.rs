// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Streamable-HTTP JSON-RPC codec: envelopes, headers, and tolerant replies.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dal_core::RawJson;
use reqwest::{
    Response,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use sonic_rs::{JsonValueTrait, Value};

use crate::mcp::McpError;

pub(crate) const PROTOCOL_VERSION: &str = "2026-07-28";
pub(crate) const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
pub(crate) fn advertises_legacy(message: &str) -> bool {
    message.contains(LEGACY_PROTOCOL_VERSION)
}
pub(crate) const MCP_VERSION_HEADER: &str = "mcp-protocol-version";
pub(crate) const MCP_METHOD_HEADER: &str = "mcp-method";
pub(crate) const MCP_NAME_HEADER: &str = "mcp-name";
pub(crate) const MCP_SESSION_HEADER: &str = "mcp-session-id";

pub(crate) fn protocol_error(message: String) -> McpError {
    McpError::Protocol {
        code: -32600,
        message,
    }
}

/// Builds one complete JSON-RPC envelope with MCP client metadata.
///
/// `params` is a JSON object text (for example `{}` or
/// `{"name":..,"arguments":..}`). The `_meta` member and the per-request
/// `progressToken` are injected at the text level so caller argument bytes
/// are never parsed and reserialized.
pub(crate) fn request_body(
    id: u64,
    method: &str,
    params: &str,
    version: &str,
    client_version: &str,
) -> Result<RawJson, McpError> {
    let method = sonic_rs::to_string(method)
        .map_err(|error| protocol_error(format!("invalid method: {error}")))?;
    let version = sonic_rs::to_string(version)
        .map_err(|error| protocol_error(format!("invalid version: {error}")))?;
    let client_version = sonic_rs::to_string(client_version)
        .map_err(|error| protocol_error(format!("invalid client: {error}")))?;
    let meta = format!(
        "\"_meta\":{{\"io.modelcontextprotocol\\/protocolVersion\":{version},\"io.modelcontextprotocol\\/clientInfo\":{{\"name\":\"dalgona\",\"version\":{client_version}}},\"io.modelcontextprotocol\\/clientCapabilities\":{{}},\"progressToken\":\"t-{id}\"}}"
    );
    let trimmed = params.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return Err(protocol_error(
            "MCP params must be a JSON object".to_owned(),
        ));
    }
    let inner = trimmed[1..trimmed.len() - 1].trim();
    let merged = if inner.is_empty() {
        format!("{{{meta}}}")
    } else {
        format!("{{{inner},{meta}}}")
    };
    RawJson::parse(&format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":{method},\"params\":{merged}}}"
    ))
    .map_err(|error| protocol_error(format!("invalid request: {error}")))
}

/// Builds one JSON-RPC notification envelope, which carries no request id.
pub(crate) fn notification_body(
    method: &str,
    version: &str,
    client_version: &str,
) -> Result<RawJson, McpError> {
    let method = sonic_rs::to_string(method)
        .map_err(|error| protocol_error(format!("invalid method: {error}")))?;
    let version = sonic_rs::to_string(version)
        .map_err(|error| protocol_error(format!("invalid version: {error}")))?;
    let client_version = sonic_rs::to_string(client_version)
        .map_err(|error| protocol_error(format!("invalid client: {error}")))?;
    RawJson::parse(&format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":{method},\"params\":{{\"_meta\":{{\"io.modelcontextprotocol\\/protocolVersion\":{version},\"io.modelcontextprotocol\\/clientInfo\":{{\"name\":\"dalgona\",\"version\":{client_version}}},\"io.modelcontextprotocol\\/clientCapabilities\":{{}}}}}}}}"
    ))
    .map_err(|error| protocol_error(format!("invalid notification: {error}")))
}

/// Encodes the `Mcp-Name` header, including sentinel-pattern values.
pub(crate) fn escape_name(tool: &str) -> String {
    let plain = tool.is_ascii()
        && !tool.is_empty()
        && tool
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !(tool.starts_with("=?base64?") && tool.ends_with("?="));
    if plain {
        return tool.to_owned();
    }
    format!("=?base64?{}?=", URL_SAFE_NO_PAD.encode(tool.as_bytes()))
}

/// Reports whether a status body carries a modern JSON-RPC error object.
/// Only the modern version-negotiation codes count; any other body takes
/// the legacy fallback path.
pub(crate) fn recognizes_modern_error(text: &str) -> bool {
    let Ok(value) = sonic_rs::from_str::<Value>(text) else {
        return false;
    };
    let Some(error) = value.get("error") else {
        return false;
    };
    let code = error.get("code").and_then(JsonValueTrait::as_i64);
    matches!(code, Some(-32020 | -32021 | -32022 | -32601)) && error.get("message").is_some()
}

/// Decodes one JSON-RPC error response, if the body carries that shape.
pub(crate) fn json_rpc_error(text: &str) -> Option<McpError> {
    let value = sonic_rs::from_str::<Value>(text).ok()?;
    let error = value.get("error")?;
    let code = error
        .get("code")
        .and_then(JsonValueTrait::as_i64)
        .unwrap_or(-32600);
    let message = error
        .get("message")
        .and_then(JsonValueTrait::as_str)
        .unwrap_or("protocol error")
        .to_owned();
    Some(McpError::Protocol { code, message })
}

/// Reads the legacy session header from a successful POST response.
pub(crate) fn session_from(response: &Response) -> Option<String> {
    response
        .headers()
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Builds one outbound header map with static MCP field names.
///
/// `method` is `None` for client responses to server requests, which carry
/// no `Mcp-Method` field.
pub(crate) fn outbound_headers(
    version: &str,
    method: Option<&str>,
    name: Option<&str>,
    extra: &[(HeaderName, HeaderValue)],
    session: Option<&str>,
) -> Result<HeaderMap, McpError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        HeaderName::from_static(MCP_VERSION_HEADER),
        HeaderValue::from_str(version).map_err(|_| protocol_error("bad version".to_owned()))?,
    );
    if let Some(method) = method {
        headers.insert(
            HeaderName::from_static(MCP_METHOD_HEADER),
            HeaderValue::from_str(method).map_err(|_| protocol_error("bad method".to_owned()))?,
        );
    }
    if let Some(name) = name {
        headers.insert(
            HeaderName::from_static(MCP_NAME_HEADER),
            HeaderValue::from_str(name).map_err(|_| protocol_error("bad tool name".to_owned()))?,
        );
    }
    for (name, value) in extra {
        headers.insert(name.clone(), value.clone());
    }
    if let Some(session) = session {
        headers.insert(
            HeaderName::from_static(MCP_SESSION_HEADER),
            HeaderValue::from_str(session)
                .map_err(|_| protocol_error("bad session id".to_owned()))?,
        );
    }
    Ok(headers)
}
