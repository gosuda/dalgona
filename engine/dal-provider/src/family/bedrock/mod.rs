//! The Amazon Bedrock family: request lowering, `SigV4` signing, and binary
//! event-stream decoding.
//!
//! The response of `POST <base>/model/<model-id>/converse-stream` is
//! `application/vnd.amazon.eventstream`, a binary frame sequence — never
//! Server-Sent Events. [`eventstream`] owns the framing and both CRC
//! validations; [`sigv4`] owns request signing. This module lowers a
//! [`ModelRequest`] to the Converse JSON body (members in one fixed order,
//! tools in registration order, tool arguments and schemas verbatim from
//! their [`RawJson`]), signs it, and turns the decoded frames into the
//! neutral [`StreamEvent`] grammar: deltas as they arrive, then exactly one
//! `ToolCallsDone`, one `Usage`, and one `Stop` at `messageStop`. A frame
//! whose `:message-type` is `exception` or `error` ends the stream with the
//! typed error its shape maps to through the shared retry table.
//!
//! Route: the Converse API path `model/<id>/converse-stream`, the wire the
//! reference Bedrock clients speak.
//!
//! Cancellation: dropping the decoded stream drops the byte source, which
//! cancels the transfer (proved in [`eventstream`] tests). A decoder that
//! ends without `messageStop` reports [`ProviderError::StreamCut`].
//!
//! Lowering seams, deliberately narrow: thinking blocks are not replayed
//! (Claude-on-Bedrock `reasoningContent` needs the signed anthropic replay
//! payload; [`crate::family::anthropic`] owns that spelling), cache points
//! are not placed (no retention configuration reaches this family yet), and
//! `WireThinking::OpenAi` is rejected like a foreign plan in
//! [`crate::family::chat`]. Blank user text becomes the `<empty>`
//! placeholder the reference clients send instead of a rejected empty
//! block.
//!
//! # Dependencies
//!
//! `SigV4` signing uses `hmac` with `sha2`, and frame validation uses
//! `crc32fast`; `jiff` formats the signing dates and `sonic-rs` encodes the
//! body.

use std::{borrow::Cow, collections::BTreeMap, mem};

use base64::{Engine, engine::general_purpose::STANDARD};
use dal_core::{
    AssistantPart, BlobId, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson, Usage,
};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};

use crate::{
    error::ProviderError,
    retry::{RequestState, RetryDecision, classify},
    stream::{StopReason, StreamEvent, ToolArgs, ToolCall},
    stream_budget::StreamBudget,
    thinking::{AnthropicThinking, ThinkingPlan, WireThinking},
    tool_names::{WIRE_NAME_MAX, wire_name},
};

pub(crate) mod auth;
mod eventstream;
mod process;
mod profile;
mod sigv4;
mod sts;

pub(crate) use eventstream::{Frame, FrameError};
pub(crate) use sigv4::{AmzDate, AwsCredential};

/// The path suffix of the Converse streaming route, relative to the base.
pub(crate) const CONVERSE_STREAM_PATH: &str = "converse-stream";

/// The placeholder the reference Bedrock clients send for a blank user text
/// block, which Bedrock would otherwise reject.
const EMPTY_TEXT_PLACEHOLDER: &str = "<empty>";

/// The default region when neither the entry nor the host names one.
const DEFAULT_REGION: &str = "us-east-1";

/// A request that cannot be lowered to a Bedrock body.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum BedrockBodyError {
    /// The request route is not a `bedrock` model route.
    #[error("a bedrock request needs a bedrock model route, not `{route}`")]
    NotBedrock {
        /// The route id of the request.
        route: Box<str>,
    },
    /// A content part still refers to stored content; the caller resolves
    /// blobs to inline bytes before lowering.
    #[error("content blob {blob_id} must be read into the request before a bedrock body is built")]
    UnresolvedBlob {
        /// The unresolved blob.
        blob_id: BlobId,
    },
    /// The thinking plan was composed for the `OpenAI` effort ladder, which
    /// the Converse wire has no member for.
    #[error("the thinking plan carries an OpenAI effort, not a bedrock thinking fragment")]
    ForeignThinking,
    /// The image part carries a format Bedrock does not accept.
    #[error("image format `{mime}` is not one of png, jpeg, gif, or webp")]
    ImageMime {
        /// The rejected MIME type.
        mime: Box<str>,
    },
    /// The JSON writer failed.
    #[error("bedrock body encoding failed: {message}")]
    Encode {
        /// The writer message.
        message: String,
    },
}

/// Percent-encodes one model id into its path segment: unreserved bytes
/// verbatim, everything else uppercase hex, so `anthropic.claude-4:0`
/// becomes `anthropic.claude-4%3A0` on the wire.
#[must_use]
pub(crate) fn model_id_segment(model_id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(model_id.len());
    for byte in model_id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 0x0F)]));
            }
        }
    }
    out
}

