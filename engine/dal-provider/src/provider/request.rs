//! Provider request assembly: contexts, wire builders, and parameter shaping.

use std::sync::Arc;

use dal_core::{
    Caps, ContextItem, Family, ModelInfo, ModelRequest, ModelRoute, SessionId, ThinkingLevel,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::api::Http;
use super::config::{AuthStyle, ProviderEntry};
use super::set::ProviderSetInner;
use crate::{
    auth::credential::{AuthStore, Credential, EnvSnapshot, OAuthCredential},
    catalog::{CatalogEntry, ResolvedModel},
    compact::{self, CompactedHistory},
    error::ProviderError,
    family::{
        self,
        anthropic::{AnthropicAuth, AnthropicRequest},
        azure::{self, AzureRequest, AzureSettings},
        bedrock::{
            self,
            auth::{AwsResolveOptions, BedrockAuth, SHARED_RESOLVE_TIMEOUT},
        },
        codex::CodexRequest,
        copilot::{self, CopilotKey, CopilotRequest, IdentityRetry, Integration},
        droid::{self, DroidRequest, RoutePolicy},
        gitlab::{
            self,
            workflow::{self as gitlab_workflow, WorkflowScope, WorkflowTurn},
        },
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

/// The captured variable that downgrades an openrouter stream to its Chat
/// Completions wire: only the exact value `"0"` counts, mirroring the
/// reference's `PI_OPENROUTER_RESPONSES` semantic.
const OPENROUTER_RESPONSES_ENV: &str = "DAL_OPENROUTER_RESPONSES";

/// Whether the openrouter row streams its Chat Completions wire this turn:
/// the row's resolved route is its Responses default and the captured
/// environment names `"0"` exactly. Any other value, including none, keeps
/// the Responses default, and a route the configuration already pinned to
/// another family keeps that pin.
#[must_use]
fn is_openrouter_chat_override(context: &StreamContext, env: &EnvSnapshot) -> bool {
    context.provider.as_ref() == "openrouter"
        && context.family == Family::Responses
        && env.get(OPENROUTER_RESPONSES_ENV) == Some("0")
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
    // The openrouter row's Responses default downgrades to its Chat
    // Completions wire for this turn: the route family follows the context
    // family, so the body, endpoint, and decoder all agree on the Chat wire.
    let mut context = context;
    if is_openrouter_chat_override(&context, &owner.env) {
        context.family = Family::Chat;
        context.request.model = ModelRoute::Api {
            family: Family::Chat,
            model: context.model.clone(),
        };
    }
    if is_copilot_entry(&slot.entry) {
        return copilot_attempt(&context, &credential, &cancel, &owner, &slot.entry).await;
    }
    if is_azure_entry(&slot.entry) {
        let parts = azure_parts(&context, &credential, &slot.entry, &owner.env)
            .map_err(AttemptFailure::Provider)?;
        return send_parts(&context, &credential, &cancel, parts).await;
    }
    match context.family {
        Family::Bedrock => Box::pin(bedrock_attempt(&context, &credential, &cancel, &owner)).await,
        Family::GitLab => gitlab_attempt(&context, &credential, &cancel, &owner).await,
        Family::Droid => droid_attempt(&context, &credential, &cancel).await,
        _ => {
            let parts =
                build_stream_request(&context, &credential).map_err(AttemptFailure::Provider)?;
            send_parts(&context, &credential, &cancel, parts).await
        }
    }
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

/// The media range of one Bedrock Converse response: binary event-stream
/// frames, never SSE.
const BEDROCK_EVENTSTREAM_ACCEPT: &str = "application/vnd.amazon.eventstream";

/// Whether one attempt failure is an unauthorized response: the only status
/// that invalidates a cached grant or credential before the established
/// refresh contract runs.
fn is_unauthorized(failure: &AttemptFailure) -> bool {
    matches!(
        failure,
        AttemptFailure::Response {
            status: 401 | 403,
            ..
        }
    )
}

/// One Bedrock attempt: bearer-or-SigV4 auth resolved under a single caller
/// envelope, one Converse request, and at most one invalidate-and-retry on a
/// pre-delivery 401/403.
///
/// The caller owns the envelope: `Instant::now() + SHARED_RESOLVE_TIMEOUT`
/// threads through region resolution and every credential tier, including
/// region and source probes; no nested call mints its own deadline. The
/// bound credential is never read here: `resolve_bedrock_auth` owns bearer
/// precedence (stored key, then `AWS_BEARER_TOKEN_BEDROCK`) and the `SigV4`
/// chain behind one call, so request-level and resolver-level bearer sources
/// cannot disagree.
///
/// A 401/403 before any byte is decoded invalidates the cached credential
/// and resends once — the `RefreshOnce` contract for a non-OAuth credential
/// the shared retry table cannot refresh. Any other failure, the second
/// outcome, and every post-delivery error return as-is: no indiscriminate
/// retry.
async fn bedrock_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
) -> Result<Option<EventStream>, AttemptFailure> {
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let slot = owner.providers.get(&context.provider).ok_or_else(|| {
        AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!("provider {} is not configured", context.provider),
        })
    })?;
    let entry = slot.entry.clone();
    let deadline = tokio::time::Instant::now() + SHARED_RESOLVE_TIMEOUT;
    let region = owner
        .aws_resolver
        .resolve_region(entry.settings.region.as_deref(), None, cancel, deadline)
        .await;
    let options = AwsResolveOptions {
        profile: None,
        region,
    };
    let first = Box::pin(bedrock_try(
        context, credential, &entry, &options, cancel, owner, deadline,
    ))
    .await;
    if !is_bedrock_unauthorized(&first) {
        return first;
    }
    owner
        .aws_resolver
        .invalidate(options.profile.as_deref(), &options.region);
    Box::pin(bedrock_try(
        context, credential, &entry, &options, cancel, owner, deadline,
    ))
    .await
}

/// Whether one Bedrock outcome earns the single invalidate-and-retry: an
/// HTTP 401/403, or a 401/403 the signer reported before sending (an
/// unusable cached credential).
fn is_bedrock_unauthorized(outcome: &Result<Option<EventStream>, AttemptFailure>) -> bool {
    match outcome {
        Err(failure) if is_unauthorized(failure) => true,
        Err(AttemptFailure::Provider(ProviderError::Status {
            status: 401 | 403, ..
        })) => true,
        _ => false,
    }
}

/// Resolves auth, builds the Converse request, sends it, and decodes the
/// byte stream. Every fallible step returns its failure for the caller to
/// classify; only the caller retries.
async fn bedrock_try(
    context: &StreamContext,
    credential: &Credential,
    entry: &ProviderEntry,
    options: &AwsResolveOptions,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
    deadline: tokio::time::Instant,
) -> Result<Option<EventStream>, AttemptFailure> {
    let store =
        AuthStore::load(owner.data_dir.join("auth.json")).map_err(AttemptFailure::Provider)?;
    let auth = Box::pin(
        owner
            .aws_resolver
            .resolve_bedrock_auth(entry, &store, options, cancel, deadline),
    )
    .await
    .map_err(AttemptFailure::Provider)?;
    let (builder, auth_secrets) = bedrock_request(context, &auth, &options.region)?;
    let mut secrets = credential_secrets(credential);
    secrets.extend(auth_secrets);
    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(None),
        response = http::send(
            Family::Bedrock,
            builder,
            &context.user_agent,
            Exchange::Bytes {
                accept: BEDROCK_EVENTSTREAM_ACCEPT,
                total: None,
            },
            tokio::time::sleep,
        ) => response.map_err(AttemptFailure::Provider)?,
    };
    if !response.status().is_success() {
        return match status_failure(response, Family::Bedrock, credential, &secrets, cancel).await?
        {
            Some(failure) => Err(failure),
            None => Ok(None),
        };
    }
    Ok(Some(transport_mod::decode_response(
        response,
        Family::Bedrock,
        &context.provider,
        &context.model,
        false,
        None,
        secrets,
        None,
    )))
}

