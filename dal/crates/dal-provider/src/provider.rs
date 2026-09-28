//! Provider request orchestration and the public family-neutral provider API.

mod config;
mod set;

pub use config::{AuthStyle, ProviderConfig, ProviderConfigError, ProviderEntry, Transport};
pub use set::{ProviderIdentity, ProviderSet};

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
};

use dal_core::{
    Caps, ContextItem, Family, ModelInfo, ModelRequest, ModelRoute, Purpose, SessionId,
    ThinkingLevel, Usage,
};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{
        credential::{Credential, OAuthCredential},
        refresh::{OAuthProvider, RefreshReason, Refresher},
    },
    catalog::{self, CatalogEntry, CatalogFetch, ModelFetch, ResolvedModel},
    compact::{self, CompactOutcome, CompactedHistory},
    error::{ProviderError, UsageCheckReason},
    family::{
        self,
        anthropic::{AnthropicAuth, AnthropicRequest},
        codex::CodexRequest,
        responses::ResponsesRequest,
    },
    http::{self, Exchange},
    lifecycle::{self, AttemptFailure, Plan},
    stream::{EventStream, NoticeSink, StreamEvent},
    thinking::{
        self, BeforeRequest, BeforeRequestPatch, Effort, RequestLimits, SessionNotices,
        ThinkingNotice, ThinkingPlan, ThinkingSupport, WireThinking,
    },
    usage::UsageVerdict,
};

/// One HTTP-backed provider adapter. Its fields stay private; construct it
/// through [`ProviderSet::provider`] or [`Provider::new`].
#[must_use]
pub struct Http {
    resolved: ResolvedModel,
    entry: ProviderEntry,
    credential: Credential,
    client: reqwest::Client,
    user_agent: Box<str>,
    owner: Weak<set::ProviderSetInner>,
}

/// A provider family and its bound route, credential, and transport state.
#[must_use]
pub enum Provider {
    /// OpenAI Chat Completions.
    Chat(Http),
    /// OpenAI Responses, over HTTPS or its configured WebSocket transport.
    Responses(Http),
    /// OpenAI Codex, over HTTPS or its pooled WebSocket transport.
    Codex(Http),
    /// Anthropic Messages.
    Anthropic(Http),
    /// Deterministic provider steps for tests and headless replay.
    Scripted(crate::scripted::Script),
}
impl std::fmt::Debug for Http {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Http")
            .field("family", &self.entry.family)
            .field("provider", &self.entry.id)
            .field("model", &self.resolved.entry.id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chat(http) => formatter.debug_tuple("Chat").field(http).finish(),
            Self::Responses(http) => formatter.debug_tuple("Responses").field(http).finish(),
            Self::Codex(http) => formatter.debug_tuple("Codex").field(http).finish(),
            Self::Anthropic(http) => formatter.debug_tuple("Anthropic").field(http).finish(),
            Self::Scripted(_) => formatter.write_str("Scripted"),
        }
    }
}


