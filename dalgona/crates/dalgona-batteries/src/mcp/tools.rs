// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use std::sync::Arc;

use dal_agent::error::{ServiceError, ToolError};
use dal_agent::ext::{ArgError, BoxFuture, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_core::{
    McpRequest, ModelInfo, Name, Preview, RawJson, SessionId, ToolClass, ToolSpec, Workspace,
};
use reqwest::header::{HeaderName, HeaderValue};
use serde::Deserialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use crate::mcp::{
    McpError, RESULT_TEXT_CAP, RESULT_TRUNCATED_MARKER, TOOL_CACHE_CAP, TOOL_CACHE_DEFAULT,
};

pub(crate) use dal_core::ext::McpServerDecl as ServerDecl;

/// Resolves `argv[0]` through the captured PATH value without consulting ambient state.
pub(crate) fn resolve_executable(program: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.is_absolute() {
        return executable_file(candidate).then(|| candidate.to_path_buf());
    }
    let path = path?;
    std::env::split_paths(path)
        .map(|directory| directory.join(program))
        .find(|candidate| executable_file(candidate))
}

fn executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// A stable per-session server identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Key {
    pub session: SessionId,
    pub skill: String,
    pub server: String,
}

impl Key {
    /// Returns the diagnostic and command identity `<session>:<skill>:<server>`.
    #[must_use]
    pub(crate) fn display(&self) -> String {
        format!("{}:{}:{}", self.session, self.skill, self.server)
    }
}

/// Folds an MCP tool name to the stable mapped-tool grammar.
#[must_use]
pub(crate) fn fold_tool_name(skill: &str, server: &str, tool: &str) -> String {
    let mut output = String::with_capacity(
        skill
            .len()
            .saturating_add(server.len())
            .saturating_add(tool.len())
            .saturating_add(2)
            .min(200),
    );
    for (index, part) in [skill, server, tool].into_iter().enumerate() {
        if index != 0 && output.len() < 200 {
            output.push('.');
        }
        for character in part.chars() {
            if output.len() == 200 {
                return output;
            }
            output.push(
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                    character
                } else {
                    '_'
                },
            );
        }
    }
    output
}

/// One validated header annotation on a remote tool's input schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HeaderAnnotation {
    pub(crate) header: String,
    pub(crate) path: Vec<String>,
}

/// An invalid `x-mcp-header` annotation or duplicate header name.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HeaderAnnotationError {
    #[error("x-mcp-header must name a valid HTTP field")]
    InvalidName,
    #[error("x-mcp-header names the same HTTP field more than once")]
    DuplicateName,
}

/// Reads `x-mcp-header` field annotations from the input-schema properties.
pub(crate) fn header_annotations(
    schema: &Value,
) -> Result<Vec<HeaderAnnotation>, HeaderAnnotationError> {
    let mut annotations = Vec::new();
    let mut names = BTreeSet::new();
    collect_header_annotations(schema, &mut Vec::new(), &mut annotations, &mut names)?;
    Ok(annotations)
}

fn collect_header_annotations(
    schema: &Value,
    path: &mut Vec<String>,
    output: &mut Vec<HeaderAnnotation>,
    names: &mut BTreeSet<String>,
) -> Result<(), HeaderAnnotationError> {
    let Some(properties) = schema.get("properties").and_then(|value| value.as_object()) else {
        return Ok(());
    };
    for (property, definition) in properties {
        let property: &str = property;
        path.push(property.to_owned());
        if let Some(annotation) = definition.get("x-mcp-header") {
            let Some(header) = annotation.as_str() else {
                return Err(HeaderAnnotationError::InvalidName);
            };
            if !valid_header_name(header) {
                return Err(HeaderAnnotationError::InvalidName);
            }
            if !names.insert(header.to_ascii_lowercase()) {
                return Err(HeaderAnnotationError::DuplicateName);
            }
            output.push(HeaderAnnotation {
                header: header.to_owned(),
                path: path.clone(),
            });
        }
        collect_header_annotations(definition, path, output, names)?;
        path.pop();
    }
    Ok(())
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// Builds request headers from the final argument object and a remote schema.
pub(crate) fn parameter_headers(
    annotations: &[HeaderAnnotation],
    arguments: &RawJson,
) -> Result<Vec<(HeaderName, HeaderValue)>, ParameterHeaderError> {
    let parsed = arguments
        .decode_as::<Value>()
        .map_err(|_| ParameterHeaderError::InvalidArguments)?;
    let mut headers = Vec::with_capacity(annotations.len());
    for annotation in annotations {
        let Some(value) = annotation
            .path
            .iter()
            .try_fold(&parsed, |value, component| value.get(component.as_str()))
        else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let text = value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value.as_i64().map(|number| number.to_string()))
            .or_else(|| value.as_u64().map(|number| number.to_string()))
            .or_else(|| value.as_bool().map(|boolean| boolean.to_string()))
            .ok_or(ParameterHeaderError::UnsupportedValue)?;
        let encoded = encode_header_value(&text);
        let name = HeaderName::from_bytes(format!("Mcp-Param-{}", annotation.header).as_bytes())
            .map_err(|_| ParameterHeaderError::InvalidName)?;
        let value =
            HeaderValue::from_str(&encoded).map_err(|_| ParameterHeaderError::InvalidValue)?;
        headers.push((name, value));
    }
    Ok(headers)
}

