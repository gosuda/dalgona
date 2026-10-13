//! Provider assembly: HTTP-backed adapters and the family-neutral API.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
};

use dal_core::{Family, ModelInfo, ModelRequest, ModelRoute, Purpose, SessionId, Usage};
use tokio_util::sync::CancellationToken;

use super::config::{ProviderEntry, Transport};
use super::set::{ProviderSet, ProviderSetInner};
use crate::{
    auth::credential::{Credential, OAuthCredential},
    catalog::{self, CatalogFetch, ModelFetch, ResolvedModel},
    compact::{self, CompactOutcome},
    error::{self, ProviderError, UsageCheckReason},
    lifecycle::{self, Plan},
    stream::{EventStream, NoticeSink},
    thinking::{self, BeforeRequest, BeforeRequestPatch, ThinkingPlan},
    usage::UsageVerdict,
};

use super::request as request_mod;
use super::transport as transport_mod;

/// One HTTP-backed provider adapter. Its fields stay private; construct it
/// through [`ProviderSet::provider`] or [`Provider::new`].
#[must_use]
pub struct Http {
    pub(crate) resolved: ResolvedModel,
    pub(crate) entry: ProviderEntry,
    pub(crate) credential: Credential,
    pub(crate) client: reqwest::Client,
    pub(crate) user_agent: Box<str>,
    owner: Weak<ProviderSetInner>,
}