impl Provider {
    /// Binds a resolved route, its matching configured provider, and a
    /// credential to the host's shared client and state.
    ///
    /// # Errors
    /// Returns `InvalidRequest` when the route, provider entry, or transport do
    /// not agree, and `NoCredentials` when an HTTP route has no credential.
    pub fn new(
        resolved: ResolvedModel,
        entry: ProviderEntry,
        credential: Credential,
        client: &reqwest::Client,
        owner: &ProviderSet,
    ) -> Result<Self, ProviderError> {
        let ModelRoute::Api { family, model } = &resolved.route else {
            return Err(ProviderError::InvalidRequest {
                message: String::from("an HTTP provider needs a concrete API route"),
            });
        };
        let slot = owner
            .inner
            .providers
            .get(&entry.id)
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: format!("provider {} is not owned by this provider set", entry.id),
            })?;
        if slot.entry.family != entry.family
            || slot.entry.base_url != entry.base_url
            || slot.entry.transport != entry.transport
            || slot.entry.key_env != entry.key_env
            || slot.entry.auth != entry.auth
            || slot.entry.max_concurrent_requests != entry.max_concurrent_requests
        {
            return Err(ProviderError::InvalidRequest {
                message: format!("provider {} does not match this provider set", entry.id),
            });
        }
        if resolved.provider != entry.id
            || resolved.entry.provider != resolved.provider
            || resolved.entry.id != *model
            || *family != entry.family
        {
            return Err(ProviderError::InvalidRequest {
                message: String::from("resolved model does not match its configured provider"),
            });
        }
        if entry.transport == Transport::Websocket
            && !matches!(*family, Family::Responses | Family::Codex)
        {
            return Err(ProviderError::InvalidRequest {
                message: String::from("WebSocket transport is supported only by Responses and Codex"),
            });
        }
        if matches!(&credential, Credential::None) {
            return Err(ProviderError::NoCredentials {
                provider: entry.id.to_string(),
            });
        }
        if matches!(*family, Family::Chat | Family::Responses)
            && matches!(&credential, Credential::OAuth(_))
        {
            return Err(ProviderError::InvalidRequest {
                message: String::from("this provider family requires an API-key credential"),
            });
        }
        let provider = Self::from_family(
            *family,
            Http {
                resolved,
                entry,
                credential,
                client: client.clone(),
                user_agent: owner.inner.user_agent.clone(),
                owner: Arc::downgrade(&owner.inner),
            },
        );
        Ok(provider)
    }

    fn from_family(family: Family, http: Http) -> Self {
        match family {
            Family::Chat => Self::Chat(http),
            Family::Responses => Self::Responses(http),
            Family::Codex => Self::Codex(http),
            Family::Anthropic => Self::Anthropic(http),
        }
    }

    fn http(&self) -> Option<&Http> {
        match self {
            Self::Chat(http) | Self::Responses(http) | Self::Codex(http) | Self::Anthropic(http) => Some(http),
            Self::Scripted(_) => None,
        }
    }

    /// Opens one provider stream for `session`.
    ///
    /// The session id is explicit and never inferred from the optional prompt
    /// cache key. Replay data is bound through each assistant context item's
    /// `ReplaySource`. Hooks run in registration order and cancellation is
    /// observed while hooks execute and throughout transport admission.
    ///
    /// # Errors
    /// Returns an unresolved-blob, tool-support, route, credential, hook, or
    /// family-body error before a stream is available.
    pub async fn open(
        &self,
        session: SessionId,
        request: &ModelRequest,
        hooks: &[Arc<dyn BeforeRequest>],
        notices: NoticeSink,
        cancel: &CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        if let Self::Scripted(script) = self {
            return script.open();
        }
        let http = self.http().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider is not HTTP-backed"),
        })?;
        ensure_request(http, request)?;
        if http.entry.family == Family::Codex
            && !matches!(&http.credential, Credential::OAuth(_))
        {
            return Err(ProviderError::InvalidRequest {
                message: String::from("openai-codex needs an OAuth credential"),
            });
        }
        if let Some(blob_id) = unresolved_blob(request) {
            return Err(ProviderError::UnresolvedBlob { blob_id });
        }
        let owner = http.owner.upgrade().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider host has ended"),
        })?;
        let patch = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Transport {
                family: http.entry.family,
                reason: String::from("provider request cancelled before it started"),
            }),
            result = thinking::compose_hooks(
                hooks,
                request_model(request),
                request.params.thinking,
            ) => result?,
        };
        let patch = merge_request_parameters(request, patch)?;
        let thinking = make_thinking_plan(http, request, &patch)?;
        let family = request_family(request)?;
        if !request.tools.is_empty()
            && !http.resolved.entry.tool_support.allows(family, thinking.level)
        {
            return Err(ProviderError::InvalidRequest {
                message: format!(
                    "{family} does not support tools for thinking level {}",
                    thinking::level_name(thinking.level)
                ),
            });
        }
        emit_thinking_notices(
            &owner,
            session,
            request_model(request),
            &thinking.notices,
            &notices,
        );
        let credential = refresh_expiring(
            &owner.refresher,
            &http.entry.id,
            http.entry.family,
            &http.credential,
            cancel,
        )
        .await?;
        if http.entry.transport == Transport::Websocket {
            return self
                .open_websocket(
                    http,
                    &owner,
                    session,
                    request,
                    thinking,
                    credential,
                    notices,
                    cancel,
                )
                .await;
        }
        self.open_http(http, &owner, session, request, thinking, credential, notices, cancel)
    }

    async fn open_websocket(
        &self,
        http: &Http,
        owner: &Arc<set::ProviderSetInner>,
        session: SessionId,
        request: &ModelRequest,
        thinking: ThinkingPlan,
        credential: Credential,
        notices: NoticeSink,
        cancel: &CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let permit = acquire_provider_permit(owner, &http.entry.id, cancel).await?;
        let retries = owner.config.stream_max_retries;
        let context = stream_context(http, request, thinking.clone(), session);
        let ws_turn = match request_family(request)? {
            Family::Codex => {
                let Credential::OAuth(oauth) = &credential else {
                    return Err(ProviderError::InvalidRequest {
                        message: String::from("openai-codex needs an OAuth credential"),
                    }
                    .into());
                };
                let wire = build_codex_wire(&context, request, &thinking, oauth, session)?;
                owner
                    .ws
                    .open(crate::ws::WsRequest {
                        provider: &http.entry.id,
                        session_id: session,
                        base_url: &http.entry.base_url,
                        wire: crate::ws::WsWire::Codex(&wire),
                        stream_max_retries: retries,
                        refreshed: false,
                        notices: &notices,
                        cancel,
                    })
                    .await?
            }
            Family::Responses => {
                let wire = build_responses_wire_from_context(&context, &credential)?;
                owner
                    .ws
                    .open(crate::ws::WsRequest {
                        provider: &http.entry.id,
                        session_id: session,
                        base_url: &http.entry.base_url,
                        wire: crate::ws::WsWire::Responses(&wire),
                        stream_max_retries: retries,
                        refreshed: false,
                        notices: &notices,
                        cancel,
                    })
                    .await?
            }
            Family::Chat | Family::Anthropic => {
                return Err(ProviderError::InvalidRequest {
                    message: String::from("this provider family has no WebSocket transport"),
                }
                .into());
            }
        };
        match ws_turn {
            crate::ws::WsTurn::Stream(stream) => Ok(hold_permit(stream, permit)),
            crate::ws::WsTurn::HttpsFallback => {
                drop(permit);
                self.open_http(http, owner, session, request, thinking, credential, notices, cancel)
            }
        }
    }

    fn open_http(
        &self,
        http: &Http,
        owner: &Arc<set::ProviderSetInner>,
        session: SessionId,
        request: &ModelRequest,
        thinking: ThinkingPlan,
        credential: Credential,
        notices: NoticeSink,
        cancel: &CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let family = request_family(request)?;
        let permits = owner
            .providers
            .get(&http.entry.id)
            .map(|slot| Arc::clone(&slot.permits))
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: format!("provider {} has no admission semaphore", http.entry.id),
            })?;
        let plan = Plan::new(
            family,
            http.entry.id.clone(),
            http.resolved.entry.id.clone(),
            owner.config.stream_max_retries,
            permits,
            cancel.clone(),
            notices,
        );
        let ctx = stream_context(http, request, thinking, session);
        let ctx_for_attempt = ctx.clone();
        let attempt = move |credential: &Credential, attempt_cancel: &CancellationToken| {
            let ctx = ctx_for_attempt.clone();
            let credential = credential.clone();
            let attempt_cancel = attempt_cancel.clone();
            async move { stream_attempt(ctx, credential, attempt_cancel).await }
        };
        let refresher = Arc::clone(&owner.refresher);
        let provider_id = http.entry.id.clone();
        let refresh = move |held: OAuthCredential| {
            refresh_credential(Arc::clone(&refresher), provider_id.clone(), held)
        };
        Ok(lifecycle::stream(plan, credential, attempt, refresh))
    }

    async fn fetch_catalog(&self) -> Result<CatalogFetch, ProviderError> {
        let http = self.http().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("the scripted provider has no model catalog"),
        })?;
        let owner = http.owner.upgrade().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider host has ended"),
        })?;
        let slot = owner
            .providers
            .get(&http.entry.id)
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: format!("provider {} is not configured", http.entry.id),
            })?;
        let permit = Arc::clone(&slot.permits)
            .acquire_owned()
            .await
            .map_err(|_| ProviderError::Transport {
                family: http.entry.family,
                reason: String::from("provider model-list admission is closed"),
            })?;
        let refresh_cancel = CancellationToken::new();
        let credential = refresh_expiring(
            &owner.refresher,
            &http.entry.id,
            http.entry.family,
            &http.credential,
            &refresh_cancel,
        )
        .await?;
        let fetch = ModelFetch {
            client: &http.client,
            provider: &http.entry,
            credential: &credential,
            cache_dir: &owner.cache_dir,
            user_agent: &http.user_agent,
            version: &owner.identity.version,
        };
        let catalog = catalog::load_models(&fetch, tokio::time::sleep).await;
        drop(permit);
        Ok(catalog)
    }

    /// Returns a display projection of this provider's model catalog.
    ///
    /// `ProviderSet::catalog` is the source-aware routing view; this projection is
    /// intentionally per-provider display metadata, not a route key.
    pub async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let http = self.http().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("the scripted provider has no model catalog"),
        })?;
        let catalog = self.fetch_catalog().await?;
        let rows = if catalog.entries.is_empty() {
            vec![http.resolved.entry.clone()]
        } else {
            catalog.entries
        };
        let owner = http.owner.upgrade().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider host has ended"),
        })?;
        Ok(rows
            .into_iter()
            .map(|entry| model_info(entry, http.entry.family, owner.config.thinking))
            .collect())
    }

    /// Reads Codex account usage through its shared single-flight checker.
    /// The host owns each spawned task through the supplied callback.
    pub async fn usage<F>(
        &self,
        spawn: &F,
    ) -> Result<UsageVerdict, UsageCheckReason>
    where
        F: Fn(Pin<Box<dyn Future<Output = ()> + Send + 'static>>),
    {
        let Some(http) = self.http() else {
            return Err(UsageCheckReason::Transport {
                reason: String::from("Scripted usage is a token frame, not a Codex account check"),
            });
        };
        if http.entry.family != Family::Codex {
            return Err(UsageCheckReason::Transport {
                reason: String::from("account usage checks are available only for Codex"),
            });
        }
        let owner = http.owner.upgrade().ok_or_else(|| UsageCheckReason::Transport {
            reason: String::from("provider host has ended"),
        })?;
        let checker = owner.usage.get(&http.entry.id).ok_or_else(|| UsageCheckReason::Transport {
            reason: String::from("Codex usage checker is unavailable"),
        })?;
        let slot = owner.providers.get(&http.entry.id).ok_or_else(|| UsageCheckReason::Transport {
            reason: String::from("Codex provider admission is unavailable"),
        })?;
        let permits = Arc::clone(&slot.permits);
        let spawn_task = |task: crate::usage::UsageTask| {
            let permits = Arc::clone(&permits);
            let task: crate::usage::UsageTask = Box::pin(async move {
                if let Ok(_permit) = permits.acquire_owned().await {
                    task.await;
                }
            });
            spawn(task);
        };
        checker.check(&http.credential, &spawn_task).await
    }

    /// Consumes a scripted token-usage step. HTTP provider account checks are
    /// exposed by [`Provider::usage`].
    pub fn scripted_usage(&self) -> Result<Usage, ProviderError> {
        match self {
            Self::Scripted(script) => script.usage(),
            _ => Err(ProviderError::InvalidRequest {
                message: String::from("token usage frames are available only on Scripted"),
            }),
        }
    }

    /// Runs the family's pure remote-compaction adapter under the shared
    /// request lifecycle. Cancellation returns `None` without a partial
    /// history.
    pub async fn compact(
        &self,
        session: SessionId,
        request: &ModelRequest,
        notices: NoticeSink,
        cancel: &CancellationToken,
    ) -> Result<Option<CompactOutcome>, ProviderError> {
        if let Self::Scripted(script) = self {
            return script.compact().map(Some);
        }
        let http = self.http().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider is not HTTP-backed"),
        })?;
        ensure_request(http, request)?;
        if request.purpose != Purpose::Compact {
            return Err(ProviderError::InvalidRequest {
                message: String::from("remote compaction needs a compact-purpose request"),
            });
        }
        if let Some(blob_id) = unresolved_blob(request) {
            return Err(ProviderError::UnresolvedBlob { blob_id });
        }
        if http.entry.family == Family::Chat {
            return Ok(Some(compact::chat()));
        }
        if !http.resolved.entry.remote_compact {
            return Ok(Some(CompactOutcome::Unsupported));
        }
        if http.entry.family == Family::Codex
            && !matches!(&http.credential, Credential::OAuth(_))
        {
            return Err(ProviderError::InvalidRequest {
                message: String::from("openai-codex needs an OAuth credential"),
            });
        }
        let owner = http.owner.upgrade().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider host has ended"),
        })?;
        let patch = merge_request_parameters(request, BeforeRequestPatch::default())?;
        let thinking = make_thinking_plan(http, request, &patch)?;
        emit_thinking_notices(
            &owner,
            session,
            request_model(request),
            &thinking.notices,
            &notices,
        );
        let credential = refresh_expiring(
            &owner.refresher,
            &http.entry.id,
            http.entry.family,
            &http.credential,
            cancel,
        )
        .await?;
        let max_retries = if http.entry.family == Family::Codex {
            compact::CODEX_STREAM_RETRIES
        } else {
            owner.config.request_max_retries
        };
        let permits = owner
            .providers
            .get(&http.entry.id)
            .map(|slot| Arc::clone(&slot.permits))
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: format!("provider {} has no admission semaphore", http.entry.id),
            })?;
        let plan = Plan::new(
            http.entry.family,
            http.entry.id.clone(),
            http.resolved.entry.id.clone(),
            max_retries,
            permits,
            cancel.clone(),
            notices,
        );
        let ctx = CompactContext {
            family: http.entry.family,
            provider: http.entry.clone(),
            resolved: http.resolved.clone(),
            request: request.clone(),
            thinking,
            session,
            client: http.client.clone(),
            user_agent: http.user_agent.clone(),
        };
        let ctx_for_attempt = ctx.clone();
        let attempt = move |credential: &Credential, attempt_cancel: &CancellationToken| {
            let ctx = ctx_for_attempt.clone();
            let credential = credential.clone();
            let attempt_cancel = attempt_cancel.clone();
            async move { compact_attempt(ctx, credential, attempt_cancel).await }
        };
        let refresher = Arc::clone(&owner.refresher);
        let provider_id = http.entry.id.clone();
        let refresh = move |held: OAuthCredential| {
            refresh_credential(Arc::clone(&refresher), provider_id.clone(), held)
        };
        lifecycle::request(plan, credential, attempt, refresh)
            .await
            .map(|outcome| outcome.map(CompactOutcome::Compacted))
    }
}

