//! Provider request assembly: contexts, wire builders, and parameter shaping.

use dal_core::{
    Caps, ContextItem, Family, ModelInfo, ModelRequest, ModelRoute, SessionId, ThinkingLevel,
};
use tokio_util::sync::CancellationToken;

use super::api::Http;
use super::config::{AuthStyle, ProviderEntry};
use crate::{
    auth::credential::{Credential, OAuthCredential},
    catalog::{CatalogEntry, ResolvedModel},
    compact::{self, CompactedHistory},
    error::ProviderError,
    family::{
        self,
        anthropic::{AnthropicAuth, AnthropicRequest},
        codex::CodexRequest,
        responses::ResponsesRequest,
    },
    http::{self, Exchange},
    lifecycle::AttemptFailure,
    stream::{EventStream, NoticeSink},
    thinking::{
        self, BeforeRequestPatch, Effort, RequestLimits, ThinkingNotice, ThinkingPlan, WireThinking,
    },
    tool_names::{ANTHROPIC_OAUTH_NAME_MAX, ToolNames, WIRE_NAME_MAX},
};

use super::transport as transport_mod;

#[derive(Clone)]
pub(crate) struct StreamContext {
    pub(crate) family: Family,
    pub(crate) provider: Box<str>,
    pub(crate) base_url: Box<str>,
    pub(crate) auth: AuthStyle,
    pub(crate) model: Box<str>,
    pub(crate) entry: CatalogEntry,
    pub(crate) request: ModelRequest,
    pub(crate) thinking: ThinkingPlan,
    pub(crate) session: SessionId,
    pub(crate) client: reqwest::Client,
    pub(crate) user_agent: Box<str>,
}

pub(crate) fn stream_context(
    http: &Http,
    request: &ModelRequest,
    thinking: ThinkingPlan,
    session: SessionId,
) -> StreamContext {
    StreamContext {
        family: http.entry.family,
        provider: http.entry.id.clone(),
        base_url: http.entry.base_url.clone(),
        auth: http.entry.auth,
        model: http.resolved.entry.id.clone(),
        entry: http.resolved.entry.clone(),
        request: request.clone(),
        thinking,
        session,
        client: http.client.clone(),
        user_agent: http.user_agent.clone(),
    }
}
#[derive(Clone)]
pub(crate) struct CompactContext {
    pub(crate) family: Family,
    pub(crate) provider: ProviderEntry,
    pub(crate) resolved: ResolvedModel,
    pub(crate) request: ModelRequest,
    pub(crate) thinking: ThinkingPlan,
    pub(crate) session: SessionId,
    pub(crate) client: reqwest::Client,
    pub(crate) user_agent: Box<str>,
}

pub(crate) async fn stream_attempt(
    context: StreamContext,
    credential: Credential,
    cancel: CancellationToken,
) -> Result<Option<EventStream>, AttemptFailure> {
    if context.family == Family::Codex {
        let Credential::OAuth(oauth) = &credential else {
            return Err(AttemptFailure::Provider(ProviderError::NoCredentials {
                provider: context.provider.to_string(),
            }));
        };
        let wire = build_codex_wire(
            &context,
            &context.request,
            &context.thinking,
            oauth,
            context.session,
        )
        .map_err(AttemptFailure::Provider)?;
        return tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(None),
            result = family::codex::https(&context.client, &context.base_url, wire) => {
                result.map(Some)
            }
        };
    }

    let (request, user_agent, secrets, replay_prefix) =
        build_stream_request(&context, &credential).map_err(AttemptFailure::Provider)?;
    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(None),
        response = http::send(
            context.family,
            request,
            &user_agent,
            Exchange::Stream,
            tokio::time::sleep,
        ) => response.map_err(AttemptFailure::Provider)?,
    };
    if !response.status().is_success() {
        return match status_failure(response, context.family, &credential, &secrets, &cancel)
            .await?
        {
            Some(failure) => Err(failure),
            None => Ok(None),
        };
    }
    Ok(Some(transport_mod::decode_response(
        response,
        context.family,
        &context.provider,
        &context.model,
        matches!(&credential, Credential::OAuth(_)),
        replay_prefix.as_deref(),
        secrets,
    )))
}

/// A built streaming request with its user agent, redaction secrets, and the
/// replay binding that signed thinking blocks in its response must carry.
type StreamRequestParts = (
    reqwest::RequestBuilder,
    Box<str>,
    Vec<Box<str>>,
    Option<Box<str>>,
);