/// The region of an AWS regional runtime host: `bedrock-runtime.<region>`
/// or `bedrock-runtime-fips.<region>` under `amazonaws.com` or
/// `amazonaws.com.cn`, or the dual-stack `bedrock-runtime.<region>.api.aws`.
/// Any other host, including a VPC endpoint, a gateway, and a host with
/// trailing labels, names no region.
fn runtime_host_region(host: &str) -> Option<String> {
    let host = host.to_ascii_lowercase();
    let rest = host.strip_prefix("bedrock-runtime")?;
    let rest = rest.strip_prefix("-fips").unwrap_or(rest);
    let rest = rest.strip_prefix('.')?;
    let region = rest
        .strip_suffix(".amazonaws.com")
        .or_else(|| rest.strip_suffix(".amazonaws.com.cn"))
        .or_else(|| rest.strip_suffix(".api.aws"))?;
    let label = !region.is_empty()
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    label.then(|| region.to_owned())
}

/// The region a request signs for. An AWS regional runtime host names its
/// own region and that region always wins: AWS refuses a credential scope
/// for any other region on its own endpoint, and the caller's region is
/// often only the ambient default. Any other host signs for the configured
/// region, else the default.
#[must_use]
pub(crate) fn region_for(host: &str, configured: Option<&str>) -> String {
    if let Some(region) = runtime_host_region(host) {
        return region;
    }
    configured
        .map(str::trim)
        .filter(|region| !region.is_empty())
        .map_or_else(|| DEFAULT_REGION.to_owned(), str::to_owned)
}

/// The request base of one resolved region for an entry whose table named no
/// `base_url`: a standard regional runtime host follows the region, the way
/// the reference pins its typed endpoint only when no other region applies,
/// so the region label swaps inside the host (preserving the parsed suffix
/// shape) and the result is owned. A matching region, an unusable region, or
/// a host outside the grammar borrows verbatim, so the unchanged path never
/// allocates and the URL target and the signing scope of [`region_for`]
/// agree by construction. An entry whose table named its own `base_url`
/// never calls this: the caller borrows its base and only the signing scope
/// follows the region.
#[must_use]
pub(crate) fn endpoint_for_defaulted_base<'a>(base: &'a str, region: &str) -> Cow<'a, str> {
    if auth::validate_region_label(region, "configured region").is_err() {
        return Cow::Borrowed(base);
    }
    let Ok(url) = url::Url::parse(base) else {
        return Cow::Borrowed(base);
    };
    let Some(host) = url
        .host_str()
        .and_then(|host| relabel_runtime_host(host, region))
    else {
        return Cow::Borrowed(base);
    };
    let mut moved = url.clone();
    if moved.set_host(Some(&host)).is_err() {
        return Cow::Borrowed(base);
    }
    Cow::Owned(moved.to_string())
}

/// The host of a standard regional runtime base with its region label
/// swapped for `region`; `None` keeps the host verbatim, including when the
/// label already names `region` — an identical host is not a rewrite, so
/// the unchanged path borrows and never allocates. The grammar is the one
/// [`runtime_host_region`] parses, so the label lands exactly where the
/// old one was read from.
fn relabel_runtime_host(host: &str, region: &str) -> Option<String> {
    let rest = host.strip_prefix("bedrock-runtime")?;
    let rest = rest.strip_prefix("-fips").unwrap_or(rest);
    let prefix = &host[..host.len() - rest.len()];
    let rest = rest.strip_prefix('.')?;
    let (label, suffix) = [".amazonaws.com.cn", ".amazonaws.com", ".api.aws"]
        .into_iter()
        .find_map(|candidate| rest.strip_suffix(candidate).map(|label| (label, candidate)))?;
    if label == region {
        return None;
    }
    Some(format!("{prefix}.{region}{suffix}"))
}

/// The `host` header value the HTTP client sends for `url`: the host as the
/// URL spells it, plus `:port` when the URL names a port other than the
/// scheme's default. The signature covers this value, so it must match what
/// the transport puts on the wire.
fn host_header(url: &url::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// The full Converse request path: `model/<encoded id>/converse-stream`,
/// percent-encoded once, as the wire sends it.
#[must_use]
pub(crate) fn request_path(model_id: &str) -> String {
    format!(
        "model/{}/{}",
        model_id_segment(model_id),
        CONVERSE_STREAM_PATH
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConverseBody<'a> {
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<SystemBlock<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inference_config: Option<InferenceConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<ToolConfig<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_model_request_fields: Option<AdditionalFields>,
}

#[derive(Serialize)]
struct SystemBlock<'a> {
    text: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InferenceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolConfig<'a> {
    tools: Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireTool<'a> {
    tool_spec: ToolSpec<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolSpec<'a> {
    name: std::borrow::Cow<'a, str>,
    description: &'a str,
    input_schema: InputSchema<&'a RawJson>,
}

#[derive(Serialize)]
struct InputSchema<J> {
    json: J,
}

#[derive(Serialize)]
struct ToolChoice {
    auto: Auto,
}

#[derive(Serialize)]
struct Auto {}

/// The `additionalModelRequestFields` member: the Claude-on-Bedrock
/// thinking fragment, carried beside the Converse schema the way the
/// reference Bedrock clients send it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdditionalFields {
    thinking: ThinkingFragment,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ThinkingFragment {
    Adaptive,
    Enabled { budget_tokens: u32 },
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: Vec<ContentBlock<'a>>,
}

/// One Converse content block. The untagged form emits exactly the block's
/// own member (`text`, `image`, `toolUse`, `toolResult`), the structural
/// tag the Converse schema dispatches on.
#[derive(Serialize)]
#[serde(untagged)]
enum ContentBlock<'a> {
    Text {
        text: std::borrow::Cow<'a, str>,
    },
    Image {
        image: ImageBlock,
    },
    ToolUse {
        #[serde(rename = "toolUse")]
        tool_use: ToolUseBlock<'a>,
    },
    ToolResult {
        #[serde(rename = "toolResult")]
        tool_result: ToolResultBlock<'a>,
    },
}

