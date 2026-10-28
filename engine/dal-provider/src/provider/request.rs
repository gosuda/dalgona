//! Provider request assembly: contexts, wire builders, and parameter shaping.

use std::{borrow::Cow, sync::Arc};

use dal_core::{
    Caps, ContextItem, Family, ModelInfo, ModelRequest, ModelRoute, SessionId, ThinkingLevel,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::api::Http;
use super::config::{AuthStyle, ProviderEntry};
use super::set::{ProviderSetInner, lock};
use crate::{
    auth::{
        credential::{AuthStore, Credential, EnvSnapshot, OAuthCredential},
        kimi::ClientIdentity,
    },
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
        devin::{
            PATH_ASSIGN, PATH_AUTH, PATH_CHAT, PATH_DISCOVERY,
            connect::{ConnectError, decode_unary},
            decode::{DiscoveredModel, decode_discovery},
            history::router_prompt,
            proto::{AssignModelResponse, GetUserJwtResponse},
            request::{
                CHAT_HEADERS, CONNECT_USER_AGENT, DevinChatInput, UNARY_HEADERS, assign_body,
                chat_frame, discovery_body, error_detail, normalize_session_token, user_jwt_body,
                user_jwt_fallback_body,
            },
        },
        droid::{self, DroidRequest, RoutePolicy},
        gitlab::{
            self,
            workflow::{self as gitlab_workflow, WorkflowScope, WorkflowTurn},
        },
        google::{
            antigravity::{self, AntigravityRequest},
            cloud_code::{self, NativeRequest},
            gemini::{self, GeminiRequest},
            gemini_cli::{self, CliRequest},
            shared as google_shared,
            vertex::{self, VertexAuth, VertexRequest},
        },
        responses::ResponsesRequest,
    },
    http::{self, Exchange, NON_STREAM_TOTAL_TIMEOUT},
    lifecycle::AttemptFailure,
    retry::{RequestState, RetryDecision},
    stream::{EventStream, NoticeSink},
    thinking::{
        self, BeforeRequestPatch, Effort, RequestLimits, ThinkingNotice, ThinkingPlan, WireThinking,
    },
    tool_names::{ANTHROPIC_OAUTH_NAME_MAX, ToolNames, WIRE_NAME_MAX},
};

use super::wire;
pub(crate) use super::wire::is_azure_entry;
use super::{Origin, transport as transport_mod};

#[derive(Clone)]
pub(crate) struct StreamContext {
    pub(crate) family: Family,
    pub(crate) provider: Box<str>,
    pub(crate) base_url: Box<str>,
    /// The wire the resolved model selected on this row: `None` for a named
    /// entry or a single-wire row. Carried so the request builders key
    /// per-wire behavior on the matched wire instead of re-scanning.
    pub(crate) wire: Option<&'static wire::WireDef>,
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
    env: &EnvSnapshot,
) -> Result<StreamContext, ProviderError> {
    let family = request_family(request)?;
    let model = http.resolved.entry.id.clone();
    let wire = wire::wire_for(&http.entry, &model);
    // An explicit table `auth` overrides every wire style; otherwise the
    // matched wire's style serves, and a single-wire row keeps its own.
    let auth = if http.entry.origins.auth == Origin::Explicit {
        http.entry.auth
    } else {
        wire.map_or(http.entry.auth, |w| w.auth)
    };
    Ok(StreamContext {
        family,
        provider: http.entry.id.clone(),
        base_url: anthropic_wire_base(
            &http.entry.id,
            wire.map(|w| w.family),
            wire::base(&http.entry, wire, env)?,
        )
        .into_owned()
        .into_boxed_str(),
        auth,
        model,
        entry: http.resolved.entry.clone(),
        request: request.clone(),
        thinking,
        session,
        client: http.client.clone(),
        user_agent: http.user_agent.clone(),
        wire,
    })
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
    /// The captured environment, for the endpoint template of a wired row.
    pub(crate) env: EnvSnapshot,
    /// The client identity of a Kimi Code OAuth credential; `None` for every
    /// other request. Resolved by [`client_identity_for`] before the
    /// attempt, so the attempt itself needs no provider set.
    pub(crate) client_identity: Option<Arc<ClientIdentity>>,
}

/// Whether `credential` is an OAuth sign-in of `provider` whose requests
/// name the vendor's client (Kimi Code): the row's hook says so. Its
/// requests carry the client identity, never Claude's fingerprint, and a
/// request of such a sign-in without the identity is refused.
pub(crate) fn names_client(provider: &str, credential: &Credential) -> bool {
    matches!(credential, Credential::OAuth(_))
        && crate::find(provider)
            .and_then(|def| def.oauth)
            .is_some_and(|oauth| oauth.hook.sends_client_identity())
}