pub(crate) fn build_stream_request(
    context: &StreamContext,
    credential: &Credential,
) -> Result<StreamRequestParts, ProviderError> {
    let route_family = context.family;
    let user_agent = context.user_agent.clone();
    let secrets = credential_secrets(credential);
    match route_family {
        Family::Chat => {
            let body = family::chat::request_body(&context.request, &context.thinking).map_err(
                |error| ProviderError::InvalidRequest {
                    message: error.to_string(),
                },
            )?;
            let url = http::endpoint(route_family, &context.base_url, family::chat::ENDPOINT_PATH)?;
            let (header, value) =
                auth_header(context.auth, credential, route_family, &context.provider)?;
            Ok((
                context.client.post(url).header(header, value).body(body),
                user_agent,
                secrets,
                None,
            ))
        }
        Family::Responses => {
            let wire = build_responses_wire_from_context(context, credential)?;
            let url = http::endpoint(route_family, &context.base_url, "responses")?;
            let mut request = context.client.post(url);
            if let Some((name, value)) = &wire.auth_header {
                request = request.header(*name, value);
            }
            Ok((
                request.body(wire.body),
                wire.user_agent.into(),
                secrets,
                None,
            ))
        }
        Family::Codex => Err(ProviderError::InvalidRequest {
            message: String::from("Codex requests use the dedicated HTTPS adapter"),
        }),
        Family::Anthropic => {
            let mut wire = build_anthropic_wire(
                &context.request,
                &context.entry,
                context.auth,
                &context.thinking,
                credential,
                false,
            )?;
            let url = http::endpoint(route_family, &context.base_url, wire.path)?;
            let user_agent = wire
                .user_agent
                .as_deref()
                .unwrap_or(context.user_agent.as_ref())
                .to_owned()
                .into_boxed_str();
            let replay_prefix = std::mem::take(&mut wire.prefix);
            Ok((
                wire.into_request(&context.client, url),
                user_agent,
                secrets,
                Some(replay_prefix),
            ))
        }
    }
}

pub(crate) fn build_responses_wire_from_context(
    context: &StreamContext,
    credential: &Credential,
) -> Result<family::responses::ResponsesWire, ProviderError> {
    family::responses::wire(&ResponsesRequest {
        request: &context.request,
        thinking: context.thinking.wire,
        reasoning_summary: context.entry.supports_reasoning_summaries,
        auth: context.auth,
        credential,
        session_id: context.session,
        user_agent: &context.user_agent,
    })
}

pub(crate) fn build_codex_wire(
    context: &StreamContext,
    request: &ModelRequest,
    thinking: &ThinkingPlan,
    credential: &OAuthCredential,
    session: SessionId,
) -> Result<family::codex::CodexWire, ProviderError> {
    family::codex::build(&CodexRequest {
        request,
        thinking: thinking.wire,
        reasoning_summaries: context.entry.supports_reasoning_summaries,
        credential,
        session_id: session,
        user_agent: &context.user_agent,
    })
}

pub(crate) fn build_anthropic_wire(
    request: &ModelRequest,
    entry: &CatalogEntry,
    auth_style: AuthStyle,
    thinking: &ThinkingPlan,
    credential: &Credential,
    summarize: bool,
) -> Result<family::anthropic::AnthropicWire, ProviderError> {
    let (thinking_fragment, effort) = match thinking.wire {
        WireThinking::Anthropic { thinking, effort } => (thinking, effort),
        WireThinking::OpenAi { .. } => {
            return Err(ProviderError::InvalidRequest {
                message: String::from("the thinking plan does not belong to Anthropic"),
            });
        }
    };
    let auth = match credential {
        Credential::ApiKey { key } => match auth_style {
            AuthStyle::XApiKey => AnthropicAuth::ApiKey(key.expose()),
            AuthStyle::Bearer => AnthropicAuth::Bearer(key.expose()),
        },
        Credential::OAuth(oauth) => AnthropicAuth::ClaudeOAuth {
            access_token: oauth.access_token.expose(),
            version: crate::claude_fingerprint::CLAUDE_CODE_VERSION,
        },
        Credential::None => {
            return Err(ProviderError::NoCredentials {
                provider: entry.provider.to_string(),
            });
        }
    };
    let request = AnthropicRequest {
        request,
        max_output: entry.max_output,
        thinking: thinking_fragment,
        effort,
        temperature: thinking.temperature,
        compaction: None,
        summarize,
        display_supported: entry.display_supported,
    };
    family::anthropic::build(&request, auth)
}

pub(crate) async fn status_failure(
    response: reqwest::Response,
    family: Family,
    credential: &Credential,
    extra_secrets: &[Box<str>],
    cancel: &CancellationToken,
) -> Result<Option<AttemptFailure>, AttemptFailure> {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(String::from);
    let body = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(None),
        body = http::read_body(family, response) => body.map_err(AttemptFailure::Provider)?,
    };
    let secrets: Vec<Box<str>> = credential_secrets(credential)
        .into_iter()
        .chain(extra_secrets.iter().cloned())
        .collect();
    let body = transport_mod::redact_text(String::from_utf8_lossy(&body).into_owned(), &secrets);
    let (code, message) = transport_mod::error_fields(&body);
    // JSON escapes decode in `error_fields`, so a secret written as `\u002d`
    // only matches after parsing.
    Ok(Some(AttemptFailure::Response {
        status,
        code: code.map(|code| transport_mod::redact_text(code, &secrets)),
        message: transport_mod::redact_text(message, &secrets),
        retry_after,
    }))
}