#[derive(Serialize)]
struct ImageBlock {
    format: &'static str,
    source: ImageSource,
}

#[derive(Serialize)]
struct ImageSource {
    bytes: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolUseBlock<'a> {
    tool_use_id: &'a str,
    name: std::borrow::Cow<'a, str>,
    input: &'a RawJson,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultBlock<'a> {
    tool_use_id: &'a str,
    content: Vec<ContentBlock<'a>>,
    status: &'static str,
}

/// Lowers `request` to the JSON body of `POST <base>/model/<id>/converse-stream`.
///
/// The system text becomes the leading `system` block unless empty. User
/// messages carry text and image parts (blank text becomes the `<empty>`
/// placeholder); assistant messages carry their text parts joined by LF and
/// their calls as `toolUse` blocks with the raw argument JSON; thinking
/// parts are never sent; an assistant message with neither is omitted. A
/// run of consecutive tool results folds into one user message, as Bedrock
/// requires all tool results inside one message, and adjacent same-role
/// messages merge so the wire keeps the role alternation Bedrock demands.
/// Tools keep registration order; a non-empty tool list selects
/// `toolChoice: auto`. The Anthropic-shaped thinking fragment becomes the
/// `additionalModelRequestFields.thinking` member; the `OpenAI` effort
/// fragment is rejected.
///
/// # Errors
///
/// [`BedrockBodyError::NotBedrock`] for a route other than a `bedrock`
/// model route, [`BedrockBodyError::UnresolvedBlob`] for a blob part,
/// [`BedrockBodyError::ForeignThinking`] for an `OpenAI` effort fragment,
/// [`BedrockBodyError::ImageMime`] for an unsupported image format, and
/// [`BedrockBodyError::Encode`] when the writer fails.
pub(crate) fn request_body(
    request: &ModelRequest,
    thinking: &ThinkingPlan,
) -> Result<Vec<u8>, BedrockBodyError> {
    let ModelRoute::Api {
        family: Family::Bedrock,
        ..
    } = &request.model
    else {
        return Err(BedrockBodyError::NotBedrock {
            route: request.model.id().into(),
        });
    };
    let additional = match thinking.wire {
        WireThinking::OpenAi { .. } => return Err(BedrockBodyError::ForeignThinking),
        WireThinking::Anthropic {
            thinking: AnthropicThinking::Enabled { budget_tokens, .. },
            ..
        } => Some(AdditionalFields {
            thinking: ThinkingFragment::Enabled { budget_tokens },
        }),
        WireThinking::Anthropic {
            thinking: AnthropicThinking::Adaptive,
            ..
        } => Some(AdditionalFields {
            thinking: ThinkingFragment::Adaptive,
        }),
        WireThinking::Anthropic { .. } => None,
    };
    let has_tools = !request.tools.is_empty();
    let body = ConverseBody {
        messages: messages(request)?,
        system: (!request.system.is_empty()).then(|| {
            vec![SystemBlock {
                text: &request.system,
            }]
        }),
        inference_config: (thinking.temperature.is_some()
            || request.params.max_output_tokens.is_some())
        .then_some(InferenceConfig {
            max_tokens: request.params.max_output_tokens,
            temperature: thinking.temperature,
        }),
        tool_config: has_tools.then(|| ToolConfig {
            tools: request
                .tools
                .iter()
                .map(|tool| WireTool {
                    tool_spec: ToolSpec {
                        name: wire_name(&tool.name, WIRE_NAME_MAX),
                        description: &tool.description,
                        input_schema: InputSchema {
                            json: &tool.parameters,
                        },
                    },
                })
                .collect(),
            tool_choice: Some(ToolChoice { auto: Auto {} }),
        }),
        additional_model_request_fields: additional,
    };
    sonic_rs::to_vec(&body).map_err(|error| BedrockBodyError::Encode {
        message: error.to_string(),
    })
}

fn messages(request: &ModelRequest) -> Result<Vec<WireMessage<'_>>, BedrockBodyError> {
    let mut messages: Vec<WireMessage<'_>> = Vec::with_capacity(request.context.len());
    for item in request.context.iter() {
        match item {
            ContextItem::User { parts } => {
                let content = user_content(parts)?;
                append(&mut messages, "user", content);
            }
            ContextItem::Assistant { parts, .. } => {
                let Some(message) = assistant(parts) else {
                    continue;
                };
                append(&mut messages, "assistant", message.content);
            }
            ContextItem::ToolResult {
                call,
                parts,
                is_error,
                ..
            } => {
                let mut content = tool_result_content(parts)?;
                if content.is_empty() {
                    content.push(text_block(EMPTY_TEXT_PLACEHOLDER));
                }
                append(
                    &mut messages,
                    "user",
                    vec![ContentBlock::ToolResult {
                        tool_result: ToolResultBlock {
                            tool_use_id: call.as_str(),
                            content,
                            status: if *is_error { "error" } else { "success" },
                        },
                    }],
                );
            }
        }
    }
    Ok(messages)
}