/// A mapped argument could not be represented as a safe request header.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ParameterHeaderError {
    #[error("MCP tool arguments are not valid JSON")]
    InvalidArguments,
    #[error("MCP header annotation points to a non-scalar value")]
    UnsupportedValue,
    #[error("MCP header annotation contains an invalid name")]
    InvalidName,
    #[error("MCP header annotation value is invalid")]
    InvalidValue,
}

fn encode_header_value(value: &str) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let already_encoded = value.starts_with("=?base64?") && value.ends_with("?=");
    if value.is_ascii() && !already_encoded {
        return value.to_owned();
    }
    format!("=?base64?{}?=", STANDARD.encode(value.as_bytes()))
}

/// One remote tools/list item after tolerant decoding.
#[derive(Clone, Debug)]
pub(crate) struct RemoteTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) schema: RawJson,
    pub(crate) headers: Vec<HeaderAnnotation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteToolWire {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    input_schema: Option<RawJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteToolPageWire {
    tools: Vec<RemoteToolWire>,
    #[serde(default)]
    next_cursor: Option<String>,
    #[serde(default)]
    ttl_ms: Option<u64>,
}

/// A decoded tool was excluded because its optional header metadata was invalid.
#[derive(Clone, Debug)]
pub(crate) struct ExcludedTool {
    pub(crate) warning: String,
}

/// One tolerant tools/list page. Unknown fields are ignored by Serde.
#[derive(Clone, Debug)]
pub(crate) struct RemoteToolPage {
    pub(crate) tools: Vec<RemoteTool>,
    pub(crate) excluded: Vec<ExcludedTool>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) ttl_ms: Option<u64>,
}

/// Parses one tools/list page while retaining each tool schema as validated raw JSON.
pub(crate) fn decode_tool_page(body: &str) -> Result<RemoteToolPage, McpError> {
    let wire =
        sonic_rs::from_str::<RemoteToolPageWire>(body).map_err(|error| McpError::Protocol {
            code: -32600,
            message: format!("invalid tools/list response: {error}"),
        })?;
    let mut tools = Vec::with_capacity(wire.tools.len());
    let mut excluded = Vec::new();
    for tool in wire.tools {
        match decode_tool(tool)? {
            Ok(tool) => tools.push(tool),
            Err(tool) => excluded.push(tool),
        }
    }
    Ok(RemoteToolPage {
        tools,
        excluded,
        next_cursor: wire.next_cursor,
        ttl_ms: wire.ttl_ms,
    })
}

fn decode_tool(wire: RemoteToolWire) -> Result<Result<RemoteTool, ExcludedTool>, McpError> {
    if wire.name.is_empty() {
        return Err(McpError::Protocol {
            code: -32600,
            message: "server returned a tool with an empty name".to_owned(),
        });
    }
    let schema = match wire.input_schema {
        Some(schema) => schema,
        None => RawJson::parse(r#"{"type":"object","properties":{}}"#).map_err(|error| {
            McpError::Protocol {
                code: -32600,
                message: format!("internal MCP schema is invalid: {error}"),
            }
        })?,
    };
    let schema_value = schema
        .decode_as::<Value>()
        .map_err(|error| McpError::Protocol {
            code: -32600,
            message: format!("server returned an invalid tool schema: {error}"),
        })?;
    let Ok(headers) = header_annotations(&schema_value) else {
        return Ok(Err(ExcludedTool {
            warning: format!(
                "mcp: mapped tool {} excluded; invalid x-mcp-header annotation",
                wire.name
            ),
        }));
    };
    let valid_schema = dal_core::ext::valid_tool_parameters(&schema);
    if !valid_schema {
        return Err(McpError::Protocol {
            code: -32600,
            message: format!("server returned an invalid schema for tool {}", wire.name),
        });
    }
    Ok(Ok(RemoteTool {
        name: wire.name,
        description: wire.description.unwrap_or_default(),
        schema,
        headers,
    }))
}