impl Http {
    fn model(&self) -> &str {
        &self.resolved.entry.id
    }
}

#[derive(Clone)]
struct StreamContext {
    family: Family,
    provider: Box<str>,
    base_url: Box<str>,
    auth: AuthStyle,
    model: Box<str>,
    entry: CatalogEntry,
    request: ModelRequest,
    thinking: ThinkingPlan,
    session: SessionId,
    client: reqwest::Client,
    user_agent: Box<str>,
}

fn stream_context(
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
struct CompactContext {
    family: Family,
    provider: ProviderEntry,
    resolved: ResolvedModel,
    request: ModelRequest,
    thinking: ThinkingPlan,
    session: SessionId,
    client: reqwest::Client,
    user_agent: Box<str>,
}

async fn stream_attempt(
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

    let (request, user_agent, secrets) =
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
        return match status_failure(response, context.family, &credential, &secrets, &cancel).await? {
            Some(failure) => Err(failure),
            None => Ok(None),
        };
    }
    Ok(Some(decode_response(
        response,
        context.family,
        &context.provider,
        &context.model,
        matches!(&credential, Credential::OAuth(_)),
        secrets,
    )))
}

fn build_stream_request(
    context: &StreamContext,
    credential: &Credential,
) -> Result<(reqwest::RequestBuilder, Box<str>, Vec<Box<str>>), ProviderError> {
    let family = context.family;
    let user_agent = context.user_agent.clone();
    let secrets = credential_secrets(credential);
    match family {
        Family::Chat => {
            let body = family::chat::request_body(&context.request, &context.thinking)
                .map_err(|error| ProviderError::InvalidRequest {
                    message: error.to_string(),
                })?;
            let url = http::endpoint(family, &context.base_url, family::chat::ENDPOINT_PATH)?;
            let (header, value) = auth_header(context.auth, credential, family, &context.provider)?;
            Ok((
                context.client.post(url).header(header, value).body(body),
                user_agent,
                secrets,
            ))
        }
        Family::Responses => {
            let wire = build_responses_wire_from_context(context, credential)?;
            let url = http::endpoint(family, &context.base_url, "responses")?;
            let mut request = context.client.post(url);
            if let Some((name, value)) = &wire.auth_header {
                request = request.header(*name, value);
            }
            Ok((request.body(wire.body), wire.user_agent.into(), secrets))
        }
        Family::Codex => Err(ProviderError::InvalidRequest {
            message: String::from("Codex requests use the dedicated HTTPS adapter"),
        }),
        Family::Anthropic => {
            let wire = build_anthropic_wire(
                &context.request,
                &context.entry,
                context.auth,
                &context.thinking,
                credential,
                false,
            )?;
            let url = http::endpoint(family, &context.base_url, wire.path)?;
            let user_agent = wire
                .user_agent
                .as_deref()
                .unwrap_or(context.user_agent.as_ref())
                .to_owned()
                .into_boxed_str();
            Ok((wire.into_request(&context.client, url), user_agent, secrets))
        }
    }
}