/// Merges `content` into the last message when Bedrock's role alternation
/// allows the fold, else opens a new message.
fn append<'a>(
    messages: &mut Vec<WireMessage<'a>>,
    role: &'static str,
    content: Vec<ContentBlock<'a>>,
) {
    if content.is_empty() {
        return;
    }
    match messages.last_mut() {
        Some(last) if last.role == role => last.content.extend(content),
        _ => messages.push(WireMessage { role, content }),
    }
}

fn text_block(text: &str) -> ContentBlock<'_> {
    ContentBlock::Text {
        text: std::borrow::Cow::Borrowed(text),
    }
}

fn user_content(parts: &[Part]) -> Result<Vec<ContentBlock<'_>>, BedrockBodyError> {
    let mut content = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } if !text.is_empty() => content.push(ContentBlock::Text {
                text: std::borrow::Cow::Borrowed(text),
            }),
            Part::Text { .. } => {}
            Part::Image { mime, bytes } => content.push(ContentBlock::Image {
                image: image_block(mime, bytes)?,
            }),
            Part::Blob { blob_id, .. } => {
                return Err(BedrockBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    if content.is_empty() {
        content.push(text_block(EMPTY_TEXT_PLACEHOLDER));
    }
    Ok(content)
}

fn assistant(parts: &[AssistantPart]) -> Option<WireMessage<'_>> {
    let mut texts: Vec<&str> = Vec::new();
    let mut content = Vec::new();
    for part in parts {
        match part {
            AssistantPart::Text { text, .. } => texts.push(text),
            AssistantPart::Thinking { .. } => {}
            AssistantPart::ToolCall {
                call, name, args, ..
            } => content.push(ContentBlock::ToolUse {
                tool_use: ToolUseBlock {
                    tool_use_id: call.as_str(),
                    name: wire_name(name, WIRE_NAME_MAX),
                    input: args,
                },
            }),
        }
    }
    if !texts.is_empty() {
        content.insert(
            0,
            ContentBlock::Text {
                text: std::borrow::Cow::Owned(texts.join("\n")),
            },
        );
    }
    if content.is_empty() {
        return None;
    }
    Some(WireMessage {
        role: "assistant",
        content,
    })
}

fn tool_result_content(parts: &[Part]) -> Result<Vec<ContentBlock<'_>>, BedrockBodyError> {
    let mut content = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } if !text.is_empty() => content.push(ContentBlock::Text {
                text: std::borrow::Cow::Borrowed(text),
            }),
            Part::Text { .. } => {}
            Part::Image { mime, bytes } => content.push(ContentBlock::Image {
                image: image_block(mime, bytes)?,
            }),
            Part::Blob { blob_id, .. } => {
                return Err(BedrockBodyError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok(content)
}

fn image_block(mime: &str, bytes: &[u8]) -> Result<ImageBlock, BedrockBodyError> {
    let format = match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        other => {
            return Err(BedrockBodyError::ImageMime { mime: other.into() });
        }
    };
    Ok(ImageBlock {
        format,
        source: ImageSource {
            bytes: STANDARD.encode(bytes),
        },
    })
}

/// One lowered request: the target, the signed header set, and the body.
#[must_use]
pub(crate) struct BedrockWire {
    /// The final URL, origin, path prefix, and query included.
    pub(crate) url: url::Url,
    /// Headers in send order; names are lowercase. Every one is either
    /// covered by the signature or the signature itself.
    pub(crate) headers: Vec<(String, String)>,
    /// The JSON body.
    pub(crate) body: Vec<u8>,
}

impl BedrockWire {
    /// Attaches the headers and the body to a `POST` of the URL; the caller
    /// passes the result to [`crate::http::send`].
    pub(crate) fn into_request(self, client: &reqwest::Client) -> reqwest::RequestBuilder {
        let mut request = client.post(self.url);
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        request.body(self.body)
    }
}

