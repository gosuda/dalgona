//! The GitLab Duo family: a proxy surface and a workflow surface, never mixed.
//!
//! Duo Chat is a REST proxy: one `direct_access` token fans out to the
//! Anthropic and `OpenAI` proxy transports with the routed inner family
//! doing the encoding. Duo Agent is a workflow service: REST setup followed
//! by a stateful WebSocket ([`workflow`]) whose frames carry checkpoints,
//! tool actions, and approvals. The two surfaces share nothing but the
//! `Family::GitLab` label: different credentials, URLs, headers, lifecycles,
//! and terminal rules.
//!
//! Ground truth is the pinned oh-my-pi source at `a4dc3302`:
//! `packages/ai/src/providers/gitlab-duo.ts` (proxy),
//! `packages/ai/src/providers/gitlab-duo-workflow.ts` (workflow), and the
//! identity table `GITLAB_DUO_MODEL_IDENTITIES` with the `api-routes
//! provider="gitlab-duo"` rows in
//! `packages/catalog/src/compat/rules/runtime/behavior.kdl`. senpi
//! `b8c1d3a8` holds no GitLab provider.
//!
//! # Auth
//!
//! Both surfaces start from the GitLab credential (OAuth with `api` scope,
//! or a personal access token). The proxy exchanges it at
//! `POST /api/v4/ai/third_party_agents/direct_access` for a short-lived
//! token plus proxy headers, cached for 25 minutes. The workflow exchanges
//! it at `POST /api/v4/ai/duo_workflows/direct_access` for a per-run
//! workflow token. A GitLab token never rides an inference request itself.
//!
//! # Approval
//!
//! A `runMCPTool` action the workflow emits becomes an ordinary dal tool
//! call: it executes only through dal approval, like every other tool. The
//! `preapproved_tools` wire list is registration with the workflow service,
//! not authorization to execute. Routing remote tool requests through dal
//! approval at execution is owned by the host bridge join, which this module
//! does not bypass: it never executes a tool itself.

#[cfg(test)]
mod tests;
pub(crate) mod workflow;

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use dal_core::{ContextItem, Family, ModelRequest, ModelRoute, ReplaySource, SessionId};
use futures::{Stream, StreamExt, stream::BoxStream};
use sha2::{Digest, Sha256};

use crate::{
    auth::credential::Credential,
    error::ProviderError,
    family::{
        anthropic::{self, AnthropicAuth, AnthropicRequest},
        chat,
        codex::{redact, redact_provider_error},
        responses,
    },
    http::{self, Exchange},
    lifecycle::AttemptFailure,
    sse::{self, SseEvent},
    stream::{EventStream, ReplayPayload, StreamEvent},
    thinking::{ThinkingPlan, WireThinking},
};

/// The Anthropic proxy base; the family appends `v1/messages`.
pub(crate) const ANTHROPIC_PROXY_BASE: &str = "https://cloud.gitlab.com/ai/v1/proxy/anthropic/";

/// The `OpenAI` proxy base; chat appends `chat/completions`, responses
/// appends `responses`.
pub(crate) const OPENAI_PROXY_BASE: &str = "https://cloud.gitlab.com/ai/v1/proxy/openai/v1";

/// How long a proxy `direct_access` token is reused.
const DIRECT_ACCESS_TTL: Duration = Duration::from_mins(25);

/// The provider id every GitLab failure names.
pub(crate) const PROVIDER_ID: &str = "gitlab-duo";

/// How a Duo Chat model reaches the proxy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProxyRoute {
    /// Claude aliases through the Anthropic proxy with the `Claude` OAuth
    /// wire shape.
    Anthropic,
    /// GPT `Codex` aliases through the Responses proxy.
    Responses,
    /// Every other alias through the chat-completions proxy.
    Completions,
}

/// One Duo Chat alias: its upstream model id and, for the reference rows,
/// the bundled first-party model the catalog copies capabilities from.
#[derive(Clone, Copy, Debug)]
struct DuoIdentity {
    alias: &'static str,
    upstream: &'static str,
}

