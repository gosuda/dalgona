//! Provider request assembly: contexts, wire builders, and parameter shaping.

use std::sync::Arc;

use dal_core::{
    Caps, ContextItem, Family, ModelInfo, ModelRequest, ModelRoute, SessionId, ThinkingLevel,
};
use tokio_util::sync::CancellationToken;

use super::api::Http;
use super::config::{AuthStyle, ProviderEntry};
use super::set::ProviderSetInner;
use crate::{
    auth::credential::{Credential, EnvSnapshot, OAuthCredential},
    catalog::{CatalogEntry, ResolvedModel},
    compact::{self, CompactedHistory},
    error::ProviderError,
    family::{
        self,
        anthropic::{AnthropicAuth, AnthropicRequest},
        azure::{self, AzureRequest, AzureSettings},
        codex::CodexRequest,
        copilot::{self, CopilotKey, CopilotRequest, IdentityRetry, Integration},
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
    owner: Arc<ProviderSetInner>,
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
    let slot = owner.providers.get(&context.provider).ok_or_else(|| {
        AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!("provider {} is not configured", context.provider),
        })
    })?;
    if is_copilot_entry(&slot.entry) {
        return copilot_attempt(&context, &credential, &cancel, &owner, &slot.entry).await;
    }
    if is_azure_entry(&slot.entry) {
        let parts = azure_parts(&context, &credential, &slot.entry, &owner.env)
            .map_err(AttemptFailure::Provider)?;
        return send_parts(&context, &credential, &cancel, parts).await;
    }
    let parts = build_stream_request(&context, &credential).map_err(AttemptFailure::Provider)?;
    send_parts(&context, &credential, &cancel, parts).await
}

async fn send_parts(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
    parts: StreamRequestParts,
) -> Result<Option<EventStream>, AttemptFailure> {
    let (request, user_agent, secrets, replay_prefix) = parts;
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
        return match status_failure(response, context.family, credential, &secrets, cancel).await? {
            Some(failure) => Err(failure),
            None => Ok(None),
        };
    }
    Ok(Some(transport_mod::decode_response(
        response,
        context.family,
        &context.provider,
        &context.model,
        matches!(credential, Credential::OAuth(_)),
        replay_prefix.as_deref(),
        secrets,
        None,
    )))
}

/// Whether `entry` routes through the GitHub Copilot proxy.
///
/// The rule is the adapter's documented shape (three named providers, the
/// explicit integration pin, or a Copilot-domain base); the catalog slice
/// mirrors it verbatim for model listing.
#[must_use]
pub(crate) fn is_copilot_entry(entry: &ProviderEntry) -> bool {
    matches!(
        entry.id.as_ref(),
        "github-copilot" | "github-copilot-responses" | "github-copilot-anthropic"
    ) || entry.settings.integration.is_some()
        || copilot_domain_base(&entry.base_url)
}

fn copilot_domain_base(base: &str) -> bool {
    let Some(host) = url::Url::parse(base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
    else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == "githubcopilot.com" || host.ends_with(".githubcopilot.com")
}

/// Whether `entry` routes through Azure `OpenAI`: a Chat or Responses row
/// with the Azure `api-key` style or Azure settings.
///
/// The style gate matters: [`azure::build`] serves only those two families,
/// so any other family keeps its own path whatever its auth style.
#[must_use]
pub(crate) fn is_azure_entry(entry: &ProviderEntry) -> bool {
    matches!(entry.family, Family::Chat | Family::Responses)
        && (entry.auth == AuthStyle::ApiKey
            || entry.settings.api_version.is_some()
            || !entry.settings.deployments.is_empty())
}

/// Lowers one Azure-routed request through the Azure adapter: deployment
/// routing, the `api-version` query, and the `api-key` header.
///
/// Entry settings win over the captured environment snapshot, per the
/// adapter contract; the decoder stays the surface family's.
fn azure_parts(
    context: &StreamContext,
    credential: &Credential,
    entry: &ProviderEntry,
    env: &EnvSnapshot,
) -> Result<StreamRequestParts, ProviderError> {
    let settings = AzureSettings {
        api_version: entry.settings.api_version.clone(),
        deployments: entry
            .settings
            .deployments
            .iter()
            .map(|(model, deployment)| (model.clone(), deployment.clone()))
            .collect(),
    }
    .over(AzureSettings::from_env(env));
    let wire = azure::build(&AzureRequest {
        request: &context.request,
        thinking: &context.thinking,
        reasoning_summary: context.entry.supports_reasoning_summaries,
        provider: &context.provider,
        base_url: &context.base_url,
        settings: &settings,
        credential,
    })?;
    let url = wire.endpoint()?;
    let secrets = vec![wire.secret.clone()];
    Ok((
        wire.into_request(&context.client, url),
        context.user_agent.clone(),
        secrets,
        None,
    ))
}

/// The Copilot token an API-key credential carries; an OAuth sign-in is
/// refused.
fn parse_copilot_token(credential: &Credential, provider: &str) -> Result<String, AttemptFailure> {
    match credential {
        Credential::ApiKey { key } => Ok(key.expose().to_owned()),
        Credential::OAuth(_) => Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!("{provider} needs an API key, not an OAuth sign-in"),
        })),
        Credential::None => Err(AttemptFailure::Provider(ProviderError::NoCredentials {
            provider: provider.to_string(),
        })),
    }
}