impl std::fmt::Debug for BedrockWire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let secret = name == "authorization" || name == "x-amz-security-token";
                (
                    name.as_str(),
                    if secret { "<redacted>" } else { value.as_str() },
                )
            })
            .collect();
        formatter
            .debug_struct("BedrockWire")
            .field("url", &self.url.as_str())
            .field("headers", &headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Signs and addresses one request body against `base`.
///
/// `base` is the provider entry's base URL (VPC endpoint or gateway
/// included); the plain-HTTP rule applies through
/// [`crate::http::check_base_url`]. An AWS regional runtime host names its
/// own signing region; any other host signs for `region`, else the
/// default (see [`region_for`]). The `host` header is signed with the port
/// the URL names, as the transport sends it. `now_unix` is the caller's
/// clock, used both for the `x-amz-date` pair and for the credential check.
///
/// # Errors
///
/// [`crate::http::endpoint`] failures for a base URL or request path that
/// fails the endpoint rules ([`ProviderError::PlainHttp`] for plain HTTP off
/// loopback, [`ProviderError::Transport`] otherwise), and
/// [`ProviderError::Status`] with status 401 for a credential that is
/// expired or cannot sign.
pub(crate) fn build(
    request: &ModelRequest,
    thinking: &ThinkingPlan,
    base: &str,
    region: Option<&str>,
    credential: &AwsCredential,
    now_unix: i64,
) -> Result<BedrockWire, ProviderError> {
    let ModelRoute::Api { model, .. } = &request.model else {
        return Err(ProviderError::InvalidRequest {
            message: String::from("a bedrock request needs a direct model route"),
        });
    };
    build_signed(
        &request_path(model),
        request_body(request, thinking).map_err(bedrock_body_error)?,
        base,
        region,
        credential,
        now_unix,
    )
}

/// The signer-and-address path of [`build`].
fn build_signed(
    path: &str,
    body: Vec<u8>,
    base: &str,
    region: Option<&str>,
    credential: &AwsCredential,
    now_unix: i64,
) -> Result<BedrockWire, ProviderError> {
    // The shared join the bearer route uses: one route builder, so both
    // routes address the same URL for the same base and model.
    let url = crate::http::endpoint(Family::Bedrock, base, path)?;
    let host = host_header(&url);
    let region = region_for(
        url.host_str().unwrap_or_default().trim_end_matches('.'),
        region,
    );
    credential
        .verify_usable(now_unix)
        .map_err(|error| ProviderError::Status {
            family: Family::Bedrock,
            status: 401,
            message: error.to_string(),
        })?;
    let signed = sigv4::SignedRequest {
        method: "POST",
        host: &host,
        path: url.path(),
        query: url.query(),
        headers: &[
            ("content-type", String::from("application/json")),
            ("accept", String::from("application/vnd.amazon.eventstream")),
        ],
        service: sigv4::SERVICE,
        region: &region,
        credential,
        date: AmzDate::from_unix(now_unix),
    }
    .sign(&body);
    let mut headers = vec![
        (
            String::from("content-type"),
            String::from("application/json"),
        ),
        (
            String::from("accept"),
            String::from("application/vnd.amazon.eventstream"),
        ),
        (String::from("x-amz-date"), signed.amz_date),
        (String::from("x-amz-content-sha256"), signed.content_sha256),
    ];
    if let Some(token) = signed.security_token {
        headers.push((String::from("x-amz-security-token"), token));
    }
    headers.push((String::from("authorization"), signed.authorization));
    Ok(BedrockWire { url, headers, body })
}

/// Maps a body error onto the neutral error, the way the transport layers
/// of the other families do.
fn bedrock_body_error(error: BedrockBodyError) -> ProviderError {
    match error {
        BedrockBodyError::UnresolvedBlob { blob_id } => ProviderError::UnresolvedBlob { blob_id },
        other => ProviderError::InvalidRequest {
            message: other.to_string(),
        },
    }
}

/// Whether the decoder can take more input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Progress {
    /// The response is still open.
    Open,
    /// The terminal was emitted, or an error was returned; later input is
    /// ignored.
    Ended,
}

/// The in-stream HTTP status of a Converse exception shape, keyed by the
/// lowercased shape name. Exception frames ride inside an HTTP 200 stream,
/// so the shape name is the only evidence of what failed upstream; the
/// values come from the bedrock-runtime service model, and an unknown shape
/// keeps 400 so an unrecognized rejection never retries by accident.
fn exception_status(shape: &str) -> u16 {
    match shape.trim().to_ascii_lowercase().as_str() {
        "accessdeniedexception" => 403,
        "internalserverexception" => 500,
        "modelerrorexception" | "modelstreamerrorexception" => 424,
        "modelnotreadyexception" | "throttlingexception" => 429,
        "modeltimeoutexception" => 408,
        "resourcenotfoundexception" => 404,
        "serviceunavailableexception" => 503,
        // validation, conflict, service-quota, and unknown shapes are
        // client-side rejections the retry table must never replay.
        _ => 400,
    }
}

/// Maps a wire Converse stop reason.
fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        "guardrail_intervened" | "content_filtered" => StopReason::Refusal,
        other => StopReason::Other(other.to_owned()),
    }
}