/// The pinned `GITLAB_DUO_MODEL_IDENTITIES` table. Capabilities, prices, and
/// limits come from the bundled reference rows at registration; this table
/// only maps the alias to the id the proxy serves.
const IDENTITIES: &[DuoIdentity] = &[
    DuoIdentity {
        alias: "duo-chat-opus-4-6",
        upstream: "claude-opus-4-6",
    },
    DuoIdentity {
        alias: "duo-chat-sonnet-4-6",
        upstream: "claude-sonnet-4-6",
    },
    DuoIdentity {
        alias: "duo-chat-opus-4-5",
        upstream: "claude-opus-4-5-20251101",
    },
    DuoIdentity {
        alias: "duo-chat-sonnet-4-5",
        upstream: "claude-sonnet-4-5-20250929",
    },
    DuoIdentity {
        alias: "duo-chat-haiku-4-5",
        upstream: "claude-haiku-4-5-20251001",
    },
    DuoIdentity {
        alias: "duo-chat-gpt-5-1",
        upstream: "gpt-5.1-2025-11-13",
    },
    DuoIdentity {
        alias: "duo-chat-gpt-5-2",
        upstream: "gpt-5.2-2025-12-11",
    },
    DuoIdentity {
        alias: "duo-chat-gpt-5-mini",
        upstream: "gpt-5-mini-2025-08-07",
    },
    DuoIdentity {
        alias: "duo-chat-gpt-5-codex",
        upstream: "gpt-5-codex",
    },
    DuoIdentity {
        alias: "duo-chat-gpt-5-2-codex",
        upstream: "gpt-5.2-codex",
    },
];

/// The models the pinned `api-routes` table sends to the Responses proxy.
const RESPONSES_EXACT: [&str; 4] = [
    "duo-chat-gpt-5-codex",
    "duo-chat-gpt-5-2-codex",
    "gpt-5-codex",
    "gpt-5.2-codex",
];

/// The upstream model id the proxy serves for one alias; unknown ids pass
/// through unchanged.
#[must_use]
pub(crate) fn upstream_model(model: &str) -> &str {
    IDENTITIES
        .iter()
        .find(|identity| identity.alias == model)
        .map_or(model, |identity| identity.upstream)
}

/// The proxy route of one model id, from the pinned `api-routes
/// provider="gitlab-duo"` rows: the `claude` prefixes ride Anthropic, the
/// four `Codex` ids ride Responses, and the default is completions.
#[must_use]
pub(crate) fn proxy_route(model: &str) -> ProxyRoute {
    if RESPONSES_EXACT.contains(&model) {
        return ProxyRoute::Responses;
    }
    let claude = model.starts_with("duo-chat-opus-")
        || model.starts_with("duo-chat-sonnet-")
        || model.starts_with("duo-chat-haiku-")
        || model.starts_with("claude-opus-")
        || model.starts_with("claude-sonnet-")
        || model.starts_with("claude-haiku-");
    if claude {
        return ProxyRoute::Anthropic;
    }
    ProxyRoute::Completions
}

/// A proxy `direct_access` grant: the token plus the headers the gateway
/// requires beside it. It has no `Debug` implementation; the token never
/// formats.
#[derive(Clone)]
pub(crate) struct DirectAccess {
    token: Box<str>,
    headers: Vec<(String, String)>,
}

/// The `direct_access` cache: one grant per GitLab token and configured
/// base, reused for [`DIRECT_ACCESS_TTL`].
///
/// The key is the exact configured base (trailing `/` trimmed, no case
/// folding: path segments are case-sensitive) joined with the SHA-256 digest
/// of the token. The token itself is never stored and never logged; the
/// digest is compared, never formatted. Epochs pre-registered before each
/// fetch make a `remove` during flight fail that fetch closed instead of
/// publishing a stale grant. Slots with no live fetcher and no fresh grant
/// are reaped by `prune_expired`; live flights always keep their slot.
#[derive(Default)]
pub(crate) struct DirectAccessCache {
    state: Mutex<CacheState>,
}

/// One cache key: the exact configured base with trailing slashes trimmed,
/// and the raw SHA-256 digest of the token.
#[derive(Clone, Eq, Hash, PartialEq)]
struct CacheKey {
    base: Box<str>,
    digest: [u8; 32],
}

/// The opaque identity of one cache epoch, pre-registered before its fetch.
/// Compared by [`Arc::ptr_eq`], never by value: the identity is the
/// allocation address, so no counter or retired-entry map is needed. The map
/// holds one ref while each in-flight fetch holds another, so the strong
/// count tells prune whether a slot is still active.
struct TokenEpoch;