/// A per-server tools/list cache entry.
#[derive(Clone, Debug)]
pub(crate) struct ToolListCache {
    pub(crate) until: Instant,
    pub(crate) tools: Vec<RemoteTool>,
}

pub(crate) fn cache_ttl(ttl_ms: Option<u64>) -> Duration {
    let maximum = u64::try_from(TOOL_CACHE_CAP.as_millis()).unwrap_or(u64::MAX);
    let fallback = u64::try_from(TOOL_CACHE_DEFAULT.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(ttl_ms.unwrap_or(fallback).min(maximum))
}

/// The shaped MCP tool result and its server-declared error state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ShapedResult {
    pub(crate) text: String,
    pub(crate) is_error: bool,
}

/// Shapes the MCP content list into the text exposed by a mapped tool.
pub(crate) fn shape_result(
    content: &[Value],
    is_error: bool,
    result_type: Option<&str>,
) -> Result<ShapedResult, McpError> {
    if let Some(value) = result_type
        && !matches!(value, "complete" | "input_required")
    {
        return Err(McpError::ResultType {
            value: value.to_owned(),
        });
    }
    let mut rendered = Vec::with_capacity(content.len());
    let mut skipped = 0_usize;
    for block in content {
        let Some(kind) = block.get("type").and_then(JsonValueTrait::as_str) else {
            skipped += 1;
            continue;
        };
        match kind {
            "text" => {
                if let Some(text) = block.get("text").and_then(JsonValueTrait::as_str) {
                    rendered.push(text.to_owned());
                }
            }
            "image" => rendered.push(format!(
                "[image block: {}, {} base64 chars]",
                mime_type(block),
                string_len(block.get("data"))
            )),
            "audio" => rendered.push(format!(
                "[audio block: {}, {} base64 chars]",
                mime_type(block),
                string_len(block.get("data"))
            )),
            "resource_link" => {
                if let Some(uri) = block.get("uri").and_then(JsonValueTrait::as_str) {
                    rendered.push(format!("[resource link: {uri}]"));
                }
            }
            "resource" => {
                if let Some(resource) = block.get("resource") {
                    if let Some(text) = resource.get("text").and_then(JsonValueTrait::as_str) {
                        rendered.push(text.to_owned());
                    } else {
                        rendered.push(format!(
                            "[resource {} (binary, {})]",
                            resource
                                .get("uri")
                                .and_then(JsonValueTrait::as_str)
                                .unwrap_or("unknown"),
                            resource
                                .get("mimeType")
                                .and_then(JsonValueTrait::as_str)
                                .unwrap_or("unknown")
                        ));
                    }
                }
            }
            _ => skipped += 1,
        }
    }
    if skipped == 1 {
        rendered.push("mcp: skipped 1 unknown content block".to_owned());
    } else if skipped > 1 {
        rendered.push(format!("mcp: skipped {skipped} unknown content blocks"));
    }
    Ok(ShapedResult {
        text: truncate_result(rendered.join("\n")),
        is_error,
    })
}

fn mime_type(block: &Value) -> &str {
    block
        .get("mimeType")
        .and_then(JsonValueTrait::as_str)
        .unwrap_or("unknown")
}

fn string_len(value: Option<&Value>) -> usize {
    value.and_then(JsonValueTrait::as_str).map_or(0, str::len)
}

fn truncate_result(text: String) -> String {
    if text.len() <= RESULT_TEXT_CAP {
        return text;
    }
    let mut boundary = RESULT_TEXT_CAP;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut output = String::with_capacity(RESULT_TEXT_CAP + RESULT_TRUNCATED_MARKER.len() + 1);
    output.push_str(&text[..boundary]);
    output.push('\n');
    output.push_str(RESULT_TRUNCATED_MARKER);
    output
}

/// Builds a tool specification from one validated remote tool.
pub(crate) fn tool_spec(name: Name, remote: &RemoteTool) -> ToolSpec {
    ToolSpec {
        name,
        description: remote.description.clone().into_boxed_str(),
        parameters: remote.schema.clone(),
        grammar: None,
    }
}