/// The client identity one request of `provider` sends with `credential`:
/// the set's shared identity for a sign-in that names the client, `None`
/// for every other credential. Derived once per provider set, off the async
/// runtime.
///
/// # Errors
///
/// [`ProviderError::AuthWrite`] when the blocking derivation task fails.
pub(crate) async fn client_identity_for(
    owner: &ProviderSetInner,
    provider: &str,
    credential: &Credential,
) -> Result<Option<Arc<ClientIdentity>>, ProviderError> {
    if names_client(provider, credential) {
        owner.client_identity.get().await.map(Some)
    } else {
        Ok(None)
    }
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
            provider: context.request.model.provider().map(Box::from),
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
        Family::Devin => devin_attempt(&context, &credential, &cancel).await,
        Family::Droid => droid_attempt(&context, &credential, &cancel).await,
        Family::GoogleGenerativeAi
        | Family::GoogleVertex
        | Family::GoogleCloudCodeAssist
        | Family::GoogleAntigravity => {
            google_attempt(&context, &credential, &cancel, &owner, &slot.entry).await
        }
        // A Kimi Code sign-in names the vendor's client on every request.
        Family::Anthropic if names_client(&context.provider, &credential) => {
            let identity = owner
                .client_identity
                .get()
                .await
                .map_err(AttemptFailure::Provider)?;
            let parts = build_anthropic_stream_request(
                &context,
                &credential,
                credential_secrets(&credential),
                Some(identity.as_ref()),
            )
            .map_err(AttemptFailure::Provider)?;
            send_parts(&context, &credential, &cancel, parts).await
        }
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
    // A defaulted row base follows the resolved region: the helper borrows
    // when unchanged and owns only the actual rewrite. An explicit base
    // (`StreamContext.base_url: Box<str>`, request.rs:71) borrows verbatim
    // and only the signing scope follows the region.
    let base_url: std::borrow::Cow<'_, str> = match entry.origins.base {
        Origin::RowDefault => bedrock::endpoint_for_defaulted_base(&context.base_url, &region),
        Origin::Explicit => std::borrow::Cow::Borrowed(&*context.base_url),
    };
    let options = AwsResolveOptions {
        profile: None,
        region,
    };
    let first = Box::pin(bedrock_try(
        context, credential, &entry, &options, &base_url, cancel, owner, deadline,
    ))
    .await;
    if !is_bedrock_unauthorized(&first) {
        return first;
    }
    owner
        .aws_resolver
        .invalidate(options.profile.as_deref(), &options.region);
    Box::pin(bedrock_try(
        context, credential, &entry, &options, &base_url, cancel, owner, deadline,
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
    base_url: &str,
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
    let (builder, auth_secrets) = bedrock_request(context, &auth, &options.region, base_url)?;
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

/// Builds the Converse `RequestBuilder` for one resolved auth: an
/// `auth = "none"` entry sends no auth header and no signature, a bearer
/// token rides `Authorization: Bearer` with no signing, and `SigV4` signs
/// through the family's narrow signer seam. The second member is the
/// redaction list of the credential material the request carries; every
/// response and stream error of the attempt passes through it.
fn bedrock_request(
    context: &StreamContext,
    auth: &BedrockAuth,
    region: &str,
    base_url: &str,
) -> Result<(reqwest::RequestBuilder, Vec<Box<str>>), ProviderError> {
    match auth {
        BedrockAuth::None => {
            let path = bedrock::request_path(&context.model);
            let url = http::endpoint(Family::Bedrock, base_url, &path)?;
            let body = bedrock::request_body(&context.request, &context.thinking)
                .map_err(bedrock_body_error)?;
            let builder = context
                .client
                .post(url)
                .header("content-type", "application/json")
                .header("accept", BEDROCK_EVENTSTREAM_ACCEPT);
            Ok((builder.body(body), Vec::new()))
        }
        BedrockAuth::Bearer(token) => {
            let path = bedrock::request_path(&context.model);
            let url = http::endpoint(Family::Bedrock, base_url, &path)?;
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
                base_url,
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
    if let Some((_, authorization)) = wire
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
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

/// Opens one Gemini CLI turn: the headless child under admission, streaming
/// its neutral events.
///
/// The child runs the set-owned TOML deny-all policy with read-only approval
/// mode, the provider-settings binary (or `gemini` from `PATH`), and exactly
/// the captured environment and working directory: ambient state is cleared.
/// A missing captured working directory or policy fails closed. The stream
/// owns the child; dropping it signals only while still armed, and no
/// detached spawn outlives the turn. One attempt, no retry: a failed spawn
/// cannot succeed by resending.
///
/// # Errors
///
/// Returns `InvalidRequest` for a route mismatch or a missing policy, and
/// `Transport` for a spawn failure or a closed admission semaphore.
pub(crate) async fn gemini_cli_open(
    http: &Http,
    _session: SessionId,
    request: &ModelRequest,
    _notices: NoticeSink,
    cancel: &CancellationToken,
) -> Result<EventStream, ProviderError> {
    ensure_request(http, request)?;
    let owner = http
        .owner
        .upgrade()
        .ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider host has ended"),
        })?;
    if cancel.is_cancelled() {
        return Err(ProviderError::Transport {
            family: Family::GoogleGeminiCli,
            reason: String::from("provider request cancelled before it started"),
        });
    }
    let permit = transport_mod::acquire_provider_permit(&owner, &http.entry.id, cancel).await?;
    let policy = owner
        .gemini_policy
        .as_deref()
        .and_then(|path| path.to_str())
        .ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("the Gemini CLI deny-all policy is not initialized"),
        })?;
    let child = gemini_cli::spawn(&CliRequest {
        request,
        binary: http.entry.settings.binary.as_deref(),
        policy_file: policy,
        env: &owner.env,
    })?;
    Ok(transport_mod::hold_permit(
        EventStream::new(child.into_events(), || {}),
        permit,
    ))
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

const ACCOUNT_VERIFICATION_PREFIX: &str = "Account verification required.";

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
        max_output: context.entry.max_output,
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

/// Resolves the chat model for one Devin turn through the adapter's own
/// discovery: only a row the server marks as a router (and never a
/// composite) earns an `AssignModel` call; every other uid chats directly.
/// Returns the serving uid with its assignment JWT, if any.
async fn resolve_devin_chat_model(
    context: &StreamContext,
    credential: &Credential,
    wire_key: &str,
    router_uid: &str,
    base: &str,
    cancel: &CancellationToken,
) -> Result<Option<(String, Option<String>, Vec<DiscoveredModel>)>, AttemptFailure> {
    let discovery_url =
        http::endpoint(Family::Devin, base, PATH_DISCOVERY).map_err(AttemptFailure::Provider)?;
    let discovery_response = devin_unary(
        &context.client,
        discovery_url,
        UNARY_HEADERS,
        discovery_body(wire_key),
        &context.user_agent,
        cancel,
    )
    .await?;
    let Some(discovery_response) = discovery_response else {
        return Ok(None);
    };
    let discovery_bytes = devin_body(discovery_response, credential, cancel).await?;
    let Some(discovery_bytes) = discovery_bytes else {
        return Ok(None);
    };
    let rows = decode_discovery(&discovery_bytes).map_err(AttemptFailure::Provider)?;
    let routed = rows
        .iter()
        .find(|row| row.uid.as_str() == router_uid)
        .is_some_and(|row| row.is_model_router && row.harness_count == 0);
    if !routed {
        return Ok(Some((router_uid.to_owned(), None, rows)));
    }
    let cascade_id = context.session.to_string();
    let assign_url =
        http::endpoint(Family::Devin, base, PATH_ASSIGN).map_err(AttemptFailure::Provider)?;
    let assign_response = devin_unary(
        &context.client,
        assign_url,
        UNARY_HEADERS,
        assign_body(
            wire_key,
            router_uid,
            &cascade_id,
            router_prompt(&context.request).map_err(AttemptFailure::Provider)?,
        ),
        &context.user_agent,
        cancel,
    )
    .await?;
    let Some(assign_response) = assign_response else {
        return Ok(None);
    };
    let assign_bytes = devin_body(assign_response, credential, cancel).await?;
    let Some(assign_bytes) = assign_bytes else {
        return Ok(None);
    };
    let assigned: AssignModelResponse =
        decode_unary(&assign_bytes, "AssignModelResponse").map_err(|err| devin_protocol(&err))?;
    let assignment = assigned
        .assignment
        .filter(|assignment| {
            !assignment.model_uid.is_empty() && !assignment.assignment_jwt.is_empty()
        })
        .ok_or_else(|| {
            AttemptFailure::Provider(ProviderError::InvalidRequest {
                message: String::from("devin assignment did not resolve a serving model"),
            })
        })?;
    Ok(Some((
        assignment.model_uid,
        Some(assignment.assignment_jwt),
        rows,
    )))
}

/// Runs one Devin turn: `GetUserJwt`, `AssignModel`, then one
/// `GetChatMessage` stream.
///
/// Auth follows the adapter's two-key ladder: the session-token form first,
/// the raw key once on a 401. A server-directed base override replaces the
/// entry base for the calls after it. Assignment resolves the route's
/// router uid to its serving uid; a failed assignment fails the turn, never
/// falls back. The chat body goes out gzipped in one Connect frame and its
/// reply decodes as raw byte chunks, never SSE.
async fn devin_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
) -> Result<Option<EventStream>, AttemptFailure> {
    let api_key = match credential {
        Credential::ApiKey { key } => key.expose().to_owned(),
        Credential::OAuth(oauth) => oauth.access_token.expose().to_owned(),
        Credential::None => {
            return Err(AttemptFailure::Provider(ProviderError::NoCredentials {
                provider: context.provider.to_string(),
            }));
        }
    };
    let router_uid = context.request.model.id().to_owned();

    let Some((wire_key, jwt, base)) =
        devin_auth_exchange(context, credential, &api_key, cancel).await?
    else {
        return Ok(None);
    };

    // 2. Resolve the route through the adapter's own discovery: only a row
    // the server marks as a router (and never a composite) earns an
    // `AssignModel` call; every other uid chats directly.
    let Some((chat_model_uid, assignment_jwt, rows)) =
        resolve_devin_chat_model(context, credential, &wire_key, &router_uid, &base, cancel)
            .await?
    else {
        return Ok(None);
    };

    let cascade_id = context.session.to_string();
    let execution_id = Uuid::new_v4().to_string();
    // The server's own ceiling for the resolved serving uid wins over the
    // catalog row; the caller's explicit cap wins over both.
    let discovered_max = rows
        .iter()
        .find(|row| row.uid == chat_model_uid)
        .and_then(|row| u32::try_from(row.max_output_tokens).ok())
        .filter(|max| *max > 0);
    let max_tokens = context
        .request
        .params
        .max_output_tokens
        .or(discovered_max)
        .or(context.entry.max_output)
        .unwrap_or(4096);
    let frame = chat_frame(&DevinChatInput {
        request: &context.request,
        api_key: &wire_key,
        user_jwt: &jwt.user_jwt,
        cascade_id: &cascade_id,
        execution_id: &execution_id,
        chat_model_uid: &chat_model_uid,
        assignment_jwt: assignment_jwt.as_deref(),
        max_tokens,
        parallel_tool_calls: false,
        temperature: context.thinking.temperature.map(f64::from),
        top_p: None,
        stop_sequences: &[],
    })
    .map_err(AttemptFailure::Provider)?;
    let chat_url =
        http::endpoint(Family::Devin, &base, PATH_CHAT).map_err(AttemptFailure::Provider)?;
    let mut request = context.client.post(chat_url);
    for (name, value) in CHAT_HEADERS {
        request = request.header(name, value);
    }
    let chat_response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(None),
        result = http::send(
            Family::Devin,
            request.body(frame),
            CONNECT_USER_AGENT,
            Exchange::Bytes {
                accept: "*/*",
                total: None,
            },
            tokio::time::sleep,
        ) => result.map_err(AttemptFailure::Provider)?,
    };
    // The session JWT and the assignment JWT ride the chat body, so an error
    // or frame that echoes either is redacted like the API key itself.
    let mut secrets = credential_secrets(credential);
    secrets.push(Box::from(jwt.user_jwt.as_str()));
    secrets.extend(assignment_jwt.as_deref().map(Box::<str>::from));
    if !chat_response.status().is_success() {
        return match status_failure(chat_response, Family::Devin, credential, &secrets, cancel)
            .await?
        {
            Some(failure) => Err(failure),
            None => Ok(None),
        };
    }
    Ok(Some(transport_mod::decode_response(
        chat_response,
        Family::Devin,
        &context.provider,
        &context.model,
        false,
        None,
        secrets,
        None,
    )))
}