#[derive(Clone)]
struct CachedGrant {
    grant: DirectAccess,
    expires: Instant,
}

struct TokenSlot {
    epoch: Arc<TokenEpoch>,
    grant: Option<CachedGrant>,
}

#[derive(Default)]
struct CacheState {
    slots: HashMap<CacheKey, TokenSlot>,
}

/// The raw SHA-256 digest of one token. The digest is the only token-derived
/// value the cache stores; it is compared, never logged or formatted.
fn token_digest(token: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(token.as_bytes());
    digest.finalize().into()
}

/// The cache key of one fetch: the exact configured base with trailing `/`
/// trimmed (no case folding), joined with the token digest. Pure; never logs.
fn cache_key(gitlab_base: &str, token: &str) -> CacheKey {
    CacheKey {
        base: Box::from(gitlab_base.trim_end_matches('/')),
        digest: token_digest(token),
    }
}

/// Whether one cached grant is still reusable: its TTL has not elapsed.
///
/// [`prune_expired`] clears expired grants before the reuse check, so in
/// production this sees an expired grant only when the TTL elapsed between
/// the prune and the check. It is a named predicate (rather than an inline
/// comparison) so the reuse rule is unit-testable in isolation.
#[must_use]
fn grant_fresh(cached: &CachedGrant) -> bool {
    Instant::now() < cached.expires
}

/// Drops expired grants and reaps slots no fetch can still publish into:
/// the map owns one epoch ref while each in-flight fetch owns another, so a
/// slot with no other owner is inactive and safe to drop. A live fetcher
/// keeps its slot (even with no grant cached) so its success still publishes
/// instead of failing spuriously. One-directional: a stale-high count only
/// misses a reap; a count of one always means inactive. No tombstones, no
/// caps: entries are live keys and active flights only.
fn prune_expired(slots: &mut HashMap<CacheKey, TokenSlot>) {
    let now = Instant::now();
    slots.retain(|_, slot| {
        if slot
            .grant
            .as_ref()
            .is_some_and(|cached| now >= cached.expires)
        {
            slot.grant = None;
        }
        slot.grant.is_some() || Arc::strong_count(&slot.epoch) > 1
    });
}

impl DirectAccessCache {
    /// The grant for one GitLab token and configured base, fetching it when
    /// absent or expired.
    ///
    /// The slot and its epoch are registered under one lock before the
    /// fetch; the grant is published under the same lock only when the slot
    /// still carries that epoch. A `remove` in between deletes the slot, so
    /// the fetch ends as a typed fail-closed transport error and never
    /// serves or caches a stale grant. A fetch failure inserts nothing and
    /// deletes nothing: waiters on the same epoch can still succeed, and the
    /// next call retries with it.
    ///
    /// # Errors
    ///
    /// The typed fail-closed transport error of a fetch invalidated
    /// mid-flight, and the mapped transport or status error of a rejected
    /// `direct_access` call.
    pub(crate) async fn ensure(
        &self,
        client: &reqwest::Client,
        gitlab_base: &str,
        token: &str,
        user_agent: &str,
    ) -> Result<DirectAccess, ProviderError> {
        let key = cache_key(gitlab_base, token);
        let epoch = {
            let mut state = lock(&self.state);
            // Reap inactive slots on entry: failed ensures leave empty,
            // inactive slots that no later success or remove would collect.
            // Live flights already hold an epoch ref (count > 1) so prune
            // keeps them; only dead empties and expired grants of inactive
            // slots go, and a pruned key simply registers fresh below.
            prune_expired(&mut state.slots);
            if let Some(slot) = state.slots.get(&key) {
                if let Some(cached) = &slot.grant
                    && grant_fresh(cached)
                {
                    return Ok(cached.grant.clone());
                }
                Arc::clone(&slot.epoch)
            } else {
                let epoch = Arc::new(TokenEpoch);
                state.slots.insert(
                    key.clone(),
                    TokenSlot {
                        epoch: Arc::clone(&epoch),
                        grant: None,
                    },
                );
                epoch
            }
        };
        let grant = fetch_direct_access(client, gitlab_base, token, user_agent).await?;
        let mut state = lock(&self.state);
        let published = if let Some(slot) = state.slots.get_mut(&key) {
            if Arc::ptr_eq(&slot.epoch, &epoch) {
                slot.grant = Some(CachedGrant {
                    grant: grant.clone(),
                    expires: Instant::now() + DIRECT_ACCESS_TTL,
                });
                true
            } else {
                false
            }
        } else {
            false
        };
        if published {
            prune_expired(&mut state.slots);
            Ok(grant)
        } else {
            Err(ProviderError::Transport {
                family: Family::GitLab,
                reason: String::from("gitlab grant invalidated during fetch"),
            })
        }
    }