/// Builds the Converse `RequestBuilder` for one resolved auth: a bearer token
/// rides `Authorization: Bearer` with no signing, and `SigV4` signs through
/// the family's narrow signer seam. The second member is the redaction list
/// of the credential material the request carries; every response and stream
/// error of the attempt passes through it.
fn bedrock_request(
    context: &StreamContext,
    auth: &BedrockAuth,
    region: &str,
) -> Result<(reqwest::RequestBuilder, Vec<Box<str>>), ProviderError> {
    match auth {
        BedrockAuth::Bearer(token) => {
            let path = bedrock::request_path(&context.model);
            let url = http::endpoint(Family::Bedrock, &context.base_url, &path)?;
            let body = bedrock::request_body(&context.request, &context.thinking)
                .map_err(bedrock_body_error)?;
            let builder = context
                .client
                .post(url)
                .header("content-type", "application/json")
                .header("accept", BEDROCK_EVENTSTREAM_ACCEPT)
                .header("authorization", format!("Bearer {}", token.expose()));
            Ok((builder.body(body), vec![token.expose().into()]))
        }
        BedrockAuth::SigV4(credential) => {
            let now_unix = jiff::Timestamp::now().as_second();
            let wire = bedrock::build(
                &context.request,
                &context.thinking,
                &context.base_url,
                Some(region),
                credential,
                now_unix,
            )?;
            let secrets = sigv4_secrets(credential, &wire);
            Ok((wire.into_request(&context.client), secrets))
        }
    }
}