/// A provider family and its bound route, credential, and transport state.
#[must_use]
pub enum Provider {
    /// `OpenAI` Chat Completions.
    Chat(Http),
    /// `OpenAI` Responses, over HTTPS or its configured WebSocket transport.
    Responses(Http),
    /// `OpenAI` Codex, over HTTPS or its pooled WebSocket transport.
    Codex(Http),
    /// Anthropic Messages.
    Anthropic(Http),
    /// Deterministic provider steps for tests and headless replay.
    Scripted(crate::scripted::Script),
}
struct OpenTurn<'a> {
    http: &'a Http,
    owner: &'a Arc<ProviderSetInner>,
    session: SessionId,
    request: &'a ModelRequest,
    thinking: ThinkingPlan,
    credential: Credential,
    notices: NoticeSink,
    cancel: &'a CancellationToken,
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
        let slot =
            owner
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
                message: String::from(
                    "WebSocket transport is supported only by Responses and Codex",
                ),
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
            Self::Chat(http)
            | Self::Responses(http)
            | Self::Codex(http)
            | Self::Anthropic(http) => Some(http),
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
        request_mod::ensure_request(http, request)?;
        if http.entry.family == Family::Codex && !matches!(&http.credential, Credential::OAuth(_)) {
            return Err(ProviderError::InvalidRequest {
                message: format!("{} needs an OAuth credential", http.entry.id),
            });
        }
        if let Some(blob_id) = request_mod::unresolved_blob(request) {
            return Err(ProviderError::UnresolvedBlob { blob_id });
        }
        let names = request_mod::tool_names(http, request)?;
        let owner = http
            .owner
            .upgrade()
            .ok_or_else(|| ProviderError::InvalidRequest {
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
                request_mod::request_model(request),
                request.params.thinking,
            ) => result?,
        };
        let patch = request_mod::merge_request_parameters(request, patch)?;
        let thinking = request_mod::make_thinking_plan(http, request, &patch)?;
        let family = request_mod::request_family(request)?;
        if !request.tools.is_empty()
            && !http
                .resolved
                .entry
                .tool_support
                .allows(family, thinking.level)
        {
            return Err(ProviderError::InvalidRequest {
                message: format!(
                    "{} does not support tools for thinking level {}",
                    error::family_label(family),
                    thinking::level_name(thinking.level)
                ),
            });
        }
        request_mod::emit_thinking_notices(
            &owner,
            session,
            request_mod::request_model(request),
            &thinking.notices,
            &notices,
        );
        let credential = transport_mod::refresh_expiring(
            &owner.refresher,
            &http.entry,
            &http.credential,
            cancel,
        )
        .await?;
        let turn = OpenTurn {
            http,
            owner: &owner,
            session,
            request,
            thinking,
            credential,
            notices,
            cancel,
        };
        let stream = if turn.http.entry.transport == Transport::Websocket {
            Box::pin(self.open_websocket(turn)).await?
        } else {
            Self::open_http(turn)?
        };
        Ok(stream.restoring(names))
    }

    async fn open_websocket(&self, turn: OpenTurn<'_>) -> Result<EventStream, ProviderError> {
        let OpenTurn {
            http,
            owner,
            session,
            request,
            thinking,
            credential,
            notices,
            cancel,
        } = turn;
        let permit = transport_mod::acquire_provider_permit(owner, &http.entry.id, cancel).await?;
        let retries = owner.config.stream_max_retries;
        let context = request_mod::stream_context(http, request, thinking.clone(), session);
        let ws_turn = match request_mod::request_family(request)? {
            Family::Codex => {
                let Credential::OAuth(oauth) = &credential else {
                    return Err(ProviderError::InvalidRequest {
                        message: format!("{} needs an OAuth credential", http.entry.id),
                    });
                };
                let wire =
                    request_mod::build_codex_wire(&context, request, &thinking, oauth, session)?;
                Box::pin(owner.ws.open(crate::ws::WsRequest {
                    provider: &http.entry.id,
                    session_id: session,
                    base_url: &http.entry.base_url,
                    wire: crate::ws::WsWire::Codex(&wire),
                    stream_max_retries: retries,
                    refreshed: false,
                    notices: &notices,
                    cancel,
                }))
                .await?
            }
            Family::Responses => {
                let wire = request_mod::build_responses_wire_from_context(&context, &credential)?;
                Box::pin(owner.ws.open(crate::ws::WsRequest {
                    provider: &http.entry.id,
                    session_id: session,
                    base_url: &http.entry.base_url,
                    wire: crate::ws::WsWire::Responses(&wire),
                    stream_max_retries: retries,
                    refreshed: false,
                    notices: &notices,
                    cancel,
                }))
                .await?
            }
            Family::Chat | Family::Anthropic => {
                return Err(ProviderError::InvalidRequest {
                    message: String::from("this provider family has no WebSocket transport"),
                });
            }
        };
        match ws_turn {
            crate::ws::WsTurn::Stream(stream) => Ok(transport_mod::hold_permit(stream, permit)),
            crate::ws::WsTurn::HttpsFallback => {
                drop(permit);
                let turn = OpenTurn {
                    http,
                    owner,
                    session,
                    request,
                    thinking,
                    credential,
                    notices,
                    cancel,
                };
                Self::open_http(turn)
            }
        }
    }

    fn open_http(turn: OpenTurn<'_>) -> Result<EventStream, ProviderError> {
        let OpenTurn {
            http,
            owner,
            session,
            request,
            thinking,
            credential,
            notices,
            cancel,
        } = turn;
        let family = request_mod::request_family(request)?;
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
        let ctx = request_mod::stream_context(http, request, thinking, session);
        let ctx_for_attempt = ctx.clone();
        let attempt = move |credential: &Credential, attempt_cancel: &CancellationToken| {
            let ctx = ctx_for_attempt.clone();
            let credential = credential.clone();
            let attempt_cancel = attempt_cancel.clone();
            async move { request_mod::stream_attempt(ctx, credential, attempt_cancel).await }
        };
        let refresher = Arc::clone(&owner.refresher);
        let (id, def) = (http.entry.id.clone(), http.entry.def);
        let refresh = move |held: OAuthCredential| {
            transport_mod::refresh_credential(Arc::clone(&refresher), id.clone(), def, held)
        };
        Ok(lifecycle::stream(plan, credential, attempt, refresh))
    }

    async fn fetch_catalog(&self) -> Result<CatalogFetch, ProviderError> {
        let http = self.http().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("the scripted provider has no model catalog"),
        })?;
        let owner = http
            .owner
            .upgrade()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: String::from("provider host has ended"),
            })?;
        let slot =
            owner
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
        let credential = transport_mod::refresh_expiring(
            &owner.refresher,
            &http.entry,
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
    ///
    /// # Errors
    /// Returns `InvalidRequest` when this provider has no model catalog or
    /// its host has ended.
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
        let owner = http
            .owner
            .upgrade()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: String::from("provider host has ended"),
            })?;
        Ok(rows
            .into_iter()
            .map(|entry| request_mod::model_info(&entry, http.entry.family, owner.config.thinking))
            .collect())
    }

    /// Reads Codex account usage through its shared single-flight checker.
    /// The host owns each spawned task through the supplied callback.
    ///
    /// # Errors
    /// Returns `Transport` when this provider is not a Codex account route,
    /// its host has ended, or its usage checker is unavailable.
    pub async fn usage<F>(&self, spawn: &F) -> Result<UsageVerdict, UsageCheckReason>
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
        let owner = http
            .owner
            .upgrade()
            .ok_or_else(|| UsageCheckReason::Transport {
                reason: String::from("provider host has ended"),
            })?;
        let checker =
            owner
                .usage
                .get(&http.entry.id)
                .ok_or_else(|| UsageCheckReason::Transport {
                    reason: String::from("Codex usage checker is unavailable"),
                })?;
        let slot =
            owner
                .providers
                .get(&http.entry.id)
                .ok_or_else(|| UsageCheckReason::Transport {
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
    ///
    /// # Errors
    /// Returns `InvalidRequest` when this provider is not scripted.
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
    ///
    /// # Errors
    /// Returns `InvalidRequest` when this provider is not HTTP-backed or the
    /// request is not a compact-purpose request.
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
        request_mod::ensure_request(http, request)?;
        if request.purpose != Purpose::Compact {
            return Err(ProviderError::InvalidRequest {
                message: String::from("remote compaction needs a compact-purpose request"),
            });
        }
        if let Some(blob_id) = request_mod::unresolved_blob(request) {
            return Err(ProviderError::UnresolvedBlob { blob_id });
        }
        request_mod::tool_names(http, request)?;
        if http.entry.family == Family::Chat {
            return Ok(Some(compact::chat()));
        }
        if !http.resolved.entry.remote_compact {
            return Ok(Some(CompactOutcome::Unsupported));
        }
        if http.entry.family == Family::Codex && !matches!(&http.credential, Credential::OAuth(_)) {
            return Err(ProviderError::InvalidRequest {
                message: format!("{} needs an OAuth credential", http.entry.id),
            });
        }
        let owner = http
            .owner
            .upgrade()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: String::from("provider host has ended"),
            })?;
        let patch = request_mod::merge_request_parameters(request, BeforeRequestPatch::default())?;
        let thinking = request_mod::make_thinking_plan(http, request, &patch)?;
        request_mod::emit_thinking_notices(
            &owner,
            session,
            request_mod::request_model(request),
            &thinking.notices,
            &notices,
        );
        let credential = transport_mod::refresh_expiring(
            &owner.refresher,
            &http.entry,
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
        let ctx = request_mod::CompactContext {
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
            async move { request_mod::compact_attempt(ctx, credential, attempt_cancel).await }
        };
        let refresher = Arc::clone(&owner.refresher);
        let (id, def) = (http.entry.id.clone(), http.entry.def);
        let refresh = move |held: OAuthCredential| {
            transport_mod::refresh_credential(Arc::clone(&refresher), id.clone(), def, held)
        };
        lifecycle::request(plan, credential, attempt, refresh)
            .await
            .map(|outcome| outcome.map(CompactOutcome::Compacted))
    }
}

impl Http {
    pub(crate) fn model(&self) -> &str {
        &self.resolved.entry.id
    }
}