pub(crate) struct MappedTool {
    pub(crate) key: Key,
    pub(crate) plugin: Name,
    pub(crate) declaration: Arc<ServerDecl>,
    pub(crate) remote: String,
    pub(crate) spec: Arc<ToolSpec>,
}

pub(crate) struct ServerEntryTool {
    pub(crate) key: Key,
    pub(crate) plugin: Name,
    pub(crate) declaration: Arc<ServerDecl>,
    pub(crate) spec: Arc<ToolSpec>,
}

impl ServerEntryTool {
    pub(crate) fn new(
        key: Key,
        plugin: Name,
        declaration: Arc<ServerDecl>,
    ) -> Result<Self, McpError> {
        let name = Name::parse_mapped_tool(&fold_tool_name(&key.skill, &key.server, "")).map_err(
            |error| McpError::Protocol {
                code: -32600,
                message: error.to_string(),
            },
        )?;
        let description = format!(
            "Connects to the MCP server \"{}\" declared by the skill \"{}\" and lists its tools. Run this once; the tools appear as {}.{}.<tool> after it returns.",
            key.server, key.skill, key.skill, key.server
        );
        let parameters =
            RawJson::parse(r#"{"type":"object","properties":{},"additionalProperties":false}"#)
                .map_err(|error| McpError::Protocol {
                    code: -32600,
                    message: error.to_string(),
                })?;
        Ok(Self {
            key,
            plugin,
            declaration,
            spec: Arc::new(ToolSpec {
                name,
                description: description.into(),
                parameters,
                grammar: None,
            }),
        })
    }
}

impl Tool for ServerEntryTool {
    fn declaring_extension(&self) -> Option<Name> {
        Some(self.plugin.clone())
    }

    fn name(&self) -> &Name {
        &self.spec.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, args: &RawJson, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        let empty = args
            .decode_as::<Value>()
            .ok()
            .and_then(|value| value.as_object().map(sonic_rs::Object::is_empty));
        if empty != Some(true) {
            return Err(ArgError::message("mcp server entry takes no arguments"));
        }
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let preview = server_preview(&self.key, &self.declaration, "", &call.args);
            if let Err(reason) = cx.authorize(preview).await {
                return ToolOutcome::Err(ToolError::Denied(reason));
            }
            call_mcp(&self.key, "", call.args, cx).await
        })
    }
}

impl Tool for MappedTool {
    fn declaring_extension(&self) -> Option<Name> {
        Some(self.plugin.clone())
    }

    fn name(&self) -> &Name {
        &self.spec.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawJson, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let preview = server_preview(&self.key, &self.declaration, &self.remote, &call.args);
            if let Err(reason) = cx.authorize(preview).await {
                return ToolOutcome::Err(ToolError::Denied(reason));
            }
            call_mcp(&self.key, &self.remote, call.args, cx).await
        })
    }
}

fn server_preview(key: &Key, declaration: &ServerDecl, tool: &str, arguments: &RawJson) -> Preview {
    let mut target = String::new();
    match declaration {
        ServerDecl::Stdio { command, .. } => {
            target.push_str("command:");
            for arg in command {
                target.push(' ');
                target.push_str(arg);
            }
        }
        ServerDecl::Http { url } => {
            target.push_str("url: ");
            target.push_str(url);
        }
    }
    Preview {
        title: format!("MCP {}.{}", key.server, tool).into(),
        body: format!(
            "server: {}\n{target}\ntool: {tool}\narguments: {}",
            key.display(),
            arguments.as_str()
        )
        .into(),
        digest: None,
    }
}

async fn call_mcp(key: &Key, remote: &str, arguments: RawJson, cx: ToolCx<'_>) -> ToolOutcome {
    let services = cx.services();
    let req = McpRequest {
        session: cx.session(),
        server: format!("{}.{}", key.skill, key.server).into(),
        tool: remote.into(),
        arguments,
    };
    match services.mcp(cx.caller(), req).await {
        Ok(response) if response.is_error => ToolOutcome::Err(ToolError::message(response.text)),
        Ok(response) => ToolOutcome::Ok(ToolOutput::from_text(response.text)),
        Err(ServiceError::Cancelled) => ToolOutcome::Interrupted,
        Err(ServiceError::Declined) => ToolOutcome::Err(ToolError::message(
            McpError::Declined {
                plugin: cx.caller().ext().as_str().to_owned(),
            }
            .to_string(),
        )),
        Err(ServiceError::Denied(reason)) => ToolOutcome::Err(ToolError::Denied(reason)),
        Err(error) => ToolOutcome::Err(ToolError::message(error.to_string())),
    }
}