pub(crate) async fn compact_attempt(
    context: CompactContext,
    credential: Credential,
    cancel: CancellationToken,
) -> Result<Option<CompactedHistory>, AttemptFailure> {
    match context.family {
        Family::Chat => Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: String::from("Chat compaction has no remote adapter"),
        })),
        Family::Responses => {
            let stream_context = compact_stream_context(&context);
            let wire = build_responses_wire_from_context(&stream_context, &credential)
                .map_err(AttemptFailure::Provider)?;
            let headers = wire.auth_header.into_iter().collect::<Vec<_>>();
            compact::openai_responses(
                &context.client,
                &context.provider.base_url,
                &context.resolved.entry.id,
                &wire.body,
                &headers,
                &wire.user_agent,
                &cancel,
            )
            .await
        }
        Family::Codex => {
            let Credential::OAuth(oauth) = &credential else {
                return Err(AttemptFailure::Provider(ProviderError::NoCredentials {
                    provider: context.provider.id.to_string(),
                }));
            };
            let stream_context = compact_stream_context(&context);
            let wire = build_codex_wire(
                &stream_context,
                &context.request,
                &context.thinking,
                oauth,
                context.session,
            )
            .map_err(AttemptFailure::Provider)?;
            compact::openai_codex(&context.client, &context.provider.base_url, wire, &cancel).await
        }
        Family::Anthropic => {
            let wire = build_anthropic_wire(
                &context.request,
                &context.resolved.entry,
                context.provider.auth,
                &context.thinking,
                &credential,
                true,
            )
            .map_err(AttemptFailure::Provider)?;
            compact::anthropic(
                &context.client,
                &context.provider.base_url,
                &context.resolved.entry.id,
                wire,
                &context.user_agent,
                &cancel,
            )
            .await
        }
    }
}

pub(crate) fn compact_stream_context(context: &CompactContext) -> StreamContext {
    StreamContext {
        family: context.family,
        provider: context.provider.id.clone(),
        base_url: context.provider.base_url.clone(),
        auth: context.provider.auth,
        model: context.resolved.entry.id.clone(),
        entry: context.resolved.entry.clone(),
        request: context.request.clone(),
        thinking: context.thinking.clone(),
        session: context.session,
        client: context.client.clone(),
        user_agent: context.user_agent.clone(),
    }
}

pub(crate) fn ensure_request(http: &Http, request: &ModelRequest) -> Result<(), ProviderError> {
    let ModelRoute::Api { family, model } = &request.model else {
        return Err(ProviderError::InvalidRequest {
            message: String::from("an HTTP provider needs a concrete API model route"),
        });
    };
    if *family != http.entry.family || model.as_ref() != http.model() {
        return Err(ProviderError::InvalidRequest {
            message: String::from("request model does not match the bound provider route"),
        });
    }
    Ok(())
}

pub(crate) fn tool_names(http: &Http, request: &ModelRequest) -> Result<ToolNames, ProviderError> {
    let max = match http.entry.family {
        Family::Anthropic if matches!(&http.credential, Credential::OAuth(_)) => {
            ANTHROPIC_OAUTH_NAME_MAX
        }
        _ => WIRE_NAME_MAX,
    };
    ToolNames::for_request(request, max)
}

pub(crate) fn request_family(request: &ModelRequest) -> Result<Family, ProviderError> {
    match &request.model {
        ModelRoute::Api { family, .. } => Ok(*family),
        _ => Err(ProviderError::InvalidRequest {
            message: String::from("an HTTP provider needs a concrete API model route"),
        }),
    }
}

pub(crate) fn request_model(request: &ModelRequest) -> &str {
    match &request.model {
        ModelRoute::Api { model, .. } => model,
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id,
    }
}

pub(crate) fn unresolved_blob(request: &ModelRequest) -> Option<dal_core::BlobId> {
    request.context.iter().find_map(|item| {
        let parts = match item {
            ContextItem::User { parts } | ContextItem::ToolResult { parts, .. } => parts,
            ContextItem::Assistant { .. } => return None,
        };
        parts.iter().find_map(|part| match part {
            dal_core::Part::Blob { blob_id, .. } => Some(*blob_id),
            dal_core::Part::Text { .. } | dal_core::Part::Image { .. } => None,
        })
    })
}