fn protocol(detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        family: Family::Bedrock,
        detail: detail.into(),
    }
}

fn parse<'a, T: Deserialize<'a>>(payload: &'a [u8], kind: &str) -> Result<T, ProviderError> {
    sonic_rs::from_slice(payload)
        .map_err(|error| protocol(format!("malformed {kind} event: {error}")))
}

/// Usage members as last reported by `metadata`; `None` when never
/// reported.
#[derive(Debug, Default)]
struct UsageState {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
}

impl UsageState {
    fn merge(&mut self, wire: WireUsage) {
        for (slot, value) in [
            (&mut self.input, wire.input),
            (&mut self.output, wire.output),
            (&mut self.cache_read, wire.cache_read_input),
            (&mut self.cache_write, wire.cache_write_input),
        ] {
            if value.is_some() {
                *slot = value;
            }
        }
    }

    /// The core counters. Bedrock's `inputTokens` already includes the
    /// cache-read tokens, so the input counter takes it verbatim instead of
    /// re-adding the cache reads the way the Anthropic decoder does.
    fn usage(&self) -> Usage {
        Usage {
            input_tokens: self.input.unwrap_or(0),
            cached_input_tokens: self.cache_read.unwrap_or(0),
            output_tokens: self.output.unwrap_or(0),
            reasoning_tokens: None,
            cache_write_tokens: self.cache_write.unwrap_or(0),
            cost_usd: None,
            premium_requests: None,
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
struct WireUsage {
    #[serde(rename = "inputTokens")]
    input: Option<u64>,
    #[serde(rename = "outputTokens")]
    output: Option<u64>,
    #[serde(rename = "cacheReadInputTokens")]
    cache_read_input: Option<u64>,
    #[serde(rename = "cacheWriteInputTokens")]
    cache_write_input: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartEvent {
    role: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockStart<'a> {
    content_block_index: u64,
    #[serde(default, borrow)]
    start: Option<StartFields<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartFields<'a> {
    #[serde(borrow)]
    tool_use: Option<ToolUseStart<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolUseStart<'a> {
    tool_use_id: Option<String>,
    name: Option<String>,
    /// The initial arguments of a call that opened with its input already
    /// complete; borrowed from the frame payload, copied into the assembly.
    #[serde(default, borrow)]
    input: Option<sonic_rs::LazyValue<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockDelta {
    content_block_index: u64,
    delta: DeltaFields,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeltaFields {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool_use: Option<ToolUseDelta>,
    #[serde(default)]
    reasoning_content: Option<ReasoningDelta>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolUseDelta {
    #[serde(default)]
    input: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReasoningDelta {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockStop {
    content_block_index: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopEvent {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataEvent {
    #[serde(default)]
    usage: Option<WireUsage>,
}

/// One tool call, from `contentBlockStart` to the terminal group.
///
/// Its `contentBlockStop` closes the arguments; the call itself stays until
/// `messageStop`, because `ToolCallsDone` carries every call of the response.
#[derive(Debug)]
struct Call {
    id: String,
    name: String,
    args: Vec<u8>,
    /// The block's `contentBlockStop` arrived; later argument fragments for
    /// the index are ignored.
    closed: bool,
}

/// The incremental decoder of one Bedrock Converse stream.
///
/// Feed it every CRC-validated [`Frame`] in order with
/// [`BedrockDecoder::feed`], then call [`BedrockDecoder::end`] at end of
/// input. It emits at most one terminal: `ToolCallsDone`, `Usage`, and
/// `Stop` together, or one error. Once ended, it ignores all further input.
#[derive(Debug)]
pub(crate) struct BedrockDecoder {
    provider: Box<str>,
    model: Box<str>,
    budget: StreamBudget,
    /// Every tool call of the response by content-block index, closed or
    /// not, held until the terminal group.
    calls: BTreeMap<u64, Call>,
    usage: UsageState,
    stop: Option<StopReason>,
    emitted: bool,
    ended: bool,
}

impl BedrockDecoder {
    /// A decoder for one response of `model` on the provider id `provider`;
    /// both only name the request in mapped in-stream errors.
    #[must_use]
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            budget: StreamBudget::default(),
            calls: BTreeMap::new(),
            usage: UsageState::default(),
            stop: None,
            emitted: false,
            ended: false,
        }
    }

    /// Decodes one frame, appending events to `out`.
    ///
    /// An `exception` or `error` frame ends the stream with its typed
    /// error; unknown event types and block shapes are ignored.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Protocol`] for frames that violate the Converse
    /// grammar, and the mapped server error for exception frames. The
    /// decoder is ended after an error.
    pub(crate) fn feed(
        &mut self,
        frame: &Frame,
        out: &mut Vec<StreamEvent>,
    ) -> Result<Progress, ProviderError> {
        if self.ended {
            return Ok(Progress::Ended);
        }
        let start = out.len();
        let charged = frame.payload.len() + frame.headers.len() + 16;
        let result = self
            .budget
            .admit_event(charged)
            .and_then(|()| self.step(frame, out));
        self.emitted |= out.len() > start;
        if !matches!(result, Ok(Progress::Open)) {
            self.ended = true;
        }
        result
    }

    /// Ends the input: a response with a `messageStop` finishes; an ended
    /// decoder emits nothing.
    ///
    /// # Errors
    ///
    /// [`ProviderError::StreamCut`] when no `messageStop` arrived.
    pub(crate) fn end(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if mem::replace(&mut self.ended, true) {
            return Ok(());
        }
        if self.stop.is_none() {
            return Err(ProviderError::StreamCut);
        }
        self.finish(out);
        Ok(())
    }

    fn step(
        &mut self,
        frame: &Frame,
        out: &mut Vec<StreamEvent>,
    ) -> Result<Progress, ProviderError> {
        let Some(message_type) = frame.string_header(":message-type") else {
            return Err(protocol("event-stream frame carries no :message-type"));
        };
        match message_type {
            "event" => {
                let Some(event_type) = frame.string_header(":event-type") else {
                    return Err(protocol("event frame carries no :event-type"));
                };
                self.event(event_type, &frame.payload, out)?;
                Ok(Progress::Open)
            }
            "exception" => {
                let shape = frame
                    .string_header(":exception-type")
                    .unwrap_or("Exception");
                Err(self.exception(shape, &frame.payload))
            }
            "error" => {
                let code = frame.string_header(":error-code").unwrap_or("UnknownError");
                let payload = frame
                    .string_header(":error-message")
                    .map_or(frame.payload.as_slice(), str::as_bytes);
                Err(self.exception(code, payload))
            }
            other => Err(protocol(format!("unknown :message-type `{other}`"))),
        }
    }

    fn event(
        &mut self,
        event_type: &str,
        payload: &[u8],
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        match event_type {
            "messageStart" => {
                let start: StartEvent = parse(payload, "messageStart")?;
                match start.role.as_deref() {
                    None | Some("assistant") => Ok(()),
                    Some(other) => Err(protocol(format!(
                        "messageStart named role `{other}`, not `assistant`"
                    ))),
                }
            }
            "contentBlockStart" => {
                self.block_start(&parse::<BlockStart<'_>>(payload, "contentBlockStart")?, out)
            }
            "contentBlockDelta" => {
                self.block_delta(&parse::<BlockDelta>(payload, "contentBlockDelta")?, out);
                Ok(())
            }
            "contentBlockStop" => {
                self.block_stop(
                    parse::<BlockStop>(payload, "contentBlockStop")?.content_block_index,
                );
                Ok(())
            }
            "messageStop" => {
                let stop: StopEvent = parse(payload, "messageStop")?;
                let reason = match stop.stop_reason {
                    Some(reason) => stop_reason(&reason),
                    None => StopReason::EndTurn,
                };
                self.stop = Some(reason);
                Ok(())
            }
            "metadata" => {
                let metadata: MetadataEvent = parse(payload, "metadata")?;
                if let Some(usage) = metadata.usage {
                    self.usage.merge(usage);
                }
                Ok(())
            }
            // Bedrock may add event types; unknown ones are ignored.
            _ => Ok(()),
        }
    }

    fn block_start(
        &mut self,
        start: &BlockStart<'_>,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let index = start.content_block_index;
        if self.calls.contains_key(&index) {
            return Err(protocol(format!("content block {index} started twice")));
        }
        let Some(tool_use) = start
            .start
            .as_ref()
            .and_then(|fields| fields.tool_use.as_ref())
        else {
            // A start without a tool call opens nothing; text and
            // reasoning deltas need no start at all.
            return Ok(());
        };
        self.budget.admit_item()?;
        let id = tool_use
            .tool_use_id
            .clone()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                protocol(format!(
                    "content block {index} opened a tool call without an id"
                ))
            })?;
        let name = tool_use
            .name
            .clone()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                protocol(format!(
                    "content block {index} opened a tool call without a name"
                ))
            })?;
        let mut args = Vec::new();
        if let Some(input) = &tool_use.input {
            let text = input.as_raw_str();
            if !text.is_empty() && text != "null" {
                args.extend_from_slice(text.as_bytes());
            }
        }
        out.push(StreamEvent::ToolCallStarted {
            id: id.clone(),
            name: name.clone(),
        });
        if !args.is_empty() {
            out.push(StreamEvent::ToolArgsDelta {
                id: id.clone(),
                fragment: args.clone(),
            });
        }
        self.calls.insert(
            index,
            Call {
                id,
                name,
                args,
                closed: false,
            },
        );
        Ok(())
    }

    fn block_delta(&mut self, delta: &BlockDelta, out: &mut Vec<StreamEvent>) {
        let index = delta.content_block_index;
        if let Some(text) = delta.delta.text.as_deref().filter(|text| !text.is_empty()) {
            out.push(StreamEvent::TextDelta {
                text: text.to_owned(),
            });
        }
        if let Some(fragment) = delta
            .delta
            .tool_use
            .as_ref()
            .and_then(|tool_use| tool_use.input.as_deref())
            .filter(|fragment| !fragment.is_empty())
            && let Some(call) = self.calls.get_mut(&index)
            && !call.closed
        {
            call.args.extend_from_slice(fragment.as_bytes());
            out.push(StreamEvent::ToolArgsDelta {
                id: call.id.clone(),
                fragment: fragment.as_bytes().to_vec(),
            });
        }
        if let Some(reasoning) = delta
            .delta
            .reasoning_content
            .as_ref()
            .and_then(|reasoning| reasoning.text.as_deref())
            .filter(|text| !text.is_empty())
        {
            out.push(StreamEvent::ReasoningDelta {
                text: reasoning.to_owned(),
            });
        }
    }

    /// Closes the arguments of the tool call at `index`; the call stays for
    /// the terminal group, as the reference Bedrock clients do. A stop
    /// without a start closes nothing: an empty text block sends
    /// `contentBlockStop` without ever sending a delta, and the reference
    /// clients ignore it the same way.
    fn block_stop(&mut self, index: u64) {
        if let Some(call) = self.calls.get_mut(&index) {
            call.closed = true;
        }
    }

    /// Emits the terminal group. Calls close in index order; empty argument
    /// text is `{}`, and every call of a `MaxTokens` response is truncated.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) {
        let reason = self.stop.take().unwrap_or(StopReason::EndTurn);
        let truncated = reason == StopReason::MaxTokens;
        let mut calls = Vec::new();
        for (_, call) in mem::take(&mut self.calls) {
            calls.push(ToolCall {
                args: if truncated {
                    ToolArgs::Truncated
                } else if call.args.is_empty() {
                    ToolArgs::from_bytes(b"{}")
                } else {
                    ToolArgs::from_bytes(&call.args)
                },
                id: call.id,
                name: call.name,
            });
        }
        out.push(StreamEvent::ToolCallsDone { calls });
        out.push(StreamEvent::Usage {
            usage: self.usage.usage(),
        });
        out.push(StreamEvent::Stop { reason });
    }

    /// Maps an in-stream exception shape through the shared retry table.
    /// The decoder returns only the error value; the request lifecycle owns
    /// retries.
    fn exception(&self, code: &str, payload: &[u8]) -> ProviderError {
        let message = sonic_rs::from_slice::<ExceptionPayload>(payload)
            .ok()
            .and_then(|payload| payload.message)
            .unwrap_or_else(|| String::from_utf8_lossy(payload).into_owned());
        if let Some(overflow) = ProviderError::context_overflow(Family::Bedrock, code, &message) {
            return overflow;
        }
        let request = RequestState {
            family: Family::Bedrock,
            provider: &self.provider,
            model: &self.model,
            oauth: false,
            refreshed: false,
            delivered: self.emitted,
        };
        match classify(Some(exception_status(code)), Some(code), &message, &request) {
            RetryDecision::Retry(error) | RetryDecision::Fail(error) => error,
            // SigV4 keys do not refresh; kept total.
            RetryDecision::RefreshOnce => ProviderError::SignInExpired {
                provider: self.provider.to_string(),
            },
        }
    }
}

#[derive(Deserialize)]
struct ExceptionPayload {
    #[serde(default)]
    message: Option<String>,
}

/// Decodes a Bedrock event-stream frame source into neutral events.
///
/// A framing failure ends the stream with
/// [`ProviderError::Protocol`] bound to [`Family::Bedrock`]; end of input
/// ends it through [`BedrockDecoder::end`]. Nothing is read after the
/// terminal.
pub(crate) fn decode_frames<S>(
    frames: S,
    decoder: BedrockDecoder,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Result<Frame, FrameError>> + Send + 'static,
{
    let state = (Box::pin(frames), decoder, std::collections::VecDeque::new());
    stream::unfold(state, |(mut frames, mut decoder, mut queue)| async move {
        loop {
            if let Some(item) = queue.pop_front() {
                return Some((item, (frames, decoder, queue)));
            }
            if decoder.ended {
                return None;
            }
            let mut out = Vec::new();
            let result = match frames.next().await {
                Some(Ok(frame)) => decoder.feed(&frame, &mut out).map(|_progress| ()),
                Some(Err(error)) => {
                    decoder.ended = true;
                    Err(protocol(error.to_string()))
                }
                None => decoder.end(&mut out),
            };
            queue.extend(out.into_iter().map(Ok));
            if let Err(error) = result {
                queue.push_back(Err(error));
            }
        }
    })
}

/// Decodes the raw bytes of one Bedrock response body into neutral events:
/// binary event-stream framing with both CRCs validated, then the Converse
/// event grammar. The transport adapter wraps this with its own
/// cancellation action and secret redaction, as it does for every family.
pub(crate) fn decode_stream<S>(
    bytes: S,
    provider: &str,
    model: &str,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Vec<u8>> + Send + 'static,
{
    decode_frames(
        eventstream::decode_stream(bytes),
        BedrockDecoder::new(provider, model),
    )
}

#[cfg(test)]
mod tests;