/// Sends one Devin unary call and returns its response head, whatever the
/// status. Cancellation returns `None` before or during the send.
async fn devin_unary(
    client: &reqwest::Client,
    url: url::Url,
    headers: [(&str, &str); 3],
    body: Vec<u8>,
    user_agent: &str,
    cancel: &CancellationToken,
) -> Result<Option<reqwest::Response>, AttemptFailure> {
    let mut request = client.post(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(None),
        result = http::send(
            Family::Devin,
            request.body(body),
            user_agent,
            Exchange::Bytes {
                accept: "application/proto",
                total: Some(NON_STREAM_TOTAL_TIMEOUT),
            },
            tokio::time::sleep,
        ) => result.map(Some).map_err(AttemptFailure::Provider),
    }
}

/// Reads one Devin unary body: a rejected response becomes the failure the
/// lifecycle classifies, cancellation returns `None`.
async fn devin_body(
    response: reqwest::Response,
    credential: &Credential,
    cancel: &CancellationToken,
) -> Result<Option<Vec<u8>>, AttemptFailure> {
    if !response.status().is_success() {
        return match status_failure(response, Family::Devin, credential, &[], cancel).await? {
            Some(failure) => Err(failure),
            None => Ok(None),
        };
    }
    http::read_body(Family::Devin, response)
        .await
        .map(Some)
        .map_err(AttemptFailure::Provider)
}

async fn devin_auth_exchange(
    context: &StreamContext,
    credential: &Credential,
    api_key: &str,
    cancel: &CancellationToken,
) -> Result<Option<(String, GetUserJwtResponse, Box<str>)>, AttemptFailure> {
    let auth_url = http::endpoint(Family::Devin, &context.base_url, PATH_AUTH)
        .map_err(AttemptFailure::Provider)?;
    let auth_response = devin_unary(
        &context.client,
        auth_url,
        UNARY_HEADERS,
        user_jwt_body(api_key),
        &context.user_agent,
        cancel,
    )
    .await?;
    let Some(auth_response) = auth_response else {
        return Ok(None);
    };
    let (wire_key, auth_response) = if auth_response.status().as_u16() == 401 {
        let retry_url = http::endpoint(Family::Devin, &context.base_url, PATH_AUTH)
            .map_err(AttemptFailure::Provider)?;
        let retry = devin_unary(
            &context.client,
            retry_url,
            UNARY_HEADERS,
            user_jwt_fallback_body(api_key),
            &context.user_agent,
            cancel,
        )
        .await?;
        let Some(retry) = retry else {
            return Ok(None);
        };
        (api_key.to_owned(), retry)
    } else {
        (normalize_session_token(api_key), auth_response)
    };
    let auth_bytes = devin_body(auth_response, credential, cancel).await?;
    let Some(auth_bytes) = auth_bytes else {
        return Ok(None);
    };
    let jwt: GetUserJwtResponse =
        decode_unary(&auth_bytes, "GetUserJwtResponse").map_err(|err| devin_protocol(&err))?;
    if jwt.user_jwt.is_empty() {
        return Err(AttemptFailure::Provider(ProviderError::Protocol {
            family: Family::Devin,
            detail: String::from("devin session exchange returned no token"),
        }));
    }
    let base: Box<str> = if jwt.custom_api_server_url.trim().is_empty() {
        context.base_url.clone()
    } else {
        jwt.custom_api_server_url.as_str().into()
    };
    Ok(Some((wire_key, jwt, base)))
}

fn devin_protocol(error: &ConnectError) -> AttemptFailure {
    AttemptFailure::Provider(ProviderError::Protocol {
        family: Family::Devin,
        detail: error.to_string(),
    })
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

/// Runs one Google turn: the surface lowers its wire, the shared transport
/// sends it (with the Antigravity endpoint failover before the first byte),
/// and the reply decodes as SSE through the shared decoder.
async fn google_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
    entry: &ProviderEntry,
) -> Result<Option<EventStream>, AttemptFailure> {
    match context.family {
        Family::GoogleGenerativeAi => {
            let api_key = match credential {
                Credential::ApiKey { key } => key.expose().to_owned(),
                Credential::OAuth(_) => {
                    return Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
                        message: format!(
                            "{} does not accept an OAuth credential",
                            context.provider
                        ),
                    }));
                }
                Credential::None => {
                    return Err(AttemptFailure::Provider(ProviderError::NoCredentials {
                        provider: context.provider.to_string(),
                    }));
                }
            };
            let wire = gemini::build(&GeminiRequest {
                request: &context.request,
                thinking: &context.thinking,
                api_key: &api_key,
                image_input: context.entry.image_input,
            })
            .map_err(AttemptFailure::Provider)?;
            google_send(
                context,
                &credential_secrets(credential),
                cancel,
                wire,
                false,
                None,
            )
            .await
        }
        Family::GoogleVertex => vertex_attempt(context, credential, cancel, owner, entry).await,
        Family::GoogleCloudCodeAssist => {
            google_cloud_code_assist_attempt(context, credential, cancel, owner, entry).await
        }
        Family::GoogleAntigravity => {
            google_antigravity_attempt(context, credential, cancel, owner, entry).await
        }
        _ => Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: String::from("not a Google request"),
        })),
    }
}

/// The OAuth sign-in a Cloud Code Assist surface needs. Both surfaces refuse
/// an API key and an absent credential, never sending a request without a
/// real sign-in.
fn cloud_code_oauth<'a>(
    context: &StreamContext,
    credential: &'a Credential,
) -> Result<&'a OAuthCredential, AttemptFailure> {
    match credential {
        Credential::OAuth(oauth) => Ok(oauth),
        Credential::ApiKey { .. } => Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!(
                "{} needs an OAuth sign-in, not an API key",
                context.provider
            ),
        })),
        Credential::None => Err(AttemptFailure::Provider(ProviderError::NoCredentials {
            provider: context.provider.to_string(),
        })),
    }
}