    /// Drops every grant and epoch of one token, on every base. In-flight
    /// holders keep a dangling epoch that can never match again, so even a
    /// first fetch with no cached grant yet cannot return or cache a stale
    /// grant, then reaps other inactive slots. Idempotent; never logs.
    pub(crate) fn remove(&self, token: &str) {
        let digest = token_digest(token);
        let mut state = lock(&self.state);
        state.slots.retain(|key, _| key.digest != digest);
        prune_expired(&mut state.slots);
    }

    /// Drops expired grants and reaps slots with no live fetcher (see
    /// [`prune_expired`]); in-flight epochs are never touched.
    #[cfg(test)]
    pub(crate) fn clear_expired(&self) {
        prune_expired(&mut lock(&self.state).slots);
    }

    /// The live slot count, for tests proving prune reaps only inactive
    /// slots. Never used in production.
    #[cfg(test)]
    pub(crate) fn slot_count(&self) -> usize {
        lock(&self.state).slots.len()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Fetches one `direct_access` grant.
///
/// # Errors
///
/// The mapped transport error, or [`ProviderError::Status`] carrying the
/// pinned detail text (with the access-denied fix on 403).
async fn fetch_direct_access(
    client: &reqwest::Client,
    gitlab_base: &str,
    token: &str,
    user_agent: &str,
) -> Result<DirectAccess, ProviderError> {
    let url = http::endpoint(
        Family::GitLab,
        gitlab_base,
        "api/v4/ai/third_party_agents/direct_access",
    )?;
    let response = http::send(
        Family::GitLab,
        client
            .post(url)
            .header("authorization", format!("Bearer {token}"))
            .body(r#"{"feature_flags":{"DuoAgentPlatformNext":true}}"#),
        user_agent,
        Exchange::Json {
            total: http::OAUTH_TIMEOUT,
        },
        tokio::time::sleep,
    )
    .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let detail = http::read_body(Family::GitLab, response)
            .await
            .map(|body| String::from_utf8_lossy(&body).into_owned())
            .unwrap_or_default();
        return Err(direct_access_status(status, &detail));
    }
    let body = http::read_body(Family::GitLab, response).await?;
    parse_direct_access(&body)
}

/// The failure of a rejected `direct_access` call, with the pinned texts.
fn direct_access_status(status: u16, detail: &str) -> ProviderError {
    if status == 403 {
        return ProviderError::Status {
            family: Family::GitLab,
            status,
            message: format!(
                "GitLab Duo access denied. Ensure Duo is enabled for this account. {detail}"
            ),
        };
    }
    ProviderError::Status {
        family: Family::GitLab,
        status,
        message: format!("Failed to get GitLab Duo direct access token: {status} {detail}"),
    }
}

/// Parses one `direct_access` grant body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the token or the headers are
/// missing or misshapen.
fn parse_direct_access(body: &[u8]) -> Result<DirectAccess, ProviderError> {
    use sonic_rs::{JsonContainerTrait, JsonValueTrait};
    let missing = || invalid("GitLab Duo direct access response missing token or headers");
    let value: sonic_rs::Value = sonic_rs::from_slice(body).map_err(|_| missing())?;
    let token = value
        .get("token")
        .and_then(|token| token.as_str())
        .filter(|token| !token.is_empty())
        .ok_or_else(missing)?;
    let headers = value.get("headers").ok_or_else(missing)?;
    let headers = headers
        .as_object()
        .ok_or_else(missing)?
        .iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.to_owned(), value.to_owned()))
                .ok_or_else(missing)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DirectAccess {
        token: Box::from(token),
        headers,
    })
}