/// The credential material one signed Bedrock request holds, for the
/// redaction list.
///
/// The access key id, the session token and the `authorization` value (which
/// carries the id, the credential scope and the signature) travel on the
/// wire, and a rejection can echo any of them: an AWS signature mismatch
/// quotes the signed headers. The secret access key never leaves the process;
/// it is listed anyway, as [`credential_secrets`] lists every secret field of
/// a credential. The whole `authorization` value comes first because
/// [`transport_mod::redact_text`] replaces in order: scrubbing the access key
/// id first would leave the scope and the signature of an echoed header
/// readable. The bare signature follows for an echo that quotes it apart.
fn sigv4_secrets(
    credential: &bedrock::AwsCredential,
    wire: &bedrock::BedrockWire,
) -> Vec<Box<str>> {
    let mut secrets: Vec<Box<str>> = Vec::with_capacity(5);
    if let Some((_, authorization)) =
        wire.headers.iter().find(|(name, _)| name == "authorization")
    {
        secrets.push(authorization.as_str().into());
        secrets.extend(
            authorization
                .rsplit_once("Signature=")
                .map(|(_, signature)| Box::<str>::from(signature)),
        );
    }
    secrets.extend(credential.session_token.clone());
    secrets.push(credential.access_key_id.clone());
    secrets.push(credential.secret_access_key.clone());
    secrets
}

/// Maps a Bedrock body error the way the family's own transport layer does:
/// blobs stay typed, everything else is an invalid request.
fn bedrock_body_error(error: bedrock::BedrockBodyError) -> ProviderError {
    match error {
        bedrock::BedrockBodyError::UnresolvedBlob { blob_id } => {
            ProviderError::UnresolvedBlob { blob_id }
        }
        other => ProviderError::InvalidRequest {
            message: other.to_string(),
        },
    }
}

/// One GitLab attempt: the proxy surface through the family's builders and
/// byte transport, or the workflow surface when the entry scopes a workflow
/// session.
///
/// Both surfaces start from the GitLab credential, which never rides an
/// inference request itself: the proxy exchanges it for the owner-cached
/// `direct_access` grant, the workflow for a per-run token. The grant cache
/// is the one `ProviderSet` instance the refresh engine invalidates on
/// sign-in rotation. A 401/403 on the proxy drops that token's grants and
/// returns the failure for the established refresh contract (OAuth refreshes
/// once, anything else is final); nothing retries past delivery.
async fn gitlab_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
) -> Result<Option<EventStream>, AttemptFailure> {
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let token = gitlab::require_token(credential).map_err(AttemptFailure::Provider)?;
    let slot = owner.providers.get(&context.provider).ok_or_else(|| {
        AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!("provider {} is not configured", context.provider),
        })
    })?;
    if slot.entry.settings.namespace.is_some() {
        return gitlab_workflow_attempt(context, token, cancel, owner, &slot.entry).await;
    }
    let grants = Arc::clone(&owner.gitlab_grants);
    let grant = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(None),
        grant = grants.ensure(
            &context.client,
            &context.base_url,
            token,
            &context.user_agent,
        ) => grant.map_err(AttemptFailure::Provider)?,
    };
    let wire = gitlab::build_proxy(&gitlab::DuoProxyRequest {
        request: &context.request,
        thinking: &context.thinking,
        grant: &grant,
        max_output: context.entry.max_output,
        reasoning_summary: context.entry.supports_reasoning_summaries,
        display_supported: context.entry.display_supported,
        session: context.session,
    })
    .map_err(AttemptFailure::Provider)?;
    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(None),
        outcome = gitlab::open(&context.client, wire, &context.user_agent) => outcome,
    };
    match outcome {
        Err(failure) if is_unauthorized(&failure) => {
            grants.remove(token);
            Err(failure)
        }
        outcome => outcome.map(Some),
    }
}