pub(crate) fn merge_request_parameters(
    request: &ModelRequest,
    mut patch: BeforeRequestPatch,
) -> Result<BeforeRequestPatch, ProviderError> {
    if patch.effort.is_none() {
        patch.effort = request
            .params
            .effort
            .as_deref()
            .map(parse_effort)
            .transpose()?;
    }
    if patch.temperature.is_none() {
        patch.temperature = request
            .params
            .temperature
            .map(parse_temperature)
            .transpose()?;
    }
    Ok(patch)
}

pub(crate) fn parse_effort(value: &str) -> Result<Effort, ProviderError> {
    match value {
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        "xhigh" => Ok(Effort::Xhigh),
        "max" => Ok(Effort::Max),
        _ => Err(ProviderError::InvalidRequest {
            message: format!("unknown effort {value:?}; use low, medium, high, xhigh, or max"),
        }),
    }
}

pub(crate) fn parse_temperature(value: f64) -> Result<f32, ProviderError> {
    if !value.is_finite() {
        return Err(ProviderError::InvalidRequest {
            message: String::from("temperature must be finite"),
        });
    }
    value
        .to_string()
        .parse::<f32>()
        .map_err(|_| ProviderError::InvalidRequest {
            message: String::from("temperature is outside the supported range"),
        })
}

pub(crate) fn make_thinking_plan(
    http: &Http,
    request: &ModelRequest,
    patch: &BeforeRequestPatch,
) -> Result<ThinkingPlan, ProviderError> {
    let route_family = request_family(request)?;
    let max_output = http.resolved.entry.max_output.unwrap_or_else(|| {
        if route_family == Family::Anthropic {
            family::anthropic::MAX_TOKENS_CAP
        } else {
            0
        }
    });
    let max_tokens = if route_family == Family::Anthropic {
        family::anthropic::base_max_tokens(http.resolved.entry.max_output)
    } else {
        max_output
    };
    Ok(thinking::plan(
        request.params.thinking,
        patch,
        &http.resolved.entry.thinking,
        RequestLimits {
            max_tokens,
            max_output,
            temperature_allowed: http.resolved.entry.temperature_allowed,
        },
    ))
}

pub(crate) fn emit_thinking_notices(
    owner: &super::set::ProviderSetInner,
    session: SessionId,
    model: &str,
    notices: &[ThinkingNotice],
    sink: &NoticeSink,
) {
    for notice in notices.iter().copied() {
        let first = super::set::lock(&owner.notices)
            .entry(session)
            .or_default()
            .first(model, notice);
        if first {
            sink(notice.render(model));
        }
    }
}

pub(crate) fn auth_header(
    auth: AuthStyle,
    credential: &Credential,
    family: Family,
    provider: &str,
) -> Result<(&'static str, String), ProviderError> {
    match credential {
        Credential::ApiKey { key } => match auth {
            AuthStyle::Bearer => Ok(("authorization", format!("Bearer {}", key.expose()))),
            AuthStyle::XApiKey => Ok(("x-api-key", key.expose().to_owned())),
        },
        Credential::OAuth(oauth) if matches!(family, Family::Codex | Family::Anthropic) => Ok((
            "authorization",
            format!("Bearer {}", oauth.access_token.expose()),
        )),
        Credential::OAuth(_) => Err(ProviderError::InvalidRequest {
            message: format!("{provider} does not accept an OAuth credential"),
        }),
        Credential::None => Err(ProviderError::NoCredentials {
            provider: provider.to_owned(),
        }),
    }
}

pub(crate) fn credential_secrets(credential: &Credential) -> Vec<Box<str>> {
    match credential {
        Credential::ApiKey { key } => vec![key.expose().into()],
        Credential::OAuth(oauth) => {
            let mut values: Vec<Box<str>> = vec![
                oauth.access_token.expose().into(),
                oauth.refresh_token.expose().into(),
            ];
            values.extend(oauth.id_token.as_deref().map(Box::<str>::from));
            values.extend(oauth.chatgpt_account_id().map(Box::<str>::from));
            values
        }
        Credential::None => Vec::new(),
    }
}

pub(crate) fn model_info(
    entry: &CatalogEntry,
    family: Family,
    default_level: ThinkingLevel,
) -> ModelInfo {
    let (effective, _) = thinking::clamp(default_level, &entry.thinking);
    ModelInfo {
        route: ModelRoute::Api {
            family,
            model: entry.id.clone(),
        },
        name: format!("{} · {}/{}", entry.display, entry.provider, entry.id).into_boxed_str(),
        caps: Caps {
            context_window: entry.context_window,
            thinking: thinking::levels_for(&entry.thinking),
            tool_use: entry.tool_support.allows(family, effective),
            image_input: entry.image_input,
            custom_grammar: entry.custom_grammar
                && matches!(family, Family::Responses | Family::Codex),
        },
    }
}