/// Runs one native Cloud Code Assist turn: the stored OAuth sign-in and the
/// project it stored lower to the plain `{project, model, request}` envelope,
/// the configured base is the only endpoint, and the reply decodes through
/// the shared Google decoder. A credential with no project never reaches the
/// wire: the module refuses a blank one.
async fn google_cloud_code_assist_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
    entry: &ProviderEntry,
) -> Result<Option<EventStream>, AttemptFailure> {
    let oauth = cloud_code_oauth(context, credential)?;
    let wire = cloud_code::build(&NativeRequest {
        request: &context.request,
        thinking: &context.thinking,
        provider: &context.provider,
        access_token: oauth.access_token.expose(),
        project: entry.cloud_code_project(oauth).unwrap_or_default(),
        identity: &owner.identity,
        image_input: context.entry.image_input,
    })
    .map_err(AttemptFailure::Provider)?;
    google_send(
        context,
        &credential_secrets(credential),
        cancel,
        wire,
        true,
        None,
    )
    .await
}

/// Environment variables that may carry the Vertex project, in resolve
/// order.
const VERTEX_PROJECT_ENV: &[&str] = &["GOOGLE_CLOUD_PROJECT", "GCP_PROJECT", "GCLOUD_PROJECT"];

/// Environment variables that may carry the Vertex location, in resolve
/// order.
const VERTEX_LOCATION_ENV: &[&str] = &[
    "GOOGLE_VERTEX_LOCATION",
    "GOOGLE_CLOUD_LOCATION",
    "VERTEX_LOCATION",
];

/// Environment variables that may carry a pre-minted Vertex bearer token,
/// in resolve order: the `GOOGLE_CLOUD_ACCESS_TOKEN` alias, then gcloud's own
/// override variable.
const VERTEX_TOKEN_ENV: &[&str] = &["GOOGLE_CLOUD_ACCESS_TOKEN", "CLOUDSDK_AUTH_ACCESS_TOKEN"];

/// The pre-minted Vertex bearer token the captured environment holds, if
/// any. Readiness and the request adapter share this one lookup.
pub(super) fn vertex_env_token(env: &EnvSnapshot) -> Option<&str> {
    env_value(env, VERTEX_TOKEN_ENV)
}

/// The first non-blank value among `names` in the captured snapshot.
fn env_value<'a>(env: &'a EnvSnapshot, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| env.get(name).filter(|value| !value.trim().is_empty()))
}

/// Resolves the Vertex project and location for one request: the provider
/// settings win, then the captured environment (`owner.env`, never the
/// process environment). A location from settings is an explicit residency
/// choice; one from the environment is ambient and may fall back to the
/// global endpoint on a 404. Readiness shares this one resolution so a row
/// can never drift from the request route.
pub(super) fn resolve_vertex_env<'a>(
    entry: &'a ProviderEntry,
    env: &'a EnvSnapshot,
) -> (Option<&'a str>, Option<&'a str>, bool) {
    let settings_project = entry
        .settings
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let settings_location = entry
        .settings
        .location
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let project = settings_project.or_else(|| env_value(env, VERTEX_PROJECT_ENV));
    let location = settings_location.or_else(|| env_value(env, VERTEX_LOCATION_ENV));
    (project, location, settings_location.is_some())
}

/// The Vertex bearer credential for one request: the stored OAuth sign-in,
/// else a pre-minted token from the captured environment. (The caller only
/// reaches here for a non-key credential.)
async fn vertex_bearer(
    credential: &Credential,
    owner: &ProviderSetInner,
    provider: &str,
    cancel: &CancellationToken,
) -> Result<Box<str>, AttemptFailure> {
    if let Credential::OAuth(oauth) = credential {
        return Ok(oauth.access_token.expose().into());
    }
    if let Some(token) = vertex_env_token(&owner.env) {
        return Ok(token.into());
    }
    owner
        .vertex_adc
        .resolve(cancel, provider)
        .await
        .map_err(AttemptFailure::Provider)
}

/// Validates the bearer route and returns the project/location pair the
/// wire carries; the refusal message names the settings keys and the
/// environment variables that can supply them. Nothing falls back to an
/// empty default.
fn require_vertex_route<'a>(
    provider: &str,
    project: Option<&'a str>,
    location: Option<&'a str>,
) -> Result<(&'a str, &'a str), AttemptFailure> {
    let usable = |value: Option<&str>| value.is_some_and(|v| !v.trim().is_empty());
    if let (Some(project), Some(location)) = (project, location)
        && usable(Some(project))
        && usable(Some(location))
    {
        return Ok((project, location));
    }
    Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
        message: format!(
            "{provider} needs a Vertex project and location: set providers.{provider}.project \
             and providers.{provider}.location, or the GOOGLE_CLOUD_PROJECT/GCP_PROJECT/\
             GCLOUD_PROJECT and GOOGLE_VERTEX_LOCATION/GOOGLE_CLOUD_LOCATION/VERTEX_LOCATION \
             environment variables"
        ),
    }))
}

/// Runs one Vertex turn: project/location come from the provider settings
/// with the captured environment as fallback; an Express key authenticates
/// directly, a bearer token comes from the stored sign-in or the captured
/// environment. Key precedence mirrors the reference: key first, then the
/// env bearer, else no credentials.
async fn vertex_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
    entry: &ProviderEntry,
) -> Result<Option<EventStream>, AttemptFailure> {
    let (project, location, explicit_location) = resolve_vertex_env(entry, &owner.env);
    // The credential's own secrets, plus the env bearer when that is what
    // authenticates: a token that never lived in the credential must still
    // be scrubbed from every error text and stream the request produces.
    let mut secrets = credential_secrets(credential);
    // The endpoint authority follows the entry's own table: only a row that
    // named no base lowers to the regional location host.
    let endpoint = match entry.origins.base {
        Origin::RowDefault => vertex::VertexEndpoint::Regional,
        Origin::Explicit => vertex::VertexEndpoint::ConfiguredProviderBase,
    };
    let bearer_token;
    let auth = match credential {
        Credential::ApiKey { key } => VertexAuth::Express {
            api_key: key.expose(),
            location,
            explicit_location,
        },
        Credential::OAuth(_) | Credential::None => {
            let (project, location) = require_vertex_route(&context.provider, project, location)?;
            bearer_token = vertex_bearer(credential, owner, &context.provider, cancel).await?;
            secrets.push(bearer_token.clone());
            VertexAuth::Bearer {
                access_token: &bearer_token,
                project,
                location,
            }
        }
    };
    let oauth = matches!(credential, Credential::OAuth(_));
    let wire = vertex::build(&VertexRequest {
        request: &context.request,
        thinking: &context.thinking,
        auth,
        image_input: context.entry.image_input,
        endpoint,
    })
    .map_err(AttemptFailure::Provider)?;
    google_send(context, &secrets, cancel, wire, oauth, None).await
}

async fn google_antigravity_attempt(
    context: &StreamContext,
    credential: &Credential,
    cancel: &CancellationToken,
    owner: &Arc<ProviderSetInner>,
    entry: &ProviderEntry,
) -> Result<Option<EventStream>, AttemptFailure> {
    let oauth = cloud_code_oauth(context, credential)?;
    let access_token = oauth.access_token.expose();
    let Some(project) = entry.cloud_code_project(oauth) else {
        return Err(AttemptFailure::Provider(ProviderError::InvalidRequest {
            message: format!(
                "{provider} has no Google Cloud project: run dalgon login {provider} again, \
                 or set providers.{provider}.project",
                provider = context.provider
            ),
        }));
    };
    let wire_model = context.request.model.id().to_owned();
    let model_enum = antigravity::wire_profile(&wire_model).and_then(|profile| profile.model_enum);
    let used_claude = google_shared::is_claude_model(&wire_model);
    let (envelope, response_slot) = {
        let mut conversations = lock(&owner.antigravity);
        let slot = conversations.entry(context.session).or_default();
        slot.session.seed_session_id(context.session.to_string());
        if let Ok(mut seen) = slot.response.lock()
            && let Some(id) = seen.take()
        {
            slot.session.note_response_id(id);
        }
        (
            slot.session.envelope(model_enum, used_claude),
            slot.response.clone(),
        )
    };
    let claude_thinking = matches!(context.thinking.wire, WireThinking::Anthropic { .. });
    let input = AntigravityRequest {
        request: &context.request,
        thinking: &context.thinking,
        access_token,
        project,
        wire_model: &wire_model,
        client_version: antigravity::CLIENT_VERSION,
        envelope: &envelope,
        claude_thinking,
        image_input: context.entry.image_input,
    };
    let configured = context.base_url.trim();
    let endpoints = if configured.is_empty() {
        antigravity::endpoints(None)
    } else {
        antigravity::endpoints(Some(&context.base_url))
    };
    antigravity_failover(
        context,
        &credential_secrets(credential),
        cancel,
        &endpoints,
        || antigravity::build(&input),
        &response_slot,
    )
    .await
}