fn build_responses_wire_from_context(
    context: &StreamContext,
    credential: &Credential,
) -> Result<family::responses::ResponsesWire, ProviderError> {
    family::responses::wire(ResponsesRequest {
        request: &context.request,
        thinking: context.thinking.wire,
        reasoning_summary: context.entry.supports_reasoning_summaries,
        auth: context.auth,
        credential,
        session_id: context.session,
        user_agent: &context.user_agent,
    })
}

fn build_codex_wire(
    context: &StreamContext,
    request: &ModelRequest,
    thinking: &ThinkingPlan,
    credential: &OAuthCredential,
    session: SessionId,
) -> Result<family::codex::CodexWire, ProviderError> {
    family::codex::build(CodexRequest {
        request,
        thinking: thinking.wire,
        reasoning_summaries: context.entry.supports_reasoning_summaries,
        credential,
        session_id: session,
        user_agent: &context.user_agent,
    })
}

fn build_anthropic_wire(
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

async fn status_failure(
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
    let mut body = String::from_utf8_lossy(&body).into_owned();
    for secret in credential_secrets(credential).iter().chain(extra_secrets) {
        if !secret.is_empty() && body.contains(secret.as_ref()) {
            body = body.replace(secret.as_ref(), "<redacted>");
        }
    }
    let (code, message) = error_fields(&body);
    Ok(Some(AttemptFailure::Response {
        status,
        code,
        message,
        retry_after,
    }))
}

#[derive(Default, Deserialize)]
struct ErrorBody {
    error: Option<ErrorDetail>,
    message: Option<String>,
    detail: Option<String>,
    code: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Default, Deserialize)]
struct ErrorDetail {
    code: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

fn error_fields(body: &str) -> (Option<String>, String) {
    let parsed = sonic_rs::from_str::<ErrorBody>(body).ok();
    let code = parsed.as_ref().and_then(|body| {
        body.error
            .as_ref()
            .and_then(|error| error.code.clone().or_else(|| error.kind.clone()))
            .or_else(|| body.code.clone().or_else(|| body.kind.clone()))
    });
    let message = parsed
        .and_then(|body| {
            body.error
                .and_then(|error| error.message)
                .or(body.message)
                .or(body.detail)
        })
        .unwrap_or_else(|| body.lines().next().unwrap_or_default().to_owned());
    (code, first_line(&message, 300))
}

fn first_line(input: &str, max_bytes: usize) -> String {
    let line = input.lines().next().unwrap_or_default();
    let mut end = line.len().min(max_bytes);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

fn decode_response(
    response: reqwest::Response,
    family: Family,
    provider: &str,
    model: &str,
    oauth: bool,
    secrets: Vec<Box<str>>,
) -> EventStream {
    let chunks = response.bytes_stream().map(|chunk| match chunk {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => Vec::new(),
    });
    let events = crate::sse::decode_stream(chunks);
    let decoded: Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>> =
        match family {
            Family::Chat => Box::pin(family::chat::decode_events(
                events,
                family::chat::ChatDecoder::new(provider, model),
            )),
            Family::Responses | Family::Codex => {
                Box::pin(family::responses::decode(events, family, model))
            }
            Family::Anthropic => {
                Box::pin(family::anthropic::decode_stream(events, model.into(), oauth))
            }
        };
    let safe_errors = decoded.map(move |result| {
        result.map_err(|error| redact_provider_error(error, &secrets))
    });
    EventStream::new(safe_errors, || {})
}

fn redact_provider_error(
    error: ProviderError,
    secrets: &[Box<str>],
) -> ProviderError {
    let redact = |mut text: String| {
        for secret in secrets {
            if !secret.is_empty() && text.contains(secret.as_ref()) {
                text = text.replace(secret.as_ref(), "<redacted>");
            }
        }
        text
    };
    match error {
        ProviderError::Transport { family, reason } => ProviderError::Transport {
            family,
            reason: redact(reason),
        },
        ProviderError::Status {
            family,
            status,
            message,
        } => ProviderError::Status {
            family,
            status,
            message: redact(message),
        },
        ProviderError::InvalidRequest { message } => {
            ProviderError::InvalidRequest { message: redact(message) }
        }
        ProviderError::ContextOverflow {
            family,
            code,
            message,
        } => ProviderError::ContextOverflow {
            family,
            code: redact(code),
            message: redact(message),
        },
        ProviderError::RateLimited {
            message,
            retry_after,
        } => ProviderError::RateLimited {
            message: redact(message),
            retry_after,
        },
        ProviderError::RetryAfterTooLong {
            seconds,
            message,
        } => ProviderError::RetryAfterTooLong {
            seconds,
            message: redact(message),
        },
        ProviderError::Quota { message } => ProviderError::Quota {
            message: redact(message),
        },
        ProviderError::WsClosed { code } => ProviderError::WsClosed {
            code: code.map(|(code, reason)| (code, redact(reason))),
        },
        ProviderError::Protocol { family, detail } => ProviderError::Protocol {
            family,
            detail: redact(detail),
        },
        ProviderError::UsageLimit { model, message } => ProviderError::UsageLimit {
            model: redact(model),
            message: redact(message),
        },
        ProviderError::UsageNotIncluded { message } => {
            ProviderError::UsageNotIncluded { message: redact(message) }
        }
        ProviderError::ReserveUnavailable { status, message } => {
            ProviderError::ReserveUnavailable {
                status,
                message: redact(message),
            }
        }
        ProviderError::AuthFileInvalid { path, message } => {
            ProviderError::AuthFileInvalid {
                path,
                message: redact(message),
            }
        }
        ProviderError::AuthWrite { reason } => ProviderError::AuthWrite {
            reason: redact(reason),
        },
        ProviderError::CallbackBind { port, reason } => ProviderError::CallbackBind {
            port,
            reason: redact(reason),
        },
        ProviderError::TokenExchange { status, message } => {
            ProviderError::TokenExchange {
                status,
                message: redact(message),
            }
        }
        ProviderError::DeviceCode { status, message } => ProviderError::DeviceCode {
            status,
            message: redact(message),
        },
        ProviderError::UsageCheck { reason } => ProviderError::UsageCheck {
            reason: match reason {
                UsageCheckReason::Status { status, message } => UsageCheckReason::Status {
                    status,
                    message: redact(message),
                },
                UsageCheckReason::Transport { reason } => UsageCheckReason::Transport {
                    reason: redact(reason),
                },
                other => other,
            },
        },
        other => other,
    }
}
async fn compact_attempt(
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
            let wire = build_codex_wire(&stream_context, &context.request, &context.thinking, oauth, context.session)
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

fn compact_stream_context(context: &CompactContext) -> StreamContext {
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

fn acquire_provider_permit<'a>(
    owner: &'a set::ProviderSetInner,
    provider: &str,
    cancel: &'a CancellationToken,
) -> impl Future<Output = Result<OwnedSemaphorePermit, ProviderError>> + 'a {
    async move {
        let slot = owner
            .providers
            .get(provider)
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: format!("provider {provider} is not configured"),
            })?;
        let family = slot.entry.family;
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ProviderError::Transport {
                family,
                reason: String::from("provider request cancelled"),
            }),
            permit = Arc::clone(&slot.permits).acquire_owned() => permit
                .map_err(|_| ProviderError::Transport {
                    family,
                    reason: String::from("provider request admission is closed"),
                }),
        }
    }
}