/// Everything one Duo Chat proxy request needs.
///
/// It has no `Debug` implementation because the grant carries the token.
#[derive(Clone, Copy)]
pub(crate) struct DuoProxyRequest<'a> {
    /// The neutral request; its route is `Family::GitLab` and the alias.
    pub(crate) request: &'a ModelRequest,
    /// The composed thinking plan; its fragment matches the route's family.
    pub(crate) thinking: &'a ThinkingPlan,
    /// The `direct_access` grant of this turn.
    pub(crate) grant: &'a DirectAccess,
    /// The model's output ceiling, when the catalog knows it.
    pub(crate) max_output: Option<u32>,
    /// Whether the Responses model supports reasoning summaries.
    pub(crate) reasoning_summary: bool,
    /// Whether the Anthropic model supports the adaptive display hint.
    pub(crate) display_supported: bool,
    /// The dal session; it is the prompt cache key.
    pub(crate) session: SessionId,
}

/// One lowered proxy request.
///
/// It has no `Debug` implementation because its headers carry the grant.
#[must_use]
pub(crate) struct DuoProxyWire {
    /// The route the alias resolved to.
    pub(crate) route: ProxyRoute,
    /// The full proxy URL.
    pub(crate) url: url::Url,
    /// Headers in send order; `authorization` is first.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The grant headers the gateway requires beside `authorization`; their
    /// names arrive at runtime, so they travel separately from the static set.
    pub(crate) grant_headers: Vec<(String, String)>,
    /// The `user-agent` override; set only on the Anthropic route, where the
    /// `Claude` fingerprint replaces the dalgon one.
    pub(crate) user_agent: Option<String>,
    /// The exact body bytes of the route's family encoder.
    pub(crate) body: Vec<u8>,
    /// The alias, for decoder binding and error mapping.
    pub(crate) model: Box<str>,
    /// The Anthropic replay prefix the decoder binds signed thinking to.
    pub(crate) replay_prefix: Option<Box<str>>,
    /// The grant token, for redacting server messages.
    pub(crate) secret: Box<str>,
}

/// Lowers one Duo Chat request through the route's family encoder.
///
/// The Anthropic route uses the `Claude` OAuth wire shape with the grant
/// token, exactly as the pinned proxy does (`isOAuth: true`): the shape's
/// Claude Code identity system block, `_`-prefixed tool names, betas, and
/// fingerprint all ride the request. The shape's own `authorization` header
/// is dropped because the grant bearer always leads, followed by the grant
/// headers.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the route is not a GitLab route,
/// the thinking plan belongs to another family, or a body encoder rejects
/// the request.
pub(crate) fn build_proxy(input: &DuoProxyRequest<'_>) -> Result<DuoProxyWire, ProviderError> {
    let ModelRoute::Api {
        family: Family::GitLab,
        model,
    } = &input.request.model
    else {
        return Err(invalid(
            "a GitLab Duo request needs a gitlab-duo model route",
        ));
    };
    let route = proxy_route(model);
    let upstream = upstream_model(model);
    let derived = derived_request(input, route, upstream);
    let (body, family_headers, user_agent, prefix) = match route {
        ProxyRoute::Anthropic => {
            let WireThinking::Anthropic { thinking, effort } = input.thinking.wire else {
                return Err(foreign_thinking());
            };
            let lowered = anthropic::build(
                &AnthropicRequest {
                    request: &derived,
                    max_output: input.max_output,
                    thinking,
                    effort,
                    display_supported: input.display_supported,
                    temperature: input.thinking.temperature,
                    compaction: None,
                    summarize: false,
                },
                AnthropicAuth::ClaudeOAuth {
                    access_token: &input.grant.token,
                    version: crate::claude_fingerprint::CLAUDE_CODE_VERSION,
                },
            )?;
            (
                lowered.body,
                lowered.headers,
                lowered.user_agent,
                Some(lowered.prefix),
            )
        }
        ProxyRoute::Responses => {
            let body =
                responses::request_body(&derived, input.thinking.wire, input.reasoning_summary)?;
            (body, Vec::new(), None, None)
        }
        ProxyRoute::Completions => {
            let WireThinking::OpenAi { .. } = input.thinking.wire else {
                return Err(foreign_thinking());
            };
            let body = chat::request_body(&derived, input.thinking)
                .map_err(|error| invalid(error.to_string()))?;
            (body, Vec::new(), None, None)
        }
    };
    let (base, path) = match route {
        ProxyRoute::Anthropic => (ANTHROPIC_PROXY_BASE, anthropic::MESSAGES_PATH),
        ProxyRoute::Responses => (OPENAI_PROXY_BASE, responses::PATH),
        ProxyRoute::Completions => (OPENAI_PROXY_BASE, chat::ENDPOINT_PATH),
    };
    let url = http::endpoint(Family::GitLab, base, path)?;
    let mut headers = vec![(
        "authorization",
        format!("Bearer {}", input.grant.token.as_ref()),
    )];
    headers.extend(
        family_headers
            .into_iter()
            .filter(|(name, _)| route != ProxyRoute::Anthropic || *name != "authorization"),
    );
    Ok(DuoProxyWire {
        route,
        url,
        headers,
        grant_headers: input.grant.headers.clone(),
        user_agent,
        body,
        model: model.clone(),
        replay_prefix: prefix,
        secret: input.grant.token.clone(),
    })
}