/// Sends one Antigravity wire to each endpoint in turn and returns the first
/// stream, failing over before any stream byte is delivered.
///
/// Endpoint selection stays with the caller (`antigravity::endpoints`); this
/// owns only the failover rule, so a test can drive it with loopback
/// endpoints.
pub(super) async fn antigravity_failover<F>(
    context: &StreamContext,
    secrets: &[Box<str>],
    cancel: &CancellationToken,
    endpoints: &[String],
    build: F,
    response_slot: &google_shared::ResponseIdSlot,
) -> Result<Option<EventStream>, AttemptFailure>
where
    F: Fn() -> Result<google_shared::GoogleWire, ProviderError>,
{
    let mut last_failure: Option<AttemptFailure> = None;
    for endpoint in endpoints {
        let mut wire = build().map_err(AttemptFailure::Provider)?;
        wire.base_override = Some(endpoint.clone());
        match send_google_wire(context, cancel, wire).await {
            Ok(Some(response)) => {
                match google_decoded(
                    context,
                    secrets,
                    cancel,
                    response,
                    true,
                    Some(response_slot.clone()),
                )
                .await
                {
                    Ok(stream) => return Ok(stream),
                    Err(failure) => {
                        let retryable = match &failure {
                            AttemptFailure::Response {
                                status,
                                code,
                                message,
                                ..
                            } => {
                                !message.starts_with(ACCOUNT_VERIFICATION_PREFIX)
                                    && matches!(
                                        crate::retry::classify(
                                            Some(*status),
                                            code.as_deref(),
                                            message,
                                            &RequestState {
                                                family: context.family,
                                                provider: &context.provider,
                                                model: &context.model,
                                                oauth: true,
                                                refreshed: false,
                                                delivered: false,
                                            },
                                        ),
                                        RetryDecision::Retry(_)
                                    )
                            }
                            AttemptFailure::Provider(ProviderError::Transport { .. }) => true,
                            AttemptFailure::Provider(_) => false,
                        };
                        if retryable {
                            last_failure = Some(failure);
                        } else {
                            return Err(failure);
                        }
                    }
                }
            }
            Ok(None) => return Ok(None),
            Err(failure @ AttemptFailure::Provider(ProviderError::Transport { .. })) => {
                last_failure = Some(failure);
            }
            Err(failure) => return Err(failure),
        }
    }
    Err(last_failure.unwrap_or_else(|| {
        AttemptFailure::Provider(ProviderError::Transport {
            family: Family::GoogleAntigravity,
            reason: String::from("the antigravity endpoints answered nothing"),
        })
    }))
}

/// Sends one Google wire and decodes its SSE reply through the shared
/// decoder, with Google error classification on rejection.
async fn google_send(
    context: &StreamContext,
    secrets: &[Box<str>],
    cancel: &CancellationToken,
    wire: google_shared::GoogleWire,
    oauth: bool,
    response_id: Option<google_shared::ResponseIdSlot>,
) -> Result<Option<EventStream>, AttemptFailure> {
    let response = send_google_wire(context, cancel, wire).await?;
    let Some(response) = response else {
        return Ok(None);
    };
    google_decoded(context, secrets, cancel, response, oauth, response_id).await
}

async fn send_google_wire(
    context: &StreamContext,
    cancel: &CancellationToken,
    wire: google_shared::GoogleWire,
) -> Result<Option<reqwest::Response>, AttemptFailure> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(None),
        result = google_shared::send(
            context.family,
            &context.client,
            wire,
            &context.base_url,
            &context.user_agent,
        ) => result.map(Some).map_err(AttemptFailure::Provider),
    }
}

async fn google_decoded(
    context: &StreamContext,
    secrets: &[Box<str>],
    cancel: &CancellationToken,
    response: reqwest::Response,
    oauth: bool,
    response_id: Option<google_shared::ResponseIdSlot>,
) -> Result<Option<EventStream>, AttemptFailure> {
    if !response.status().is_success() {
        return match google_status_failure(
            response,
            context.family,
            &context.provider,
            &context.model,
            oauth,
            secrets,
            cancel,
        )
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
        oauth,
        None,
        secrets.to_vec(),
        response_id,
    )))
}

