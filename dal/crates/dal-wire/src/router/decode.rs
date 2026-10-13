//! OpenAI-compatible request decoding and model resolution.
//!
//! Each family decodes its documented fields, ignores its documented
//! unknown-but-meaningless members (reported in `x-dal-ignored`), and rejects
//! its documented unsupported members. Model ids resolve in order: the three
//! harness modes, one `[aliases]` level, then a catalog id in the
//! [`route_id`] rendering; anything else is `model_not_found`.

use std::collections::BTreeMap;

use dal_core::{Family, ModelRoute};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::RouterOptions;

/// A decoded chat-completions request.
#[derive(Clone, Debug, Default)]
pub(crate) struct ChatRequest {
    /// The requested model id.
    pub model: String,
    /// The conversation messages, oldest first.
    pub messages: Vec<Value>,
    /// Whether the client wants server-sent events.
    pub stream: bool,
    /// Whether the client wants a usage chunk.
    pub include_usage: bool,
    /// Declared function tools.
    pub tools: Vec<Value>,
    /// The tool-choice selector.
    pub tool_choice: String,
    /// Output token limits.
    pub max_tokens: Option<u32>,
    /// Sampling controls.
    pub temperature: Option<f64>,
    /// Requested reasoning effort.
    pub reasoning_effort: Option<String>,
}

/// A decoded responses request.
#[derive(Clone, Debug, Default)]
pub(crate) struct ResponsesRequest {
    /// The requested model id.
    pub model: String,
    /// The input items or raw string.
    pub input: Value,
    /// The system instructions.
    pub instructions: String,
    /// Whether the client wants events.
    pub stream: bool,
    /// Declared function tools.
    pub tools: Vec<Value>,
    /// The tool-choice selector.
    pub tool_choice: String,
    /// A previous response id for continuation.
    pub previous_response_id: Option<String>,
    /// Output token limits.
    pub max_output_tokens: Option<u32>,
    /// Sampling controls.
    pub temperature: Option<f64>,
    /// Requested reasoning effort.
    pub reasoning_effort: Option<String>,
}

/// A decoded Anthropic messages request.
#[derive(Clone, Debug, Default)]
pub(crate) struct MessagesRequest {
    /// The requested model id.
    pub model: String,
    /// The conversation messages, oldest first.
    pub messages: Vec<Value>,
    /// The system prompt blocks or text.
    pub system: Value,
    /// The output token limit.
    pub max_tokens: Option<u32>,
    /// Whether the client wants server-sent events.
    pub stream: bool,
    /// Declared tools.
    pub tools: Vec<Value>,
    /// The tool-choice selector.
    pub tool_choice: String,
    /// Sampling controls.
    pub temperature: Option<f64>,
    /// The Anthropic thinking budget in tokens.
    pub thinking_budget: Option<u64>,
}

/// A decoded request body with its ignored members.
pub(crate) struct Decoded<T> {
    /// The decoded request.
    pub request: T,
    /// Unknown members sorted by bytes for `x-dal-ignored`.
    pub ignored: Vec<String>,
}

/// A model resolution failure with its HTTP status and wire code.
#[derive(Clone, Debug)]
pub(crate) struct RouterFail {
    /// The HTTP status.
    pub status: u16,
    /// The wire error code.
    pub code: &'static str,
    /// The human-readable message.
    pub message: String,
}

impl RouterFail {
    /// Builds a 400 failure with a wire code.
    pub(crate) fn bad(code: &'static str, message: String) -> Self {
        Self {
            status: 400,
            code,
            message,
        }
    }

    /// Builds a 401 failure for missing provider credentials.
    pub(crate) fn unauthorized(message: String) -> Self {
        Self {
            status: 401,
            code: "invalid_api_key",
            message,
        }
    }

    /// Builds a 404 failure with a wire code.
    pub(crate) fn missing(code: &'static str, message: String) -> Self {
        Self {
            status: 404,
            code,
            message,
        }
    }

    /// Builds a 409 conflict failure.
    pub(crate) fn busy(message: String) -> Self {
        Self {
            status: 409,
            code: "conflict",
            message,
        }
    }

    /// A 500 server error for a harness turn that failed or was cancelled.
    pub(crate) fn failed(message: String) -> Self {
        Self {
            status: 500,
            code: "server_error",
            message,
        }
    }
}