/// One GitLab workflow turn: REST setup plus the stateful socket, driven
/// through the set-owned resumable sessions. The scope comes from
/// configuration (`providers.<id>.namespace`); workspace discovery would
/// need process spawn, which the provider layer forbids.
async fn gitlab_workflow_attempt(
    context: &StreamContext,
    token: &str,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
    entry: &ProviderEntry,
) -> Result<Option<EventStream>, AttemptFailure> {
    let namespace = entry.settings.namespace.as_deref().ok_or_else(|| {
        AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!(
                "provider {} needs a namespace for a workflow session",
                entry.id
            ),
        })
    })?;
    if !valid_workflow_namespace(namespace) {
        return Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!(
                "provider {} names a namespace {namespace:?}; a workflow session needs digits or gid://gitlab/Group/<digits>",
                entry.id
            ),
        }));
    }
    let scope = WorkflowScope {
        root_namespace_id: namespace.to_owned(),
        namespace_path: None,
        project_id: None,
        project_path: None,
        workflow_definition: None,
    };
    let turn = WorkflowTurn {
        request: &context.request,
        model: context.model.as_ref(),
        base_url: context.base_url.as_ref(),
        token,
        provider: context.provider.as_ref(),
        session: context.session,
        scope: &scope,
        tool_choice_none: context.request.tools.is_empty(),
        timeouts: gitlab_workflow::Timeouts::default(),
        cancel,
        user_agent: context.user_agent.as_ref(),
    };
    gitlab_workflow::run_turn(&context.client, &owner.gitlab_workflows, &turn).await
}

/// Whether a configured GitLab namespace scopes a workflow session: the
/// family's contract admits a root namespace id only, digits or
/// `gid://gitlab/Group/<digits>`. A namespace path would break workflow
/// creation at the first turn, so it is refused here instead of passed
/// through to the GraphQL and REST bodies.
fn valid_workflow_namespace(namespace: &str) -> bool {
    if !namespace.is_empty() && namespace.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    namespace
        .strip_prefix("gid://gitlab/Group/")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
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

/// The default `x-api-provider` of one Droid model id: the first entry of
/// its `upstream-rotation` in the pinned Factory registry. Ids outside the
/// registry fall back to their route's dominant upstream; that fallback is
/// a join default, not a pin fact.
pub(crate) fn droid_default_upstream(model: &str) -> &'static str {
    if let Some(upstream) = DROID_UPSTREAMS
        .iter()
        .find(|(id, _)| *id == model)
        .map(|(_, upstream)| *upstream)
    {
        return upstream;
    }
    match droid::wire_for(model) {
        droid::Wire::Anthropic => "anthropic",
        droid::Wire::Responses => "openai",
        droid::Wire::Gemini => "google",
        droid::Wire::Completions => "fireworks",
    }
}