/// Maps one rejected Google response: account exhaustion is terminal
/// quota through the adapter classifier, a validated account-verification
/// URL on the Cloud Code Assist flows renders the fixed actionable message,
/// and everything else rides the shared lifecycle as a response failure.
/// Quota keeps precedence over verification; the rendered message (including
/// the JSON-decoded URL) is re-redacted, and the URL is never fetched.
async fn google_status_failure(
    response: reqwest::Response,
    family: Family,
    provider: &str,
    model: &str,
    oauth: bool,
    secrets: &[Box<str>],
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
    let redacted = transport_mod::redact_text(String::from_utf8_lossy(&body).into_owned(), secrets);
    let (code, message) = google_shared::error_fields(&redacted);
    let state = RequestState {
        family,
        provider,
        model,
        oauth,
        refreshed: false,
        delivered: false,
    };
    let mapped = google_shared::classify_http(status, &redacted, &state);
    if matches!(mapped, ProviderError::Quota { .. }) {
        return Ok(Some(AttemptFailure::Provider(mapped)));
    }
    // The named reference callers (`google-gemini-cli.ts`, its OAuth
    // registry, and the Antigravity OAuth registry) render the fixed
    // account-verification message with `retry your request` and no
    // email or PII when the structured URL validates; ordinary error
    // mapping below is untouched.
    if matches!(
        family,
        Family::GoogleCloudCodeAssist | Family::GoogleAntigravity
    ) && let Some(url) = antigravity::validation_url(&redacted)
    {
        let message = transport_mod::redact_text(
            format!(
                "{ACCOUNT_VERIFICATION_PREFIX} Visit {url} to continue, then retry your request."
            ),
            secrets,
        );
        return Ok(Some(AttemptFailure::Response {
            status,
            code,
            message,
            retry_after,
        }));
    }
    Ok(Some(AttemptFailure::Response {
        status,
        code,
        message,
        retry_after,
    }))
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

fn build_chat_stream_request(
    context: &StreamContext,
    credential: &Credential,
    secrets: Vec<Box<str>>,
    user_agent: Box<str>,
) -> Result<StreamRequestParts, ProviderError> {
    let dialect = family::chat::Dialect::of(&context.provider, &context.base_url, &context.model);
    let body = family::chat::request_body(&context.request, &context.thinking, dialect).map_err(
        |error| ProviderError::InvalidRequest {
            message: error.to_string(),
        },
    )?;
    let url = http::endpoint(
        context.family,
        &context.base_url,
        family::chat::ENDPOINT_PATH,
    )?;
    let mut request = context.client.post(url);
    // A credential-less row (a keyless local gateway such as
    // llama.cpp, configured `auth = "none"`) sends no header.
    if let Some((header, value)) =
        optional_auth_header(context.auth, credential, context.family, &context.provider)?
    {
        request = request.header(header, value);
    }
    if let Some((name, value)) = session_header(context) {
        request = request.header(name, value);
    }
    Ok((request.body(body), user_agent, secrets, None))
}

fn build_mistral_stream_request(
    context: &StreamContext,
    credential: &Credential,
    secrets: Vec<Box<str>>,
    user_agent: Box<str>,
) -> Result<StreamRequestParts, ProviderError> {
    let body =
        family::mistral::request_body(&context.request, &context.thinking).map_err(|error| {
            ProviderError::InvalidRequest {
                message: error.to_string(),
            }
        })?;
    let url = http::endpoint(
        context.family,
        &context.base_url,
        family::mistral::ENDPOINT_PATH,
    )?;
    let mut request = context.client.post(url);
    if let Some((header, value)) =
        optional_auth_header(context.auth, credential, context.family, &context.provider)?
    {
        request = request.header(header, value);
    }
    Ok((request.body(body), user_agent, secrets, None))
}

fn build_ollama_stream_request(
    context: &StreamContext,
    credential: &Credential,
    secrets: Vec<Box<str>>,
    user_agent: Box<str>,
) -> Result<StreamRequestParts, ProviderError> {
    let deployment = family::ollama::Deployment::of(&context.provider, &context.base_url);
    let policy =
        family::ollama::WirePolicy::for_model(deployment, &context.model, &context.entry.thinking);
    let body = family::ollama::request_body(&context.request, &context.thinking, policy).map_err(
        |error| ProviderError::InvalidRequest {
            message: error.to_string(),
        },
    )?;
    let base = family::ollama::trim_api_suffix(&context.base_url);
    let url = http::endpoint(context.family, base, family::ollama::ENDPOINT_PATH)?;
    let mut request = context.client.post(url);
    if let Some((header, value)) =
        optional_auth_header(context.auth, credential, context.family, &context.provider)?
    {
        request = request.header(header, value);
    }
    Ok((request.body(body), user_agent, secrets, None))
}

fn build_pi_stream_request(
    context: &StreamContext,
    credential: &Credential,
    secrets: Vec<Box<str>>,
    user_agent: Box<str>,
) -> Result<StreamRequestParts, ProviderError> {
    let body =
        family::pi_messages::request_body(&context.request, &context.provider, &context.thinking)
            .map_err(|error| ProviderError::InvalidRequest {
            message: error.to_string(),
        })?;
    let url = http::endpoint(
        context.family,
        &context.base_url,
        family::pi_messages::ENDPOINT_PATH,
    )?;
    let (header, value) = auth_header(context.auth, credential, context.family, &context.provider)?;
    Ok((
        context.client.post(url).header(header, value).body(body),
        user_agent,
        secrets,
        None,
    ))
}

pub(crate) fn build_stream_request(
    context: &StreamContext,
    credential: &Credential,
) -> Result<StreamRequestParts, ProviderError> {
    let route_family = context.family;
    let user_agent = context.user_agent.clone();
    let secrets = credential_secrets(credential);
    match route_family {
        Family::Chat => build_chat_stream_request(context, credential, secrets, user_agent),
        Family::Responses => {
            let wire = build_responses_wire_from_context(context, credential)?;
            let url = http::endpoint(route_family, &context.base_url, "responses")?;
            let mut request = context.client.post(url);
            if let Some((name, value)) = &wire.auth_header {
                request = request.header(*name, value);
            }
            if let Some((name, value)) = session_header(context) {
                request = request.header(name, value);
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
        Family::GoogleGenerativeAi
        | Family::GoogleVertex
        | Family::GoogleCloudCodeAssist
        | Family::GoogleAntigravity => Err(ProviderError::InvalidRequest {
            message: String::from("Google requests use the dedicated GenerateContent adapter"),
        }),
        Family::GoogleGeminiCli => Err(ProviderError::InvalidRequest {
            message: String::from("Gemini CLI requests use the dedicated subprocess adapter"),
        }),
        Family::Mistral => build_mistral_stream_request(context, credential, secrets, user_agent),
        Family::Ollama => build_ollama_stream_request(context, credential, secrets, user_agent),
        Family::PiMessages => build_pi_stream_request(context, credential, secrets, user_agent),
        Family::Anthropic => build_anthropic_stream_request(context, credential, secrets, None),
    }
}

fn build_anthropic_stream_request(
    context: &StreamContext,
    credential: &Credential,
    secrets: Vec<Box<str>>,
    client_identity: Option<&ClientIdentity>,
) -> Result<StreamRequestParts, ProviderError> {
    let mut wire = build_anthropic_wire(
        &context.request,
        &context.entry,
        context.auth,
        &context.thinking,
        credential,
        WireOptions::turn()
            .named_by(client_identity)
            .with_tool_schemas(anthropic_tool_schemas(
                context.provider.as_ref(),
                context.wire.map(|w| w.family),
            )),
    )?;
    let url = http::endpoint(context.family, &context.base_url, wire.path)?;
    let user_agent = wire
        .user_agent
        .as_deref()
        .unwrap_or(context.user_agent.as_ref())
        .to_owned()
        .into_boxed_str();
    let replay_prefix = std::mem::take(&mut wire.prefix);
    let mut request = wire.into_request(&context.client, url);
    if let Some((name, value)) = session_header(context) {
        request = request.header(name, value);
    }
    Ok((request, user_agent, secrets, Some(replay_prefix)))
}

/// The row's session header with the explicit `SessionId` of this turn:
/// `None` for every row that names none. The value is the dal conversation
/// id `Provider::open` received, never an inferred one and never a
/// per-request field.
fn session_header(context: &StreamContext) -> Option<(&'static str, String)> {
    let name = crate::find(&context.provider)?.session_header?;
    Some((name, context.session.to_string()))
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
        max_output: context.entry.max_output,
        auth: context.auth,
        credential,
        session_id: context.session,
        user_agent: &context.user_agent,
        // One trusted selector for every Responses request, stream and
        // compact alike: the tool-schema flavor is the matched wire's, keyed
        // on the row and the wire family — never on model-id text. The B.AI
        // Responses wire repairs union-root tool schemas (`OpenAiCompat`);
        // every other Responses wire sends them verbatim.
        tool_schemas: match (context.provider.as_ref(), context.wire.map(|w| w.family)) {
            ("bai", Some(Family::Responses)) => crate::family::schema::ToolSchemas::OpenAiCompat,
            _ => crate::family::schema::ToolSchemas::Verbatim,
        },
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

/// How one Anthropic wire is lowered beyond its request and credential.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WireOptions<'a> {
    /// Whether this is the summarize (remote compaction) request.
    summarize: bool,
    /// The vendor client identity a Kimi Code sign-in sends; `None` for
    /// every other credential.
    client_identity: Option<&'a ClientIdentity>,
    /// The tool-schema flavor the matched wire selected; `Verbatim`
    /// everywhere but the B.AI Anthropic wire.
    tool_schemas: family::schema::ToolSchemas,
}

impl<'a> WireOptions<'a> {
    /// An ordinary turn.
    pub(crate) const fn turn() -> Self {
        Self {
            summarize: false,
            client_identity: None,
            tool_schemas: family::schema::ToolSchemas::Verbatim,
        }
    }

    /// The summarize request of a remote compaction.
    pub(crate) const fn summarize() -> Self {
        Self {
            summarize: true,
            client_identity: None,
            tool_schemas: family::schema::ToolSchemas::Verbatim,
        }
    }

    /// The same options for a request that names the vendor's client.
    pub(crate) const fn named_by(self, client_identity: Option<&'a ClientIdentity>) -> Self {
        Self {
            client_identity,
            ..self
        }
    }

    /// The same options with the tool-schema flavor the matched wire
    /// selected.
    pub(crate) const fn with_tool_schemas(self, tool_schemas: family::schema::ToolSchemas) -> Self {
        Self {
            tool_schemas,
            ..self
        }
    }
}

/// The effective base of one Anthropic wire, keyed on the row and the wire
/// family. The B.AI wire path already carries the `v1` version segment in
/// `v1/messages`, so a base ending in `/v1` keeps exactly one version: the
/// trailing segment is dropped here and re-added by the path. Every other
/// row keeps its base verbatim.
fn anthropic_wire_base<'b>(
    provider: &str,
    wire_family: Option<Family>,
    base: Cow<'b, str>,
) -> Cow<'b, str> {
    if (provider, wire_family) != ("bai", Some(Family::Anthropic)) {
        return base;
    }
    let path_start = base
        .find("://")
        .and_then(|scheme_end| base[scheme_end + 3..].find('/').map(|p| scheme_end + 3 + p));
    let Some(path_start) = path_start else {
        return base;
    };
    let trimmed = {
        let (origin, path) = base.split_at(path_start);
        let path = path.strip_suffix('/').unwrap_or(path);
        path.strip_suffix("/v1")
            .map(|kept| format!("{origin}{kept}"))
    };
    match trimmed {
        Some(joined) => Cow::Owned(joined),
        None => base,
    }
}
/// The tool-schema flavor of one Anthropic wire, keyed on the row and the
/// wire family — never on model-id text. The B.AI Anthropic wire repairs
/// union-root tool schemas (`ToolSchemas::OpenAiCompat`, per-wire parity
/// with its Chat and Responses wires); every other Anthropic wire sends them
/// verbatim.
fn anthropic_tool_schemas(
    provider: &str,
    wire_family: Option<Family>,
) -> family::schema::ToolSchemas {
    match (provider, wire_family) {
        ("bai", Some(Family::Anthropic)) => family::schema::ToolSchemas::OpenAiCompat,
        _ => family::schema::ToolSchemas::Verbatim,
    }
}

pub(crate) fn build_anthropic_wire(
    request: &ModelRequest,
    entry: &CatalogEntry,
    auth_style: AuthStyle,
    thinking: &ThinkingPlan,
    credential: &Credential,
    options: WireOptions<'_>,
) -> Result<family::anthropic::AnthropicWire, ProviderError> {
    let WireOptions {
        summarize,
        client_identity,
        tool_schemas,
    } = options;
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
        AuthStyle::Bearer | AuthStyle::XApiKey | AuthStyle::CfAigAuthorization => {}
    }
    let auth = match credential {
        Credential::ApiKey { key } => match auth_style {
            AuthStyle::XApiKey => AnthropicAuth::ApiKey(key.expose()),
            AuthStyle::Bearer => AnthropicAuth::Bearer(key.expose()),
            AuthStyle::CfAigAuthorization => AnthropicAuth::GatewayKey { key: key.expose() },
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
        Credential::OAuth(oauth) if auth_style == AuthStyle::CfAigAuthorization => {
            return Err(ProviderError::InvalidRequest {
                message: format!(
                    "{} carries its gateway key in cf-aig-authorization, never an OAuth credential",
                    entry.provider
                ),
            });
        }
        Credential::OAuth(oauth) => match client_identity {
            Some(identity) => AnthropicAuth::ClientBearer {
                access_token: oauth.access_token.expose(),
                user_agent: identity.user_agent(),
                headers: identity.headers(),
            },
            // A sign-in that names the vendor's client never falls back to
            // Claude's fingerprint: without the identity the request is
            // refused.
            None if names_client(&entry.provider, credential) => {
                return Err(ProviderError::InvalidRequest {
                    message: format!(
                        "{} needs its client identity before it can send a request",
                        entry.provider
                    ),
                });
            }
            None => AnthropicAuth::ClaudeOAuth {
                access_token: oauth.access_token.expose(),
                version: crate::claude_fingerprint::CLAUDE_CODE_VERSION,
            },
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
    family::anthropic::build_with_tool_schemas(&request, auth, tool_schemas)
}

/// Maps one rejected response the way its family requires: Devin bodies go
/// through the family's bounded detail sanitizer (untrusted markup and
/// control text fall back to a fixed message), everything else rides the
/// shared first-line mapping. Status, code, and retry classification are
/// preserved on every path; cancellation returns `None` before or during
/// the body read.
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
    // The content type is header metadata: capture it before the body read
    // consumes the response, or the Devin sanitizer cannot refuse proxy HTML.
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
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
    if family == Family::Devin {
        return Ok(Some(devin_failure(
            status,
            content_type.as_deref(),
            &body,
            &secrets,
            retry_after,
        )));
    }
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

/// The fixed message for Devin bodies the sanitizer refuses: it carries no
/// body text at all, so proxied markup and control bytes never surface.
const DEVIN_GENERIC_ERROR: &str = "devin request failed";

/// Maps one rejected Devin response: the sanitizer's bounded safe detail
/// when the redacted body carries one, else the generic fallback. The body
/// arrives already bounded by the transport read ceiling and the detail by
/// the sanitizer's own cap, so no extra truncation here: cutting mid-JSON
/// would break envelope unwrapping and lose the code. Status, code, and
/// retry classification are preserved either way, and the surfaced detail
/// is re-redacted after JSON unescaping so an escaped credential never
/// appears.
fn devin_failure(
    status: u16,
    content_type: Option<&str>,
    body: &[u8],
    secrets: &[Box<str>],
    retry_after: Option<String>,
) -> AttemptFailure {
    let redacted = transport_mod::redact_text(String::from_utf8_lossy(body).into_owned(), secrets);
    let (code, _) = transport_mod::error_fields(&redacted);
    let message = error_detail(redacted.as_bytes(), content_type).map_or_else(
        || String::from(DEVIN_GENERIC_ERROR),
        |detail| transport_mod::redact_text(detail, secrets),
    );
    AttemptFailure::Response {
        status,
        code: code.map(|code| transport_mod::redact_text(code, secrets)),
        message,
        retry_after,
    }
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
            let stream_context =
                compact_stream_context(&context).map_err(AttemptFailure::Provider)?;
            let wire = build_responses_wire_from_context(&stream_context, &credential)
                .map_err(AttemptFailure::Provider)?;
            let headers = wire
                .auth_header
                .into_iter()
                .chain(session_header(&stream_context))
                .collect::<Vec<_>>();
            compact::openai_responses(
                &context.client,
                stream_context.base_url.as_ref(),
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
            let stream_context =
                compact_stream_context(&context).map_err(AttemptFailure::Provider)?;
            let wire = build_codex_wire(
                &stream_context,
                &context.request,
                &context.thinking,
                oauth,
                context.session,
            )
            .map_err(AttemptFailure::Provider)?;
            compact::openai_codex(
                &context.client,
                stream_context.base_url.as_ref(),
                wire,
                &cancel,
            )
            .await
        }
        Family::Anthropic => {
            let stream_context =
                compact_stream_context(&context).map_err(AttemptFailure::Provider)?;
            let wire = build_anthropic_wire(
                &context.request,
                &context.resolved.entry,
                stream_context.auth,
                &context.thinking,
                &credential,
                WireOptions::summarize()
                    .named_by(context.client_identity.as_deref())
                    .with_tool_schemas(anthropic_tool_schemas(
                        stream_context.provider.as_ref(),
                        stream_context.wire.map(|w| w.family),
                    )),
            )
            .map_err(AttemptFailure::Provider)?;
            compact::anthropic(
                &context.client,
                &stream_context.base_url,
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
        | Family::GoogleCloudCodeAssist
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

pub(crate) fn compact_stream_context(
    context: &CompactContext,
) -> Result<StreamContext, ProviderError> {
    let model = context.resolved.entry.id.clone();
    let wire = wire::wire_for(&context.provider, &model);
    let auth = if context.provider.origins.auth == Origin::Explicit {
        context.provider.auth
    } else {
        wire.map_or(context.provider.auth, |w| w.auth)
    };
    Ok(StreamContext {
        family: context.family,
        provider: context.provider.id.clone(),
        base_url: wire::base(&context.provider, wire, &context.env)?
            .into_owned()
            .into_boxed_str(),
        auth,
        model,
        entry: context.resolved.entry.clone(),
        request: context.request.clone(),
        thinking: context.thinking.clone(),
        session: context.session,
        client: context.client.clone(),
        user_agent: context.user_agent.clone(),
        wire,
    })
}

pub(crate) fn ensure_request(http: &Http, request: &ModelRequest) -> Result<(), ProviderError> {
    let ModelRoute::Api {
        family,
        model,
        provider,
    } = &request.model
    else {
        return Err(ProviderError::InvalidRequest {
            message: String::from("an HTTP provider needs a concrete API model route"),
        });
    };
    let other_provider = provider
        .as_deref()
        .is_some_and(|named| named != http.entry.id.as_ref());
    // The bound route's family, not the row's logical default: a wired row
    // serves other families on their wires, and `Provider::new` already
    // authorized this route against the wire table.
    let bound_family = match &http.resolved.route {
        ModelRoute::Api { family, .. } => *family,
        // Unreachable after `Provider::new`, which refuses every non-API
        // route for an HTTP provider; the row family keeps the match total.
        _ => http.entry.family,
    };
    if *family != bound_family || model.as_ref() != http.model() || other_provider {
        return Err(ProviderError::InvalidRequest {
            message: String::from("request model does not match the bound provider route"),
        });
    }
    Ok(())
}

pub(crate) fn tool_names(http: &Http, request: &ModelRequest) -> Result<ToolNames, ProviderError> {
    // Only Claude's OAuth prefixes tool names, so only it gets the reduced
    // cap; a Kimi Code sign-in sends names as an API key does.
    let max = match request_family(request)? {
        Family::Anthropic
            if matches!(&http.credential, Credential::OAuth(_))
                && !names_client(&http.entry.id, &http.credential) =>
        {
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
    let catalog = http.resolved.entry.max_output;
    let requested = request.params.max_output_tokens;
    // The caller's requested cap bounds the thinking budget too: the planner
    // shrinks or drops a budget that does not fit under it.
    let (max_tokens, max_output) = if route_family == Family::Anthropic {
        (
            family::anthropic::base_max_tokens(catalog, requested),
            family::anthropic::output_ceiling(catalog, requested),
        )
    } else {
        // Droid and GitLab Duo route by their own family yet plan a budget
        // for their Anthropic wires from these limits (unknown ceiling: 0).
        let ceiling = catalog.unwrap_or(0);
        let ceiling = requested.map_or(ceiling, |ask| ask.min(ceiling));
        (ceiling, ceiling)
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
            AuthStyle::CfAigAuthorization => {
                Ok(("cf-aig-authorization", format!("Bearer {}", key.expose())))
            }
            AuthStyle::None => Err(ProviderError::InvalidRequest {
                message: String::from("credential-less entries carry no auth header"),
            }),
        },
        Credential::OAuth(oauth)
            if matches!(
                family,
                Family::Codex | Family::Anthropic | Family::PiMessages
            ) =>
        {
            Ok((
                "authorization",
                format!("Bearer {}", oauth.access_token.expose()),
            ))
        }
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
            provider: (!entry.provider.is_empty()).then(|| entry.provider.clone()),
        },
        name: format!("{} · {}/{}", entry.display, entry.provider, entry.id).into_boxed_str(),
        provider: entry.provider.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A signed-in Radius row carries an OAuth credential, and the pi-messages
    /// wire sends it as a bearer. Refusing it would make `login radius`
    /// succeed and then fail every turn.
    #[test]
    fn pi_messages_accepts_an_oauth_bearer() {
        let oauth = Credential::OAuth(OAuthCredential {
            access_token: "radius-token".into(),
            refresh_token: "refresh".into(),
            expires_at: None,
            id_token: None,
            account_id: None,
            project_id: None,
        });
        let header = auth_header(AuthStyle::Bearer, &oauth, Family::PiMessages, "radius")
            .expect("the radius row accepts its OAuth credential");
        assert_eq!(header, ("authorization", "Bearer radius-token".to_owned()));
    }

    /// The pre-minted bearer order: the `GOOGLE_CLOUD_ACCESS_TOKEN` alias
    /// wins, the gcloud override answers alone, a blank alias is skipped,
    /// and nothing derives from an empty snapshot.
    #[test]
    fn vertex_env_token_prefers_the_cloud_alias_over_the_gcloud_override() {
        let both = EnvSnapshot::test(&[
            ("GOOGLE_CLOUD_ACCESS_TOKEN", "cloud-bearer"),
            ("CLOUDSDK_AUTH_ACCESS_TOKEN", "sdk-bearer"),
        ]);
        assert_eq!(vertex_env_token(&both), Some("cloud-bearer"));
        let only_sdk = EnvSnapshot::test(&[("CLOUDSDK_AUTH_ACCESS_TOKEN", "sdk-bearer")]);
        assert_eq!(vertex_env_token(&only_sdk), Some("sdk-bearer"));
        let blank_alias = EnvSnapshot::test(&[
            ("GOOGLE_CLOUD_ACCESS_TOKEN", "   "),
            ("CLOUDSDK_AUTH_ACCESS_TOKEN", "sdk-bearer"),
        ]);
        assert_eq!(vertex_env_token(&blank_alias), Some("sdk-bearer"));
        assert_eq!(vertex_env_token(&EnvSnapshot::test(&[])), None);
    }

    /// The bearer route gate: both a project and a location are required,
    /// blanks do not count, and the refusal names the settings keys and the
    /// environment variables that can supply them.
    #[test]
    fn vertex_bearer_route_requires_project_and_location() {
        assert!(require_vertex_route("vertex", Some("p"), Some("l")).is_ok());
        for (project, location) in [
            (Some("p"), None),
            (None, Some("l")),
            (Some(" "), Some("l")),
            (Some("p"), Some("")),
        ] {
            match require_vertex_route("vertex", project, location) {
                Err(AttemptFailure::Provider(ProviderError::InvalidRequest { message })) => {
                    assert!(message.contains("providers.vertex.project"), "{message}");
                    assert!(message.contains("GOOGLE_CLOUD_PROJECT"), "{message}");
                    assert!(message.contains("GOOGLE_VERTEX_LOCATION"), "{message}");
                }
                other => panic!("expected a route refusal, got {other:?}"),
            }
        }
    }

    /// The shared route resolution: settings win over the captured
    /// environment, and the ambient flag follows the settings location only.
    #[test]
    fn vertex_route_resolution_prefers_settings_over_environment() {
        let entry =
            |project: Option<&str>, location: Option<&str>| crate::provider::ProviderEntry {
                id: "vertex".into(),
                def: None,
                family: Family::GoogleVertex,
                base_url: String::from("https://aiplatform.googleapis.com").into_boxed_str(),
                transport: crate::provider::Transport::Https,
                key_env: None,
                auth: crate::provider::AuthStyle::Bearer,
                max_concurrent_requests: 1,
                origins: crate::provider::Origins::default(),
                settings: crate::provider::ProviderSettings {
                    project: project.map(Into::into),
                    location: location.map(Into::into),
                    ..crate::provider::ProviderSettings::default()
                },
            };
        let ambient = EnvSnapshot::test(&[
            ("GOOGLE_CLOUD_PROJECT", "env-project"),
            ("GOOGLE_VERTEX_LOCATION", "env-location"),
        ]);
        let explicit_entry = entry(Some("set"), Some("set"));
        let (project, location, explicit) = resolve_vertex_env(&explicit_entry, &ambient);
        assert_eq!(project, Some("set"));
        assert_eq!(location, Some("set"));
        assert!(explicit, "a settings location is an explicit residency");
        let ambient_entry = entry(None, None);
        let (project, location, explicit) = resolve_vertex_env(&ambient_entry, &ambient);
        assert_eq!(project, Some("env-project"));
        assert_eq!(location, Some("env-location"));
        assert!(!explicit, "an environment location is ambient");
    }
}