fn hold_permit(stream: EventStream, permit: OwnedSemaphorePermit) -> EventStream {
    let source = futures::stream::unfold((stream, Some(permit)), |(mut stream, mut permit)| async move {
        match stream.next().await {
            Some(Ok(event @ StreamEvent::Stop { .. })) => {
                drop(permit.take());
                Some((Ok(event), (stream, permit)))
            }
            Some(item) => Some((item, (stream, permit))),
            None => None,
        }
    });
    EventStream::new(source, || {})
}

async fn refresh_expiring(
    refresher: &Refresher,
    provider: &str,
    family: Family,
    credential: &Credential,
    cancel: &CancellationToken,
) -> Result<Credential, ProviderError> {
    let Credential::OAuth(held) = credential else {
        return Ok(credential.clone());
    };
    let Some(provider_kind) = OAuthProvider::from_id(provider) else {
        return Ok(credential.clone());
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProviderError::Transport {
            family,
            reason: String::from("provider request cancelled while refreshing credentials"),
        }),
        credential = refresher.refresh(provider_kind, held, RefreshReason::Expiring) => credential,
    }
}
fn refresh_credential(
    refresher: Arc<Refresher>,
    provider: Box<str>,
    held: OAuthCredential,
) -> impl Future<Output = Result<Credential, ProviderError>> + Send + 'static {
    async move {
        let Some(provider_kind) = OAuthProvider::from_id(&provider) else {
            return Err(ProviderError::SignInExpired {
                provider: provider.to_string(),
            });
        };
        refresher.refresh(provider_kind, &held, RefreshReason::Rejected).await
    }
}