struct CopilotSession<'a> {
    context: &'a StreamContext,
    credential: &'a Credential,
    cancel: &'a CancellationToken,
    owner: &'a Arc<ProviderSetInner>,
    key: &'a CopilotKey,
}

impl CopilotSession<'_> {
    async fn resend(
        &self,
        next: &Integration,
        retry: &mut IdentityRetry,
    ) -> Result<Option<EventStream>, AttemptFailure> {
        if self.cancel.is_cancelled() {
            retry.failed(&self.owner.copilot_identities);
            return Ok(None);
        }
        let resent = copilot::build(&copilot_input(self.context, self.key, next))
            .map_err(AttemptFailure::Provider)?;
        let dispatch = copilot_dispatch(self.context, resent)?;
        let CopilotDispatch {
            family,
            builder,
            agent,
            model,
            prefix,
            secret,
        } = dispatch;
        let Some(response) = send_stream_wire(family, builder, agent, self.cancel).await? else {
            retry.failed(&self.owner.copilot_identities);
            return Ok(None);
        };
        if !response.status().is_success() {
            let Some(failure) = status_failure(
                response,
                self.context.family,
                self.credential,
                &[],
                self.cancel,
            )
            .await?
            else {
                retry.failed(&self.owner.copilot_identities);
                return Ok(None);
            };
            if let AttemptFailure::Response {
                status,
                code,
                message,
                ..
            } = &failure
                && let Some(mapped) =
                    copilot::failure_error(self.context.family, *status, code.as_deref(), message)
            {
                retry.failed(&self.owner.copilot_identities);
                return Err(AttemptFailure::Provider(mapped));
            }
            retry.failed(&self.owner.copilot_identities);
            return Err(failure);
        }
        retry.succeeded(&self.owner.copilot_identities);
        Ok(Some(copilot_decode(
            self.context,
            response,
            family,
            &model,
            prefix.as_deref(),
            secret,
        )))
    }
}

/// Runs one Copilot turn: the identity negotiation starts from the pin,
/// the learned identity, or the account default, and a denial resends once
/// with the fallback identity inside the same attempt.
///
/// Quota failures are terminal through the adapter classifier; the decoder
/// stays the surface family's with replay bound to the catalog model.
async fn copilot_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
    entry: &ProviderEntry,
) -> Result<Option<EventStream>, AttemptFailure> {
    let token = parse_copilot_token(credential, &context.provider)?;
    let key = CopilotKey::parse(&token);
    let pinned = entry
        .settings
        .integration
        .as_deref()
        .and_then(Integration::pinned);
    let mut retry =
        IdentityRetry::begin(&owner.copilot_identities, &key, &context.base_url, pinned);
    let wire = copilot::build(&copilot_input(context, &key, retry.integration()))
        .map_err(AttemptFailure::Provider)?;
    let dispatch = copilot_dispatch(context, wire)?;
    let CopilotDispatch {
        family,
        builder,
        agent,
        model,
        prefix,
        secret,
    } = dispatch;
    let Some(response) = send_stream_wire(family, builder, agent, cancel).await? else {
        retry.failed(&owner.copilot_identities);
        return Ok(None);
    };
    if response.status().is_success() {
        retry.succeeded(&owner.copilot_identities);
        return Ok(Some(copilot_decode(
            context,
            response,
            family,
            &model,
            prefix.as_deref(),
            secret,
        )));
    }
    let Some(failure) = status_failure(response, context.family, credential, &[], cancel).await?
    else {
        retry.failed(&owner.copilot_identities);
        return Ok(None);
    };
    let AttemptFailure::Response {
        status,
        code,
        message,
        retry_after,
    } = failure
    else {
        retry.failed(&owner.copilot_identities);
        return Err(failure);
    };
    // An identity denial resends once with the fallback identity before any
    // note or quota mapping runs: a quota code is never a denial, so the
    // two never compete.
    let surface = context.family;
    let next = retry.denied(status, code.as_deref()).cloned();
    let Some(next) = next else {
        if let Some(mapped) = copilot::failure_error(surface, status, code.as_deref(), &message) {
            retry.failed(&owner.copilot_identities);
            return Err(AttemptFailure::Provider(mapped));
        }
        retry.failed(&owner.copilot_identities);
        return Err(AttemptFailure::Response {
            status,
            code,
            message,
            retry_after,
        });
    };
    let session = CopilotSession {
        context,
        credential,
        cancel,
        owner,
        key: &key,
    };
    session.resend(&next, &mut retry).await
}