/// The request one family encoder lowers: the route's family and the
/// upstream model id replace the alias route, and the session id is the
/// cache key. The system text is unchanged; the Anthropic encoder prepends
/// its own OAuth identity block. Assistant entries journaled under
/// [`Family::GitLab`] rebind to the encoder family and the upstream id of
/// their alias: the journal records the alias route while the wire carries
/// the upstream id, and the encoders replay a stored block only when its
/// source names the wire family and model.
fn derived_request(input: &DuoProxyRequest<'_>, route: ProxyRoute, upstream: &str) -> ModelRequest {
    let family = match route {
        ProxyRoute::Anthropic => Family::Anthropic,
        ProxyRoute::Responses => Family::Responses,
        ProxyRoute::Completions => Family::Chat,
    };
    let request = input.request;
    let rebinds = request.context.iter().any(
        |item| matches!(item, ContextItem::Assistant { source, .. } if source.family == Family::GitLab),
    );
    let context = if rebinds {
        request
            .context
            .iter()
            .map(|item| rebound(item, family))
            .collect()
    } else {
        Arc::clone(&request.context)
    };
    ModelRequest {
        purpose: request.purpose,
        model: ModelRoute::Api {
            family,
            model: Box::from(upstream),
        },
        system: Arc::clone(&request.system),
        tools: Arc::clone(&request.tools),
        context,
        params: request.params.clone(),
        cache_key: Some(Box::from(input.session.to_string())),
    }
}