/// The model target selected for one router request.
#[derive(Clone, Debug)]
pub(crate) enum ResolvedModel {
    /// Run the dal harness with the named mode.
    Harness(crate::router::HarnessMode),
    /// Relay to a provider route; the client runs its own tools.
    Route(ModelRoute),
}

/// Decodes one chat-completions body.
pub(crate) fn decode_chat(body: &Value) -> Result<Decoded<ChatRequest>, RouterFail> {
    let known = [
        "model",
        "messages",
        "stream",
        "stream_options",
        "tools",
        "tool_choice",
        "max_tokens",
        "max_completion_tokens",
        "temperature",
        "reasoning_effort",
        "n",
        "user",
        "metadata",
        "store",
        "seed",
        "presence_penalty",
        "frequency_penalty",
        "parallel_tool_calls",
        "service_tier",
        "stream_options.include_obfuscation",
    ];
    let ignored = super::ignored_members(body, &known);
    let model = required_string(body, "model")?;
    let messages = body
        .get("messages")
        .and_then(|value| value.as_array())
        .map(|items| items.iter().cloned().collect())
        .unwrap_or_default();
    if let Some(number) = body.get("n").and_then(Value::as_i64)
        && number != 1
    {
        return Err(RouterFail::bad(
            "invalid_request",
            r#"field "n" must be 1"#.to_owned(),
        ));
    }
    for member in [
        "logprobs",
        "top_logprobs",
        "audio",
        "prediction",
        "functions",
        "function_call",
    ] {
        if body.get(member).is_some() {
            return Err(RouterFail::bad(
                "invalid_request",
                format!(r#"field "{member}" is not supported by dalgon serve"#),
            ));
        }
    }
    if let Some(format) = body.get("response_format") {
        let text = format
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if text != "text" {
            return Err(RouterFail::bad(
                "invalid_request",
                r#"field "response_format" is not supported by dalgon serve"#.to_owned(),
            ));
        }
    }
    if let Some(modalities) = body.get("modalities").and_then(|value| value.as_array()) {
        let text_only = modalities
            .iter()
            .all(|value| value.as_str() == Some("text"));
        if !text_only {
            return Err(RouterFail::bad(
                "invalid_request",
                r#"field "modalities" is not supported by dalgon serve"#.to_owned(),
            ));
        }
    }
    let include_usage = body
        .get("stream_options")
        .and_then(|options| options.get("include_usage"))
        .and_then(sonic_rs::JsonValueTrait::as_bool)
        .unwrap_or(false);
    Ok(Decoded {
        request: ChatRequest {
            model,
            messages,
            stream: body
                .get("stream")
                .and_then(sonic_rs::JsonValueTrait::as_bool)
                .unwrap_or(false),
            include_usage,
            tools: array_member(body, "tools"),
            tool_choice: string_member(body, "tool_choice", "auto"),
            max_tokens: int_member(body, "max_tokens")
                .or_else(|| int_member(body, "max_completion_tokens")),
            temperature: float_member(body, "temperature"),
            reasoning_effort: opt_string(body, "reasoning_effort"),
        },
        ignored,
    })
}

/// Decodes one responses body.
pub(crate) fn decode_responses(body: &Value) -> Result<Decoded<ResponsesRequest>, RouterFail> {
    let known = [
        "model",
        "input",
        "instructions",
        "stream",
        "tools",
        "tool_choice",
        "previous_response_id",
        "max_output_tokens",
        "temperature",
        "reasoning",
        "store",
        "metadata",
        "include",
        "parallel_tool_calls",
        "service_tier",
        "truncation",
        "prompt_cache_key",
        "safety_identifier",
        "user",
    ];
    let ignored = super::ignored_members(body, &known);
    let model = required_string(body, "model")?;
    if body
        .get("background")
        .and_then(sonic_rs::JsonValueTrait::as_bool)
        .unwrap_or(false)
    {
        return Err(RouterFail::bad(
            "invalid_request",
            r#"field "background" is not supported by dalgon serve"#.to_owned(),
        ));
    }
    if let Some(format) = body.get("text").and_then(|text| text.get("format")) {
        let kind = format
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if kind != "text" {
            return Err(RouterFail::bad(
                "invalid_request",
                r#"field "text.format" is not supported by dalgon serve"#.to_owned(),
            ));
        }
    }
    if let Some(tools) = body.get("tools").and_then(|value| value.as_array()) {
        for tool in tools {
            let kind = tool
                .get("type")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if kind != "function" {
                return Err(RouterFail::bad(
                    "invalid_request",
                    format!(r#"tool type "{kind}" is not supported by dalgon serve"#),
                ));
            }
        }
    }
    let effort = body
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    Ok(Decoded {
        request: ResponsesRequest {
            model,
            input: body.get("input").cloned().unwrap_or(Value::from("")),
            instructions: string_member(body, "instructions", ""),
            stream: body
                .get("stream")
                .and_then(sonic_rs::JsonValueTrait::as_bool)
                .unwrap_or(false),
            tools: array_member(body, "tools"),
            tool_choice: string_member(body, "tool_choice", "auto"),
            previous_response_id: opt_string(body, "previous_response_id"),
            max_output_tokens: int_member(body, "max_output_tokens"),
            temperature: float_member(body, "temperature"),
            reasoning_effort: effort,
        },
        ignored,
    })
}

/// Decodes one Anthropic messages body.
pub(crate) fn decode_messages(body: &Value) -> Result<Decoded<MessagesRequest>, RouterFail> {
    let known = [
        "model",
        "messages",
        "system",
        "max_tokens",
        "stream",
        "tools",
        "tool_choice",
        "temperature",
        "thinking",
        "metadata",
        "top_k",
        "service_tier",
        "container",
    ];
    let ignored = super::ignored_members(body, &known);
    let model = required_string(body, "model")?;
    let budget = body
        .get("thinking")
        .and_then(|thinking| thinking.get("budget_tokens"))
        .and_then(Value::as_u64);
    Ok(Decoded {
        request: MessagesRequest {
            model,
            messages: body
                .get("messages")
                .and_then(|value| value.as_array())
                .map(|items| items.iter().cloned().collect())
                .unwrap_or_default(),
            system: body.get("system").cloned().unwrap_or(Value::from("")),
            max_tokens: int_member(body, "max_tokens"),
            stream: body
                .get("stream")
                .and_then(sonic_rs::JsonValueTrait::as_bool)
                .unwrap_or(false),
            tools: array_member(body, "tools"),
            tool_choice: string_member(body, "tool_choice", "auto"),
            temperature: float_member(body, "temperature"),
            thinking_budget: budget,
        },
        ignored,
    })
}

/// Maps a reasoning effort name to its thinking level; `absent` applies when
/// the request names none.
pub(crate) fn effort_level(
    effort: Option<&str>,
    absent: dal_core::ThinkingLevel,
) -> Result<dal_core::ThinkingLevel, RouterFail> {
    use dal_core::ThinkingLevel;
    match effort {
        None => Ok(absent),
        Some("medium") => Ok(ThinkingLevel::Medium),
        Some("none" | "off") => Ok(ThinkingLevel::Off),
        Some("minimal") => Ok(ThinkingLevel::Minimal),
        Some("low") => Ok(ThinkingLevel::Low),
        Some("high") => Ok(ThinkingLevel::High),
        Some("xhigh") => Ok(ThinkingLevel::Xhigh),
        Some("max") => Ok(ThinkingLevel::Max),
        Some(other) => Err(RouterFail::bad(
            "invalid_request",
            format!(
                r#"field "reasoning_effort" is not supported by dalgon serve: use one of none, off, minimal, low, medium, high, xhigh, max (got "{other}")"#
            ),
        )),
    }
}

/// Maps an Anthropic thinking budget to its thinking level; `absent` applies
/// when the request sets no budget.
pub(crate) fn budget_level(
    budget: Option<u64>,
    absent: dal_core::ThinkingLevel,
) -> dal_core::ThinkingLevel {
    use dal_core::ThinkingLevel;
    match budget {
        None => absent,
        Some(0) => ThinkingLevel::Off,
        Some(1..=2047) => ThinkingLevel::Low,
        Some(16384..) => ThinkingLevel::High,
        Some(_) => ThinkingLevel::Medium,
    }
}

/// Resolves one model id to its harness or relay target.
///
/// Every id [`listed_ids`] produces resolves: harness ids, alias keys, and
/// catalog ids rendered by [`route_id`].
pub(crate) fn resolve_model(
    options: &RouterOptions,
    model: &str,
) -> Result<ResolvedModel, RouterFail> {
    let target = options.aliases.get(model).map_or(model, AsRef::as_ref);
    if let Some(mode) = crate::router::HarnessMode::parse(target) {
        return Ok(ResolvedModel::Harness(mode));
    }
    parse_route_id(target)
        .map(ResolvedModel::Route)
        .ok_or_else(|| {
            RouterFail::bad(
                "model_not_found",
                format!(r#"model "{model}" was not found"#),
            )
        })
}

/// Renders one route as its wire model id: `openai-chat/<model>`,
/// `openai-responses/<model>`, `openai-codex/<model>`, `anthropic/<model>`,
/// or the synthetic or harness id itself.
pub(crate) fn route_id(route: &ModelRoute) -> String {
    match route {
        ModelRoute::Api { family, model } => {
            let prefix = match family {
                Family::Chat => "openai-chat",
                Family::Responses => "openai-responses",
                Family::Codex => "openai-codex",
                Family::Anthropic => "anthropic",
            };
            format!("{prefix}/{model}")
        }
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id.to_string(),
    }
}

/// Parses one wire model id in the [`route_id`] rendering.
pub(crate) fn parse_route_id(id: &str) -> Option<ModelRoute> {
    if let Some(mode) = crate::router::HarnessMode::parse(id) {
        return Some(ModelRoute::Harness {
            id: mode.id().into(),
        });
    }
    let (prefix, name) = id.split_once('/')?;
    let family = match prefix {
        "dalgon" => return None,
        "openai-chat" => Family::Chat,
        "openai-responses" => Family::Responses,
        "openai-codex" => Family::Codex,
        "anthropic" => Family::Anthropic,
        _ => return ModelRoute::synthetic(id).ok(),
    };
    (!name.is_empty()).then(|| ModelRoute::Api {
        family,
        model: name.into(),
    })
}

/// Lists the `GET /v1/models` ids: harness modes, sorted aliases, then the
/// sorted catalog ids rendered by [`route_id`], each listed once. A catalog
/// route whose id parses back to a different route (a synthetic id in a
/// family namespace such as `anthropic/x`) is left out.
pub(crate) fn listed_ids(
    aliases: &BTreeMap<Box<str>, Box<str>>,
    catalog: &[dal_core::ModelInfo],
) -> Vec<String> {
    let mut ids: Vec<String> = [
        crate::router::HarnessMode::Normal,
        crate::router::HarnessMode::EvalFirst,
        crate::router::HarnessMode::EvalOnly,
    ]
    .iter()
    .map(|mode| mode.id().to_owned())
    .collect();
    ids.extend(aliases.keys().map(ToString::to_string));
    let mut routes: Vec<String> = catalog
        .iter()
        .filter(|info| parse_route_id(&route_id(&info.route)).as_ref() == Some(&info.route))
        .map(|info| route_id(&info.route))
        .filter(|id| !ids.contains(id))
        .collect();
    routes.sort();
    routes.dedup();
    ids.extend(routes);
    ids
}

/// Reads a required string member.
fn required_string(body: &Value, name: &str) -> Result<String, RouterFail> {
    body.get(name)
        .and_then(|value| value.as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| RouterFail::bad("invalid_request", format!(r#"field "{name}" is required"#)))
}

/// Reads an optional string member with a default.
fn string_member(body: &Value, name: &str, default: &str) -> String {
    body.get(name)
        .and_then(|value| value.as_str())
        .unwrap_or(default)
        .to_owned()
}

/// Reads an optional string member.
fn opt_string(body: &Value, name: &str) -> Option<String> {
    body.get(name)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

/// Reads an optional unsigned member.
fn int_member(body: &Value, name: &str) -> Option<u32> {
    body.get(name)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

/// Reads an optional float member.
fn float_member(body: &Value, name: &str) -> Option<f64> {
    body.get(name).and_then(Value::as_f64)
}

/// Reads an array member or an empty vector.
fn array_member(body: &Value, name: &str) -> Vec<Value> {
    body.get(name)
        .and_then(|value| value.as_array())
        .map(|items| items.iter().cloned().collect())
        .unwrap_or_default()
}