/// One lowered Copilot request ready to send, with the decode bindings its
/// surface family needs.
struct CopilotDispatch {
    family: Family,
    builder: reqwest::RequestBuilder,
    agent: &'static str,
    model: Box<str>,
    prefix: Option<Box<str>>,
    secret: Box<str>,
}

fn copilot_dispatch(
    context: &StreamContext,
    wire: copilot::CopilotWire,
) -> Result<CopilotDispatch, AttemptFailure> {
    let url = wire.endpoint().map_err(AttemptFailure::Provider)?;
    let dispatch = CopilotDispatch {
        family: wire.surface.family(),
        agent: wire.user_agent,
        model: wire.model.clone(),
        prefix: wire.prefix.clone(),
        secret: wire.secret.clone(),
        builder: wire.into_request(&context.client, url),
    };
    Ok(dispatch)
}

/// Decodes one accepted Copilot response through its surface decoder with
/// replay bound to the catalog model id.
///
/// The decode bindings travel as parts, not as the whole dispatch: the send
/// already moved the builder out, and the builder is never cloned or rebuilt.
fn copilot_decode(
    context: &StreamContext,
    response: reqwest::Response,
    family: Family,
    model: &str,
    prefix: Option<&str>,
    secret: Box<str>,
) -> EventStream {
    let premium = copilot::premium_requests(
        copilot::initiator(&context.request),
        context.entry.premium_multiplier,
    );
    let decoded = transport_mod::decode_response(
        response,
        family,
        &context.provider,
        model,
        false,
        prefix,
        vec![secret],
        None,
    );
    transport_mod::stamp_premium(decoded, premium)
}

fn copilot_input<'a>(
    context: &'a StreamContext,
    key: &'a CopilotKey,
    integration: &'a Integration,
) -> CopilotRequest<'a> {
    CopilotRequest {
        request: &context.request,
        thinking: &context.thinking,
        max_output: context.entry.max_output,
        display_supported: context.entry.display_supported,
        reasoning_summary: context.entry.supports_reasoning_summaries,
        base_url: &context.base_url,
        key,
        integration,
    }
}

async fn send_stream_wire(
    family: Family,
    builder: reqwest::RequestBuilder,
    user_agent: &str,
    cancel: &CancellationToken,
) -> Result<Option<reqwest::Response>, AttemptFailure> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(None),
        result = http::send(
            family,
            builder,
            user_agent,
            Exchange::Stream,
            tokio::time::sleep,
        ) => result.map(Some).map_err(AttemptFailure::Provider),
    }
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
            let mut request = context.client.post(url);
            // A credential-less row (a keyless local gateway such as
            // llama.cpp, configured `auth = "none"`) sends no header.
            if let Some((header, value)) =
                optional_auth_header(context.auth, credential, route_family, &context.provider)?
            {
                request = request.header(header, value);
            }
            Ok((request.body(body), user_agent, secrets, None))
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
        Family::Bedrock => Err(ProviderError::InvalidRequest {
            message: String::from("Bedrock requests use the dedicated Converse adapter"),
        }),
        Family::Cursor => Err(ProviderError::InvalidRequest {
            message: String::from("Cursor requests use the dedicated run adapter"),
        }),
        Family::Devin => Err(ProviderError::InvalidRequest {
            message: String::from("Devin requests use the dedicated Connect adapter"),
        }),
        Family::Droid => Err(ProviderError::InvalidRequest {
            message: String::from("Droid requests use the dedicated proxy adapter"),
        }),
        Family::GitLab => Err(ProviderError::InvalidRequest {
            message: String::from("GitLab requests use the dedicated Duo adapter"),
        }),
        Family::GoogleGenerativeAi | Family::GoogleVertex | Family::GoogleAntigravity => {
            Err(ProviderError::InvalidRequest {
                message: String::from("Google requests use the dedicated GenerateContent adapter"),
            })
        }
        Family::GoogleGeminiCli => Err(ProviderError::InvalidRequest {
            message: String::from("Gemini CLI requests use the dedicated subprocess adapter"),
        }),
        Family::Mistral => {
            let body = family::mistral::request_body(&context.request, &context.thinking).map_err(
                |error| ProviderError::InvalidRequest {
                    message: error.to_string(),
                },
            )?;
            let url = http::endpoint(
                route_family,
                &context.base_url,
                family::mistral::ENDPOINT_PATH,
            )?;
            let mut request = context.client.post(url);
            if let Some((header, value)) =
                optional_auth_header(context.auth, credential, route_family, &context.provider)?
            {
                request = request.header(header, value);
            }
            Ok((request.body(body), user_agent, secrets, None))
        }
        Family::Ollama => {
            let deployment = family::ollama::Deployment::of(&context.provider, &context.base_url);
            let policy = family::ollama::WirePolicy::for_model(
                deployment,
                &context.model,
                &context.entry.thinking,
            );
            let body = family::ollama::request_body(&context.request, &context.thinking, policy)
                .map_err(|error| ProviderError::InvalidRequest {
                    message: error.to_string(),
                })?;
            let base = family::ollama::trim_api_suffix(&context.base_url);
            let url = http::endpoint(route_family, base, family::ollama::ENDPOINT_PATH)?;
            let mut request = context.client.post(url);
            if let Some((header, value)) =
                optional_auth_header(context.auth, credential, route_family, &context.provider)?
            {
                request = request.header(header, value);
            }
            Ok((request.body(body), user_agent, secrets, None))
        }
        Family::PiMessages => {
            let body = family::pi_messages::request_body(
                &context.request,
                &context.provider,
                &context.thinking,
            )
            .map_err(|error| ProviderError::InvalidRequest {
                message: error.to_string(),
            })?;
            let url = http::endpoint(
                route_family,
                &context.base_url,
                family::pi_messages::ENDPOINT_PATH,
            )?;
            let (header, value) =
                auth_header(context.auth, credential, route_family, &context.provider)?;
            Ok((
                context.client.post(url).header(header, value).body(body),
                user_agent,
                secrets,
                None,
            ))
        }
        Family::Anthropic => build_anthropic_stream_request(context, credential, secrets),
    }
}