fn rebound(item: &ContextItem, family: Family) -> ContextItem {
    match item {
        ContextItem::Assistant {
            source,
            parts,
            replay,
        } if source.family == Family::GitLab => ContextItem::Assistant {
            source: ReplaySource {
                family,
                model: Box::from(upstream_model(&source.model)),
            },
            parts: parts.clone(),
            replay: replay.clone(),
        },
        other => other.clone(),
    }
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

fn foreign_thinking() -> ProviderError {
    invalid("the thinking plan does not belong to the GitLab route")
}

/// Sends one proxy request and decodes its stream.
///
/// Failures follow the shared shape: a rejected response becomes the
/// [`AttemptFailure::Response`] the lifecycle classifies, a replay payload
/// rebinds to [`Family::GitLab`], and a body that stops reading mid-stream
/// ends as a transport failure.
///
/// # Errors
///
/// [`AttemptFailure`] for a transport failure before the response head or a
/// rejected response.
pub(crate) async fn open(
    client: &reqwest::Client,
    wire: DuoProxyWire,
    user_agent: &str,
) -> Result<EventStream, AttemptFailure> {
    let DuoProxyWire {
        route,
        url,
        headers,
        grant_headers,
        user_agent: wire_agent,
        body,
        model,
        replay_prefix,
        secret,
    } = wire;
    let secrets = [secret];
    let mut request = client.post(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    for (name, value) in grant_headers {
        request = request.header(name, value);
    }
    let agent = wire_agent.as_deref().unwrap_or(user_agent);
    let response = http::send(
        Family::GitLab,
        request.body(body),
        agent,
        Exchange::Stream,
        tokio::time::sleep,
    )
    .await?;
    if !response.status().is_success() {
        return Err(rejected(response, &secrets).await);
    }
    let read_failed = Arc::new(AtomicBool::new(false));
    let chunks = body_chunks(response.bytes_stream(), Arc::clone(&read_failed));
    let events = sse::decode_stream(chunks);
    let decoded = decode_proxy(route, events, model, replay_prefix.as_deref());
    let guarded = decoded.map(move |item| match item {
        Err(_) if read_failed.load(Ordering::Acquire) => {
            let mut error = ProviderError::Transport {
                family: Family::GitLab,
                reason: String::from("GitLab Duo response body read failed"),
            };
            redact_provider_error(&mut error, &secrets);
            Err(error)
        }
        Err(mut error) => {
            redact_provider_error(&mut error, &secrets);
            Err(error)
        }
        Ok(event) => Ok(rebind_replay(event)),
    });
    Ok(EventStream::new(guarded, || {}))
}

/// Ends the chunk stream at the first read error and records it.
fn body_chunks(
    bytes: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    failed: Arc<AtomicBool>,
) -> impl Stream<Item = Vec<u8>> + Send + 'static {
    bytes.scan(failed, |failed, chunk| {
        let next = if let Ok(bytes) = chunk {
            Some(bytes.to_vec())
        } else {
            failed.store(true, Ordering::Release);
            None
        };
        std::future::ready(next)
    })
}

/// The route's decoder over the SSE events of one response.
fn decode_proxy<S>(
    route: ProxyRoute,
    events: S,
    model: Box<str>,
    replay_prefix: Option<&str>,
) -> BoxStream<'static, Result<StreamEvent, ProviderError>>
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static,
{
    match route {
        ProxyRoute::Completions => {
            chat::decode_events(events, chat::ChatDecoder::new(PROVIDER_ID, &model)).boxed()
        }
        ProxyRoute::Responses => responses::decode(events, Family::Responses, model).boxed(),
        ProxyRoute::Anthropic => {
            anthropic::decode_stream(events, model, true, replay_prefix).boxed()
        }
    }
}

/// Binds a decoded replay payload to [`Family::GitLab`].
fn rebind_replay(event: StreamEvent) -> StreamEvent {
    match event {
        StreamEvent::Replay { payload } => StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::GitLab,
                ..payload
            },
        },
        other => other,
    }
}

/// The failure of a non-success proxy response.
async fn rejected(response: reqwest::Response, secrets: &[Box<str>]) -> AttemptFailure {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(String::from);
    let body = match http::read_body(Family::GitLab, response).await {
        Ok(body) => body,
        Err(error) => return AttemptFailure::Provider(error),
    };
    let text = redact(&String::from_utf8_lossy(&body), secrets).into_owned();
    let (code, message) = error_fields(&text);
    AttemptFailure::Response {
        status,
        code,
        message,
        retry_after,
    }
}

/// The error code and message of a proxy error body. Unlike the Droid proxy,
/// the pinned Duo Chat source maps no region refusal, so the body passes
/// through verbatim.
fn error_fields(body: &str) -> (Option<String>, String) {
    use sonic_rs::JsonValueTrait;
    let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(body) else {
        return (None, String::from(body));
    };
    let error = value.get("error");
    let text = |holder: Option<&sonic_rs::Value>, key: &str| {
        holder
            .and_then(|holder| holder.get(key))
            .and_then(|field| field.as_str())
            .map(String::from)
    };
    let code = text(error, "code").or_else(|| text(error, "type"));
    let message = error
        .and_then(|error| error.as_str().map(String::from))
        .or_else(|| text(error, "message"))
        .or_else(|| text(Some(&value), "message"))
        .or_else(|| text(Some(&value), "detail"))
        .unwrap_or_else(|| String::from(body));
    (code, message)
}

/// The GitLab credential token of a request, or the refusal.
///
/// # Errors
///
/// [`ProviderError::NoCredentials`] without a usable credential. An API key
/// and an OAuth sign-in both serve: the PAT rides `direct_access` as-is and
/// the OAuth token the same way.
pub(crate) fn require_token(credential: &Credential) -> Result<&str, ProviderError> {
    match credential {
        Credential::ApiKey { key } if !key.expose().trim().is_empty() => Ok(key.expose()),
        Credential::OAuth(oauth) if !oauth.access_token.expose().trim().is_empty() => {
            Ok(oauth.access_token.expose())
        }
        Credential::ApiKey { .. } | Credential::OAuth(_) | Credential::None => {
            Err(ProviderError::NoCredentials {
                provider: String::from("gitlab-duo"),
            })
        }
    }
}