/// The default `x-api-provider` per Droid model id, transcribed from the
/// first entry of each `upstream-rotation` in the pinned Factory registry.
/// Region narrowing (`region-upstreams-eu/us`) is not applied: the stream
/// path carries no region signal.
const DROID_UPSTREAMS: &[(&str, &str)] = &[
    ("aster-07-15", "anthropic"),
    ("atlas-07-21", "anthropic"),
    ("claude-fable-5", "anthropic"),
    ("claude-fable-5.1", "anthropic"),
    ("claude-haiku-4-5-20251001", "anthropic"),
    ("claude-haiku-5-5", "anthropic"),
    ("claude-opus-4-5-20251101", "anthropic"),
    ("claude-opus-4-6", "anthropic"),
    ("claude-opus-4-7", "anthropic"),
    ("claude-opus-4-8", "anthropic"),
    ("claude-opus-4-8-fast", "anthropic"),
    ("claude-opus-5", "anthropic"),
    ("claude-opus-5-5", "anthropic"),
    ("claude-opus-5-5-fast", "anthropic"),
    ("claude-opus-5-fast", "anthropic"),
    ("claude-sonnet-4-5-20250929", "anthropic"),
    ("claude-sonnet-4-6", "anthropic"),
    ("claude-sonnet-5", "anthropic"),
    ("claude-sonnet-5-5", "anthropic"),
    ("deepseek-v4-flash-0731", "fireworks"),
    ("deepseek-v4-pro", "fireworks"),
    ("deepseek-v4.1-flash", "fireworks"),
    ("garnet-07-15", "google"),
    ("gemini-3-flash-preview", "google"),
    ("gemini-3.1-pro-preview", "google"),
    ("gemini-3.5-flash", "google"),
    ("gemini-3.6-flash", "google"),
    ("gemini-3.7-flash", "google"),
    ("gemini-3.8-flash", "google"),
    ("glm-5.2", "baseten"),
    ("glm-5.2-fast", "baseten"),
    ("glm-5.3", "fireworks"),
    ("glm-5.3-flash", "fireworks"),
    ("gpt-5.2", "openai"),
    ("gpt-5.3-codex", "openai"),
    ("gpt-5.3-codex-fast", "openai"),
    ("gpt-5.4", "openai"),
    ("gpt-5.4-fast", "openai"),
    ("gpt-5.4-mini", "openai"),
    ("gpt-5.4-mini-fast", "openai"),
    ("gpt-5.5", "openai"),
    ("gpt-5.5-fast", "openai"),
    ("gpt-5.5-pro", "openai"),
    ("gpt-5.6-luna", "openai"),
    ("gpt-5.6-sol", "openai"),
    ("gpt-5.6-sol-fast", "openai"),
    ("gpt-5.6-terra", "openai"),
    ("gpt-6-astra", "openai"),
    ("gpt-6-luna", "openai"),
    ("gpt-6-sol", "openai"),
    ("gpt-6.1-sol", "openai"),
    ("grok-4.5", "xai"),
    ("grok-4.6", "xai"),
    ("grok-4.7", "xai"),
    ("inkling", "fireworks"),
    ("kimi-k3", "fireworks"),
    ("minimax-m2.7", "fireworks"),
    ("minimax-m3", "fireworks"),
    ("mistral-large-4", "mistral"),
    ("mistral-medium-3.5", "mistral"),
    ("nemotron-3-ultra", "baseten"),
    ("qwen3.8-max", "fireworks"),
];

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

/// Runs one Droid turn through the proxy lifecycle: the wire lowers the
/// request to its route shape and the family owns send, rejection mapping,
/// redaction, and replay rebinding.
///
/// The upstream router is the registry default for the model id; the
/// per-turn assistant-message id is fresh per attempt.
async fn droid_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
) -> Result<Option<EventStream>, AttemptFailure> {
    let bearer = droid::require_bearer(credential).map_err(AttemptFailure::Provider)?;
    let model = context.request.model.id().to_owned();
    let request_id = Uuid::new_v4().to_string();
    let policy = RoutePolicy {
        reasoning_summary: context.entry.supports_reasoning_summaries,
        display_supported: context.entry.display_supported,
        max_output: context.entry.max_output,
        ..Default::default()
    };
    let wire = droid::build_wire(&DroidRequest {
        request: &context.request,
        thinking: &context.thinking,
        base_url: &context.base_url,
        upstream: droid_default_upstream(&model),
        session: context.session,
        request_id: &request_id,
        bearer,
        policy: &policy,
    })
    .map_err(AttemptFailure::Provider)?;
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(None),
        result = droid::open(&context.client, wire) => result.map(Some),
    }
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