fn build_anthropic_stream_request(
    context: &StreamContext,
    credential: &Credential,
    secrets: Vec<Box<str>>,
) -> Result<StreamRequestParts, ProviderError> {
    let mut wire = build_anthropic_wire(
        &context.request,
        &context.entry,
        context.auth,
        &context.thinking,
        credential,
        false,
    )?;
    let url = http::endpoint(context.family, &context.base_url, wire.path)?;
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

/// The auth header of one request, or none for a credential-less entry that
/// authenticates nothing. A held credential on such an entry stays refused;
/// only the admitted absence sends no header.
fn optional_auth_header(
    auth: AuthStyle,
    credential: &Credential,
    family: Family,
    provider: &str,
) -> Result<Option<(&'static str, String)>, ProviderError> {
    if auth == AuthStyle::None && matches!(credential, Credential::None) {
        return Ok(None);
    }
    auth_header(auth, credential, family, provider).map(Some)
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
    match auth_style {
        AuthStyle::None => {
            return Err(ProviderError::InvalidRequest {
                message: String::from("credential-less entries carry no auth header"),
            });
        }
        AuthStyle::ApiKey => {
            return Err(ProviderError::InvalidRequest {
                message: String::from("Anthropic requests do not support the api-key header style"),
            });
        }
        AuthStyle::Bearer | AuthStyle::XApiKey => {}
    }
    let auth = match credential {
        Credential::ApiKey { key } => match auth_style {
            AuthStyle::XApiKey => AnthropicAuth::ApiKey(key.expose()),
            AuthStyle::Bearer => AnthropicAuth::Bearer(key.expose()),
            AuthStyle::ApiKey => {
                return Err(ProviderError::InvalidRequest {
                    message: String::from(
                        "Anthropic requests do not support the api-key header style",
                    ),
                });
            }
            AuthStyle::None => {
                return Err(ProviderError::InvalidRequest {
                    message: String::from("credential-less entries carry no auth header"),
                });
            }
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
        // Every other family names its own wire for turns but has no remote
        // compaction endpoint; each is listed so a new family forces a
        // decision here instead of falling into this refusal.
        Family::Bedrock
        | Family::Cursor
        | Family::Devin
        | Family::Droid
        | Family::GitLab
        | Family::GoogleGenerativeAi
        | Family::GoogleVertex
        | Family::GoogleGeminiCli
        | Family::GoogleAntigravity
        | Family::Mistral
        | Family::Ollama
        | Family::PiMessages => Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!(
                "{} compaction has no remote adapter",
                crate::error::family_label(context.family)
            ),
        })),
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
    if auth == AuthStyle::None {
        return Err(ProviderError::InvalidRequest {
            message: String::from("credential-less entries carry no auth header"),
        });
    }
    match credential {
        Credential::ApiKey { key } => match auth {
            AuthStyle::Bearer => Ok(("authorization", format!("Bearer {}", key.expose()))),
            AuthStyle::XApiKey => Ok(("x-api-key", key.expose().to_owned())),
            AuthStyle::ApiKey => Ok(("api-key", key.expose().to_owned())),
            AuthStyle::None => Err(ProviderError::InvalidRequest {
                message: String::from("credential-less entries carry no auth header"),
            }),
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