fn ensure_request(http: &Http, request: &ModelRequest) -> Result<(), ProviderError> {
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

fn request_family(request: &ModelRequest) -> Result<Family, ProviderError> {
    match &request.model {
        ModelRoute::Api { family, .. } => Ok(*family),
        _ => Err(ProviderError::InvalidRequest {
            message: String::from("an HTTP provider needs a concrete API model route"),
        }),
    }
}

fn request_model(request: &ModelRequest) -> &str {
    match &request.model {
        ModelRoute::Api { model, .. } => model,
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id,
    }
}

fn unresolved_blob(request: &ModelRequest) -> Option<dal_core::BlobId> {
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

fn merge_request_parameters(
    request: &ModelRequest,
    mut patch: BeforeRequestPatch,
) -> Result<BeforeRequestPatch, ProviderError> {
    if patch.effort.is_none() {
        patch.effort = request.params.effort.as_deref().map(parse_effort).transpose()?;
    }
    if patch.temperature.is_none() {
        patch.temperature = request.params.temperature.map(parse_temperature).transpose()?;
    }
    Ok(patch)
}

fn parse_effort(value: &str) -> Result<Effort, ProviderError> {
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

fn parse_temperature(value: f64) -> Result<f32, ProviderError> {
    if !value.is_finite() {
        return Err(ProviderError::InvalidRequest {
            message: String::from("temperature must be finite"),
        });
    }
    value.to_string().parse::<f32>().map_err(|_| ProviderError::InvalidRequest {
        message: String::from("temperature is outside the supported range"),
    })
}

fn make_thinking_plan(
    http: &Http,
    request: &ModelRequest,
    patch: &BeforeRequestPatch,
) -> Result<ThinkingPlan, ProviderError> {
    let family = request_family(request)?;
    let max_output = http.resolved.entry.max_output.unwrap_or_else(|| {
        if family == Family::Anthropic { family::anthropic::MAX_TOKENS_CAP } else { 0 }
    });
    let max_tokens = if family == Family::Anthropic {
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

fn emit_thinking_notices(
    owner: &set::ProviderSetInner,
    session: SessionId,
    model: &str,
    notices: &[ThinkingNotice],
    sink: &NoticeSink,
) {
    for notice in notices.iter().copied() {
        let first = set::lock(&owner.notices)
            .entry(session)
            .or_insert_with(SessionNotices::default)
            .first(model, notice);
        if first {
            sink(notice.render(model));
        }
    }
}

fn auth_header(
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
        Credential::OAuth(oauth) if matches!(family, Family::Codex | Family::Anthropic) => {
            Ok(("authorization", format!("Bearer {}", oauth.access_token.expose())))
        }
        Credential::OAuth(_) => Err(ProviderError::InvalidRequest {
            message: format!("{provider} does not accept an OAuth credential"),
        }),
        Credential::None => Err(ProviderError::NoCredentials {
            provider: provider.to_owned(),
        }),
    }
}

fn credential_secrets(credential: &Credential) -> Vec<Box<str>> {
    match credential {
        Credential::ApiKey { key } => vec![key.expose().into()],
        Credential::OAuth(oauth) => {
            let mut values = vec![
                oauth.access_token.expose().into(),
                oauth.refresh_token.expose().into(),
            ];
            if let Some(id_token) = &oauth.id_token {
                values.push(id_token.clone().into_boxed_str());
            }
            values
        }
        Credential::None => Vec::new(),
    }
}

fn model_info(entry: CatalogEntry, family: Family, default_level: ThinkingLevel) -> ModelInfo {
    let (effective, _) = thinking::clamp(default_level, &entry.thinking);
    ModelInfo {
        route: ModelRoute::Api {
            family,
            model: entry.id.clone(),
        },
        name: format!("{} · {}/{}", entry.display, entry.provider, entry.id).into_boxed_str(),
        caps: Caps {
            context_window: entry.context_window,
            thinking: supported_levels(&entry.thinking),
            tool_use: entry.tool_support.allows(family, effective),
            image_input: entry.image_input,
        },
    }
}

fn supported_levels(support: &ThinkingSupport) -> Box<[ThinkingLevel]> {
    use ThinkingLevel::{High, Low, Max, Medium, Minimal, Off, Xhigh};
    let levels = [Off, Minimal, Low, Medium, High, Xhigh, Max];
    levels
        .into_iter()
        .filter(|level| match support {
            ThinkingSupport::OpenAi { accepted, none_supported } => {
                accepted.contains(level) || (*level == Off && *none_supported)
            }
            ThinkingSupport::Adaptive { accepted, can_disable } => {
                if *level == Off { *can_disable }
                else {
                    let effort = match level {
                        Minimal | Low => Effort::Low,
                        Medium => Effort::Medium,
                        High => Effort::High,
                        Xhigh => Effort::Xhigh,
                        Max => Effort::Max,
                        Off => return *can_disable,
                    };
                    accepted.contains(&effort)
                }
            }
            ThinkingSupport::Budget { can_disable } => *level != Off || *can_disable,
            ThinkingSupport::UnknownAdaptive => true,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        io,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };

    use dal_core::{
        ContextItem, ModelToolSpec, Part, Purpose, RawJson, RequestParams, ThinkingLevel,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
        sync::{mpsc, oneshot},
    };

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> io::Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "dalgona-provider-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Reply {
        status: u16,
        content_type: &'static str,
        body: String,
    }

    async fn server(
        path_prefix: &str,
        replies: Vec<Reply>,
    ) -> io::Result<(String, JoinHandle<io::Result<Vec<Vec<u8>>>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let url = format!("http://{address}{path_prefix}");
        let task = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let (mut socket, _) = listener.accept().await?;
                requests.push(read_request(&mut socket).await?);
                write_reply(&mut socket, reply).await?;
            }
            Ok(requests)
        });
        Ok((url, task))
    }

    async fn read_request(socket: &mut TcpStream) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 2048];
        loop {
            if let Some(separator) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let header_end = separator + 4;
                let headers = String::from_utf8_lossy(&bytes[..separator]);
                let body_len = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if bytes.len() >= header_end + body_len {
                    return Ok(bytes);
                }
            }
            let count = socket.read(&mut chunk).await?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "provider closed before sending its request body",
                ));
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
    }

    async fn write_reply(socket: &mut TcpStream, reply: Reply) -> io::Result<()> {
        let reason = match reply.status {
            200 => "OK",
            401 => "Unauthorized",
            500 => "Internal Server Error",
            _ => "Test",
        };
        let headers = format!(
            "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            reply.status,
            reply.content_type,
            reply.body.len()
        );
        socket.write_all(headers.as_bytes()).await?;
        socket.write_all(reply.body.as_bytes()).await?;
        socket.flush().await
    }

    fn entry(
        id: &str,
        family: Family,
        base_url: String,
        key_env: Option<&str>,
        max_concurrent_requests: u32,
        transport: Transport,
    ) -> ProviderEntry {
        ProviderEntry {
            id: id.into(),
            family,
            base_url: base_url.into(),
            transport,
            key_env: key_env.map(Into::into),
            auth: AuthStyle::Bearer,
            max_concurrent_requests,
        }
    }

    fn config(providers: Vec<ProviderEntry>, stream_max_retries: u32) -> ProviderConfig {
        ProviderConfig {
            default_model: None,
            thinking: ThinkingLevel::Medium,
            aliases: Vec::new(),
            request_max_retries: 0,
            stream_max_retries,
            providers,
        }
    }

    fn identity() -> ProviderIdentity {
        ProviderIdentity {
            version: "test".into(),
            os: "linux".into(),
            os_version: "test".into(),
            arch: "x86_64".into(),
        }
    }

    fn resolved(provider: &str, family: Family, model: &str) -> ResolvedModel {
        let entry = CatalogEntry {
            provider: provider.into(),
            id: model.into(),
            display: model.into(),
            listing: crate::catalog::Listing::Listed,
            context_window: Some(32_000),
            max_output: Some(4_096),
            thinking: ThinkingSupport::OpenAi {
                accepted: vec![ThinkingLevel::Low, ThinkingLevel::Medium],
                none_supported: true,
            },
            image_input: true,
            remote_compact: true,
            supports_reasoning_summaries: false,
            tool_support: crate::catalog::ToolSupport::Any,
            temperature_allowed: true,
            display_supported: false,
        };
        ResolvedModel {
            provider: provider.into(),
            route: ModelRoute::Api {
                family,
                model: model.into(),
            },
            entry,
        }
    }

    fn request(family: Family, model: &str, context: Vec<ContextItem>) -> ModelRequest {
        ModelRequest {
            purpose: Purpose::Turn,
            model: ModelRoute::Api {
                family,
                model: model.into(),
            },
            system: Arc::from("Follow the request."),
            tools: Arc::<[ModelToolSpec]>::from(Vec::new()),
            context: Arc::<[ContextItem]>::from(context),
            params: RequestParams {
                thinking: ThinkingLevel::Off,
                effort: None,
                temperature: None,
            },
            cache_key: Some("session-cache:1".into()),
        }
    }

    fn user_text(text: &str) -> ContextItem {
        ContextItem::User {
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    fn notice_sink() -> NoticeSink {
        Arc::new(|_| {})
    }

    async fn collect(mut stream: EventStream) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event?);
        }
        Ok(events)
    }

    fn chat_reply() -> String {
        String::from(
            r#"data: {"choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]

"#,
        )
    }

    fn responses_reply() -> String {
        String::from(
            r#"data: {"type":"response.output_text.delta","delta":"Hello"}

data: {"type":"response.completed","response":{"output":[],"usage":null}}

"#,
        )
    }

    fn stream_reply(body: String) -> Reply {
        Reply {
            status: 200,
            content_type: "text/event-stream",
            body,
        }
    }

    #[tokio::test]
    async fn chat_and_responses_complete_through_the_real_http_lifecycle() -> TestResult {
        let dir = TestDir::new()?;
        let (chat_base, chat_server) =
            server("/v1", vec![stream_reply(chat_reply())]).await?;
        let (responses_base, responses_server) =
            server("/v1", vec![stream_reply(responses_reply())]).await?;
        let chat_entry = entry(
            "chat-test",
            Family::Chat,
            chat_base,
            Some("CHAT_TEST_KEY"),
            2,
            Transport::Https,
        );
        let responses_entry = entry(
            "responses-test",
            Family::Responses,
            responses_base,
            Some("RESPONSES_TEST_KEY"),
            2,
            Transport::Https,
        );
        let set = ProviderSet::new(
            &config(vec![chat_entry, responses_entry], 0),
            identity(),
            EnvSnapshot::test(&[
                ("CHAT_TEST_KEY", "chat-secret"),
                ("RESPONSES_TEST_KEY", "responses-secret"),
            ]),
            dir.path(),
            dir.path(),
        )?;

        for (id, family, model, expected_path, expected_key, result) in [
            (
                "chat-test",
                Family::Chat,
                "gpt-chat-test",
                "/v1/chat/completions",
                "chat-secret",
                chat_server,
            ),
            (
                "responses-test",
                Family::Responses,
                "gpt-responses-test",
                "/v1/responses",
                "responses-secret",
                responses_server,
            ),
        ] {
            let provider = set.provider(resolved(id, family, model))?;
            let request = request(family, model, vec![user_text("Read the file.")]);
            let events = collect(
                provider
                    .open(
                        SessionId::new_v7(),
                        &request,
                        &[],
                        notice_sink(),
                        &CancellationToken::new(),
                    )
                    .await?,
            )
            .await?;
            assert!(events.iter().any(|event| {
                matches!(event, StreamEvent::TextDelta { text } if text == "Hello")
            }));
            assert!(matches!(events.last(), Some(StreamEvent::Stop { .. })));
            let captured = result.await??;
            let request_bytes = String::from_utf8_lossy(&captured[0]).to_ascii_lowercase();
            assert!(request_bytes.starts_with(&format!("post {expected_path} ")));
            assert!(request_bytes.contains(&format!("authorization: bearer {expected_key}")));
            assert!(request_bytes.contains("accept: text/event-stream"));
            assert!(request_bytes.contains("content-type: application/json"));
            assert!(request_bytes.contains("\"prompt_cache_key\":\"session-cache:1\""));
        }
        Ok(())
    }

    #[tokio::test]
    async fn scripted_usage_and_compaction_keep_their_typed_values() -> TestResult {
        let usage = Usage {
            input_tokens: 11,
            cached_input_tokens: 3,
            output_tokens: 5,
            reasoning_tokens: Some(2),
            cache_write_tokens: 1,
            cost_usd: Some(0.002),
        };
        let raw = RawJson::parse(r#"{"type":"summary","text":"keep raw"}"#)?;
        let history = CompactedHistory::new(Family::Responses, "gpt-test", vec![raw.clone()]);
        let script = crate::scripted::Script::new(vec![
            crate::scripted::ScriptStep::Usage(usage),
            crate::scripted::ScriptStep::Compact(CompactOutcome::Compacted(history)),
        ])?;
        let provider = Provider::Scripted(script);
        assert_eq!(provider.scripted_usage()?, usage);
        let result = provider
            .compact(
                SessionId::new_v7(),
                &request(Family::Responses, "gpt-test", Vec::new()),
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?;
        let Some(CompactOutcome::Compacted(history)) = result else {
            return Err(io::Error::other("Scripted compaction did not return its history").into());
        };
        assert_eq!(history.family(), Family::Responses);
        assert_eq!(history.model(), "gpt-test");
        assert_eq!(history.items(), &[raw]);
        Ok(())
    }

    #[tokio::test]
    async fn unresolved_blob_is_typed_and_sends_no_request() -> TestResult {
        let dir = TestDir::new()?;
        let (base, mut server_task) = server("/v1", vec![stream_reply(chat_reply())]).await?;
        let config = config(
            vec![entry(
                "chat-test",
                Family::Chat,
                base,
                Some("CHAT_TEST_KEY"),
                1,
                Transport::Https,
            )],
            0,
        );
        let set = ProviderSet::new(
            &config,
            identity(),
            EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
            dir.path(),
            dir.path(),
        )?;
        let provider = set.provider(resolved("chat-test", Family::Chat, "gpt-chat-test"))?;
        let blob_id = dal_core::BlobId::from_bytes(b"stored image bytes");
        let request = request(
            Family::Chat,
            "gpt-chat-test",
            vec![ContextItem::User {
                parts: vec![Part::Blob {
                    blob_id,
                    mime: "image/png".into(),
                    bytes: 18,
                }],
            }],
        );
        let result = provider
            .open(
                SessionId::new_v7(),
                &request,
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(ProviderError::UnresolvedBlob { blob_id: found }) if found == blob_id
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut server_task)
                .await
                .is_err()
        );
        server_task.abort();
        Ok(())
    }
    struct GatedServer {
        base: String,
        accepted: mpsc::Receiver<Vec<u8>>,
        ready: oneshot::Receiver<()>,
        release: oneshot::Sender<()>,
        task: JoinHandle<io::Result<Vec<Vec<u8>>>>,
    }

    async fn gated_server(body: String) -> io::Result<GatedServer> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (accepted_sender, accepted) = mpsc::channel(2);
        let (ready_sender, ready) = oneshot::channel();
        let (release, release_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut ready_sender = Some(ready_sender);
            let mut release_receiver = Some(release_receiver);
            let mut handlers = Vec::new();
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await?;
                let request = read_request(&mut socket).await?;
                accepted_sender
                    .send(request.clone())
                    .await
                    .map_err(|_| io::Error::other("request observer was dropped"))?;
                let body = body.clone();
                let ready = if index == 0 {
                    ready_sender.take()
                } else {
                    None
                };
                let release = if index == 0 {
                    release_receiver.take()
                } else {
                    None
                };
                handlers.push(tokio::spawn(async move {
                    if index == 0 {
                        write_reply_head(&mut socket, 200, "text/event-stream", body.len()).await?;
                        if let Some(ready) = ready {
                            let _ = ready.send(());
                        }
                        if let Some(release) = release {
                            let _ = release.await;
                        }
                        socket.write_all(body.as_bytes()).await?;
                        socket.flush().await?;
                    } else {
                        write_reply(
                            &mut socket,
                            Reply {
                                status: 200,
                                content_type: "text/event-stream",
                                body,
                            },
                        )
                        .await?;
                    }
                    Ok::<_, io::Error>(request)
                }));
            }
            let mut requests = Vec::new();
            for handler in handlers {
                match handler.await {
                    Ok(Ok(request)) => requests.push(request),
                    Ok(Err(error)) => return Err(error),
                    Err(error) => return Err(io::Error::other(error.to_string())),
                }
            }
            Ok(requests)
        });
        Ok(GatedServer {
            base: format!("http://{address}/v1"),
            accepted,
            ready,
            release,
            task,
        })
    }

    async fn write_reply_head(
        socket: &mut TcpStream,
        status: u16,
        content_type: &str,
        body_len: usize,
    ) -> io::Result<()> {
        let reason = if status == 200 { "OK" } else { "Test" };
        let headers = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {body_len}\r\nconnection: close\r\n\r\n"
        );
        socket.write_all(headers.as_bytes()).await?;
        socket.flush().await
    }

    async fn stalled_server() -> io::Result<(
        String,
        oneshot::Receiver<()>,
        JoinHandle<io::Result<bool>>,
    )> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (headers_sent, headers_received) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let _request = read_request(&mut socket).await?;
            write_reply_head(&mut socket, 200, "text/event-stream", 4096).await?;
            let _ = headers_sent.send(());
            let mut byte = [0_u8; 1];
            let closed = match tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte)).await {
                Ok(Ok(0)) | Ok(Err(_)) => true,
                Ok(Ok(_)) | Err(_) => false,
            };
            Ok(closed)
        });
        Ok((format!("http://{address}/v1"), headers_received, task))
    }

    #[tokio::test]
    async fn provider_semaphore_bounds_concurrent_local_requests() -> TestResult {
        let dir = TestDir::new()?;
        let mut gated = gated_server(chat_reply()).await?;
        let set = ProviderSet::new(
            &config(
                vec![entry(
                    "chat-test",
                    Family::Chat,
                    gated.base.clone(),
                    Some("CHAT_TEST_KEY"),
                    1,
                    Transport::Https,
                )],
                0,
            ),
            identity(),
            EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
            dir.path(),
            dir.path(),
        )?;
        let provider = set.provider(resolved("chat-test", Family::Chat, "gpt-chat-test"))?;
        let first_request = request(Family::Chat, "gpt-chat-test", vec![user_text("first")]);
        let first_stream = provider
            .open(
                SessionId::new_v7(),
                &first_request,
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?;
        let first_task = tokio::spawn(collect(first_stream));
        let first_wire = tokio::time::timeout(Duration::from_secs(1), gated.accepted.recv())
            .await?
            .ok_or_else(|| io::Error::other("first provider request was not accepted"))?;
        assert!(String::from_utf8_lossy(&first_wire).starts_with("POST /v1/chat/completions "));
        gated.ready.await?;

        let second_request = request(Family::Chat, "gpt-chat-test", vec![user_text("second")]);
        let second_stream = provider
            .open(
                SessionId::new_v7(),
                &second_request,
                &[],
                notice_sink(),
                &CancellationToken::new(),
            )
            .await?;
        let second_task = tokio::spawn(collect(second_stream));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), gated.accepted.recv())
                .await
                .is_err(),
            "the provider exceeded its configured concurrency cap"
        );

        assert!(gated.release.send(()).is_ok());
        let second_wire = tokio::time::timeout(Duration::from_secs(1), gated.accepted.recv())
            .await?
            .ok_or_else(|| io::Error::other("queued provider request was not admitted"))?;
        assert!(String::from_utf8_lossy(&second_wire).starts_with("POST /v1/chat/completions "));
        assert!(first_task.await??.iter().any(|event| matches!(event, StreamEvent::Stop { .. })));
        assert!(second_task.await??.iter().any(|event| matches!(event, StreamEvent::Stop { .. })));
        let _requests = gated.task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_closes_the_active_response_and_releases_its_permit() -> TestResult {
        let dir = TestDir::new()?;
        let (base, headers_received, server_task) = stalled_server().await?;
        let set = ProviderSet::new(
            &config(
                vec![entry(
                    "chat-test",
                    Family::Chat,
                    base,
                    Some("CHAT_TEST_KEY"),
                    1,
                    Transport::Https,
                )],
                0,
            ),
            identity(),
            EnvSnapshot::test(&[("CHAT_TEST_KEY", "chat-secret")]),
            dir.path(),
            dir.path(),
        )?;
        let provider = set.provider(resolved("chat-test", Family::Chat, "gpt-chat-test"))?;
        let cancel = CancellationToken::new();
        let stream = provider
            .open(
                SessionId::new_v7(),
                &request(Family::Chat, "gpt-chat-test", vec![user_text("wait")]),
                &[],
                notice_sink(),
                &cancel,
            )
            .await?;
        let consumer = tokio::spawn(async move {
            let mut stream = stream;
            stream.next().await
        });
        headers_received.await?;
        cancel.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(1), server_task).await??);
        assert_eq!(
            set.inner
                .providers
                .get("chat-test")
                .map(|slot| slot.permits.available_permits()),
            Some(1)
        );
        consumer.abort();
        Ok(())
    }
    #[tokio::test]
    async fn codex_401_refreshes_then_retries_with_the_stored_new_credential() -> TestResult {
        let dir = TestDir::new()?;
        let (codex_base, codex_server) = server(
            "/backend-api/codex",
            vec![
                Reply {
                    status: 401,
                    content_type: "application/json",
                    body: String::from(r#"{"error":{"type":"invalid_token","message":"expired"}}"#),
                },
                stream_reply(responses_reply()),
            ],
        )
        .await?;
        let (token_base, token_server) = server(
            "/codex",
            vec![Reply {
                status: 200,
                content_type: "application/json",
                body: String::from(
                    r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
                ),
            }],
        )
        .await?;

        let credential = OAuthCredential {
            access_token: crate::auth::credential::SecretString::from("old-access"),
            refresh_token: crate::auth::credential::SecretString::from("old-refresh"),
            expires_at: None,
            id_token: Some(String::from("old-id-token")),
            account_id: Some(String::from("acct-1")),
        };
        let mut auth = crate::auth::credential::AuthStore::empty(
            dir.path().join("auth.json"),
        );
        auth.set("openai-codex", Credential::OAuth(credential))?;
        auth.store()?;

        let codex_entry = entry(
            "openai-codex",
            Family::Codex,
            codex_base,
            None,
            1,
            Transport::Https,
        );
        let config = config(vec![codex_entry], 0);
        let endpoints = crate::auth::refresh::TokenEndpoints::with_bases(
            "http://127.0.0.1:1/anthropic",
            &token_base,
        )?;
        let set = ProviderSet::build(
            &config,
            identity(),
            EnvSnapshot::test(&[]),
            dir.path(),
            dir.path(),
            endpoints,
        )?;
        let provider = set.provider(resolved(
            "openai-codex",
            Family::Codex,
            "gpt-codex-test",
        ))?;
        let events = collect(
            provider
                .open(
                    SessionId::new_v7(),
                    &request(Family::Codex, "gpt-codex-test", vec![user_text("continue")]),
                    &[],
                    notice_sink(),
                    &CancellationToken::new(),
                )
                .await?,
        )
        .await?;
        assert!(events.iter().any(|event| matches!(event, StreamEvent::Stop { .. })));

        let codex_requests = codex_server.await??;
        assert_eq!(codex_requests.len(), 2);
        let first = String::from_utf8_lossy(&codex_requests[0]).to_ascii_lowercase();
        let second = String::from_utf8_lossy(&codex_requests[1]).to_ascii_lowercase();
        assert!(first.contains("authorization: bearer old-access"));
        assert!(second.contains("authorization: bearer new-access"));
        assert!(second.contains("\"prompt_cache_key\":\"session-cache:1\""));
        let token_requests = token_server.await??;
        assert_eq!(token_requests.len(), 1);
        assert!(String::from_utf8_lossy(&token_requests[0]).contains("old-refresh"));
        let stored = crate::auth::credential::AuthStore::load(dir.path().join("auth.json"))?;
        let Some(Credential::OAuth(stored)) = stored.credential("openai-codex") else {
            return Err(io::Error::other("refreshed OAuth credential was not stored").into());
        };
        assert_eq!(stored.access_token.expose(), "new-access");
        Ok(())
    }
}
