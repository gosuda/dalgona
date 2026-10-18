//! Provider assembly: HTTP-backed adapters and the family-neutral API.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
};

use dal_core::{Family, ModelInfo, ModelRequest, ModelRoute, Purpose, SessionId, Usage};
use tokio_util::sync::CancellationToken;

use super::config::{AuthStyle, ProviderEntry, Transport};
use super::set::{ProviderSet, ProviderSetInner, authenticates_ambiently};
use crate::family::cursor::{decode as cursor_decode, lower as cursor_lower};
use crate::run::{HostToolResult, ProviderRun};
use crate::{
    auth::credential::{Credential, OAuthCredential},
    catalog::{self, CatalogFetch, ModelFetch, ResolvedModel},
    compact::{self, CompactOutcome},
    error::{self, ProviderError, UsageCheckReason},
    family::azure,
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
    pub(super) owner: Weak<ProviderSetInner>,
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
    /// Every other HTTP-backed family (Bedrock, Cursor, Devin, Droid, GitLab,
    /// the Google HTTPS surfaces, Mistral, Ollama, `pi-messages`): one general
    /// variant whose request family drives the dispatch in [`Provider::open`].
    Http(Http),
    /// `google-gemini-cli`: a credential-less local subprocess, never HTTP.
    /// It drives `gemini_cli::spawn().into_events()` instead of `http::send`,
    /// so it stays out of the HTTP catalog, refresh, and decode paths.
    GeminiCli(Http),
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
    /// not agree, and `NoCredentials` when an HTTP route has no credential and
    /// its family authenticates through one.
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
            || slot.entry.settings != entry.settings
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
        if request_mod::is_azure_entry(&entry) || request_mod::is_copilot_entry(&entry) {
            azure::require_https(&entry.id, entry.transport)?;
        }
        // An entry that authenticates nothing admits the credential-less
        // row; every other style still refuses to build without one. Bedrock
        // also admits it: the AWS chain in `aws_resolver` owns Bedrock auth
        // (bearer-or-SigV4 per attempt), never the bound credential. Vertex
        // admits it too: without a key or stored sign-in each attempt reads
        // the pre-minted bearer token from the captured environment and
        // refuses with `NoCredentials` itself when there is none.
        if matches!(&credential, Credential::None)
            && entry.auth != AuthStyle::None
            && !authenticates_ambiently(entry.family)
        {
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
        Ok(Self::from_family(
            *family,
            Http {
                resolved,
                entry,
                credential,
                client: client.clone(),
                user_agent: owner.inner.user_agent.clone(),
                owner: Arc::downgrade(&owner.inner),
            },
        ))
    }

    /// Binds each family to its provider variant. Every family is named, so
    /// a new `Family` must choose its variant here instead of falling into
    /// an existing codec's path.
    fn from_family(family: Family, http: Http) -> Self {
        match family {
            Family::Chat => Self::Chat(http),
            Family::Responses => Self::Responses(http),
            Family::Codex => Self::Codex(http),
            Family::Anthropic => Self::Anthropic(http),
            Family::Bedrock
            | Family::Cursor
            | Family::Devin
            | Family::Droid
            | Family::GitLab
            | Family::GoogleGenerativeAi
            | Family::GoogleVertex
            | Family::GoogleAntigravity
            | Family::Mistral
            | Family::Ollama
            | Family::PiMessages => Self::Http(http),
            Family::GoogleGeminiCli => Self::GeminiCli(http),
        }
    }

    fn http(&self) -> Option<&Http> {
        match self {
            Self::Chat(http)
            | Self::Responses(http)
            | Self::Codex(http)
            | Self::Anthropic(http)
            | Self::Http(http) => Some(http),
            // The subprocess family is never HTTP-backed: its turn drives
            // the child, so the HTTP catalog, refresh, and decode paths
            // below stay unreachable for it.
            Self::GeminiCli(_) | Self::Scripted(_) => None,
        }
    }

    /// Whether this provider speaks the Cursor duplex `Run` protocol.
    ///
    /// Ordinary [`Provider::open`] routes here internally for a single step.
    /// Callers that keep a run across tool exchanges select Cursor requests
    /// by the family of their model route and call [`Provider::open_run`] and
    /// [`Provider::resume_run`] directly.
    pub(crate) fn is_cursor(&self) -> bool {
        matches!(self, Self::Http(http) if http.entry.family == Family::Cursor)
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
        // A fresh single-step Cursor call works without keeping a run: the
        // returned stream owns its run until the terminal, so a live run is
        // never dropped before its `Stop`. A run left awaiting host results
        // after a tool-use stop is released with the stream, never resumed.
        if self.is_cursor() {
            return self
                .open_cursor_single(session, request, hooks, notices, cancel)
                .await;
        }
        // The subprocess turn drives the child under admission; it never
        // enters the HTTP hook, thinking-plan, or refresh path.
        if let Self::GeminiCli(http) = self {
            return request_mod::gemini_cli_open(http, session, request, notices, cancel).await;
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

    /// Opens one Cursor run and returns its owner plus its first step stream.
    ///
    /// The pre-wire pipeline of [`Provider::open`] is preserved
    /// (route and blob validation, hooks, merged parameters, the thinking
    /// plan and support check, thinking notices, credential refresh) and
    /// fails before any socket is opened. A provider permit is acquired
    /// before the HTTP/2 handshake; the first step keeps it through its
    /// `Stop` or error via [`transport_mod::hold_permit`]. A run awaiting
    /// host results holds no permit; its owner drives the keepalive future
    /// and no provider task is spawned.
    ///
    /// The decoded step events already carry internal tool names, so they are
    /// handed to the run directly and `respond` matches journaled names
    /// verbatim.
    ///
    /// # Errors
    /// Returns `InvalidRequest` for a non-Cursor provider, a WebSocket entry,
    /// an unlowerable request, or hook/thinking misuse; `UnresolvedBlob` for
    /// stored content; `NoCredentials` without a key; `Transport` when the
    /// handshake cannot start or admission is closed/cancelled.
    pub async fn open_run(
        &self,
        session: SessionId,
        request: &ModelRequest,
        hooks: &[Arc<dyn BeforeRequest>],
        notices: NoticeSink,
        cancel: &CancellationToken,
    ) -> Result<(ProviderRun, EventStream), ProviderError> {
        let http = self.http().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider is not HTTP-backed"),
        })?;
        if http.entry.family != Family::Cursor {
            return Err(ProviderError::InvalidRequest {
                message: String::from("open_run serves the cursor family only"),
            });
        }
        if http.entry.transport == Transport::Websocket {
            return Err(ProviderError::InvalidRequest {
                message: String::from("this provider family has no WebSocket transport"),
            });
        }
        request_mod::ensure_request(http, request)?;
        if let Some(blob_id) = request_mod::unresolved_blob(request) {
            return Err(ProviderError::UnresolvedBlob { blob_id });
        }
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
        let bearer = Self::cursor_bearer(&credential, &http.entry.id)?;
        let wire = Self::cursor_wire(http, request);
        // Admission before the handshake: a parked run never blocks catalog.
        let permit = transport_mod::acquire_provider_permit(&owner, &http.entry.id, cancel).await?;
        let conversation_id = cursor_lower::uuid_text();
        let message_id = cursor_lower::uuid_text();
        let open = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Transport {
                family: Family::Cursor,
                reason: String::from("provider request cancelled before it started"),
            }),
            open = cursor_decode::open_run(cursor_decode::OpenArgs {
                client: &http.client,
                base_url: &http.entry.base_url,
                bearer: &bearer,
                provider: &http.entry.id,
                user_agent: &http.user_agent,
                request,
                wire: &wire,
                conversation_id,
                message_id,
            }) => open?,
        };
        let mut run = ProviderRun::new(Family::Cursor, open.events, open.transport);
        // `permit` drops here on `next_step` misuse, so no step ever runs
        // without admission.
        let step = run.next_step()?;
        Ok((run, transport_mod::hold_permit(step, permit)))
    }

    /// Answers a parked Cursor run and returns its next step stream.
    ///
    /// Acquires a generation permit before calling `respond`/`next_step`; the
    /// step keeps it through its `Stop` or error via `hold_permit`. Awaiting
    /// holds no permit, so cancellation while acquiring leaves the run
    /// untouched (still awaiting) for its owner to retry or release. The
    /// result supplier runs only after admission succeeds. Cancellation while
    /// waiting does not consume the caller's results. The supplier returns
    /// exactly the pending calls in pending-step order under their step-event
    /// names; `respond` re-verifies the exact cover and
    /// loses the run on mismatch. No hooks, notices, thinking, or refresh
    /// run here: the connection is already open.
    ///
    /// The run owner is the only caller; it never calls `next_step`/`respond`
    /// directly, so every generation step passes through admission.
    ///
    /// # Errors
    /// Returns `InvalidRequest` for a non-Cursor provider or a misuse (no
    /// pending calls); `RunLost` for a cover mismatch or a dead transport;
    /// `Transport` when admission is closed or cancelled before `respond`.
    pub async fn resume_run(
        &self,
        run: &mut ProviderRun,
        results: impl FnOnce() -> Result<Vec<HostToolResult>, ProviderError> + Send,
        cancel: &CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let http = self.http().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider is not HTTP-backed"),
        })?;
        if http.entry.family != Family::Cursor {
            return Err(ProviderError::InvalidRequest {
                message: String::from("resume_run serves the cursor family only"),
            });
        }
        let owner = http
            .owner
            .upgrade()
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: String::from("provider host has ended"),
            })?;
        let permit = transport_mod::acquire_provider_permit(&owner, &http.entry.id, cancel).await?;
        run.respond(results()?)?;
        let step = run.next_step()?;
        Ok(transport_mod::hold_permit(step, permit))
    }

    /// Single-step Cursor `open`: fresh run, first step owned until its end.
    ///
    /// Reuses [`Provider::open_run`] (no duplicated validation pipeline) and
    /// ties the run's lifetime to the returned stream so a live run is never
    /// dropped before its `Stop`. After a tool-use stop the owned run is
    /// released with the stream; it is never resumed from this path.
    async fn open_cursor_single(
        &self,
        session: SessionId,
        request: &ModelRequest,
        hooks: &[Arc<dyn BeforeRequest>],
        notices: NoticeSink,
        cancel: &CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let (run, first) = self
            .open_run(session, request, hooks, notices, cancel)
            .await?;
        Ok(Self::own_until_end(first, run))
    }

    /// Holds `run` alive until `first` ends: the stream owns the run.
    ///
    /// The unfold state retains the run across polls; the terminal `Stop` or
    /// error is delivered first, then the next poll observes the inner end
    /// and drops both. Early drop aborts the step (released) and drops the
    /// run once (no-op after). No task is spawned; permits stay with the
    /// inner `hold_permit` stream.
    fn own_until_end(first: EventStream, run: ProviderRun) -> EventStream {
        let source = futures::stream::unfold((Some(run), first), |(run, mut stream)| async move {
            let item = stream.next().await?;
            Some((item, (run, stream)))
        });
        EventStream::new(source, || {})
    }

    /// Cursor bearer material: API key or OAuth access token, never invented.
    fn cursor_bearer(credential: &Credential, provider: &str) -> Result<String, ProviderError> {
        match credential {
            Credential::ApiKey { key } => Ok(key.expose().to_owned()),
            Credential::OAuth(oauth) => Ok(oauth.access_token.expose().to_owned()),
            Credential::None => Err(ProviderError::NoCredentials {
                provider: provider.to_owned(),
            }),
        }
    }

    /// Resolves the Cursor wire route for one request.
    ///
    /// The wire carries two identities: the routed base id and the
    /// account-usable slug, plus reasoning parameters and the max-mode flag.
    /// Effort rides the requested model id as a trailing tier
    /// (`gpt-5.5-high`), mirroring the reference resolver: for OpenAI-class
    /// bases the tier is stripped into a `reasoning` parameter and the base
    /// id is routed, while other ids pass through unchanged. A bare
    /// `composer-2.5` pins the Standard tier (`fast=false`); its `-fast`
    /// selection declares no parameter. Without discovery rows the max-mode
    /// flag falls back to the wire slug: an `xhigh`/`max` tier or the
    /// Opus fast lane.
    fn cursor_wire(http: &Http, request: &ModelRequest) -> cursor_lower::WireModel {
        let wire_id = request_mod::request_model(request);
        let display = http.resolved.entry.display.clone();
        if wire_id == "composer-2.5" {
            return cursor_lower::WireModel {
                model_id: wire_id.into(),
                details_id: wire_id.into(),
                display_name: display,
                parameters: vec![("fast".into(), "false".into())],
                max_mode: Self::cursor_max_mode(wire_id, None),
            };
        }
        match Self::cursor_effort_split(wire_id) {
            Some((base, effort)) if Self::is_cursor_openai_base(&base) => {
                if effort == "none" {
                    return cursor_lower::WireModel {
                        model_id: base.into(),
                        details_id: wire_id.into(),
                        display_name: display,
                        parameters: Vec::new(),
                        max_mode: Self::cursor_max_mode(wire_id, None),
                    };
                }
                cursor_lower::WireModel {
                    model_id: base.clone().into(),
                    details_id: wire_id.into(),
                    display_name: display,
                    parameters: vec![("reasoning".into(), effort.clone().into())],
                    max_mode: Self::cursor_max_mode(wire_id, Some(&effort)),
                }
            }
            _ => cursor_lower::WireModel {
                model_id: wire_id.into(),
                details_id: wire_id.into(),
                display_name: display,
                parameters: Vec::new(),
                max_mode: Self::cursor_max_mode(wire_id, None),
            },
        }
    }

    /// Splits a trailing effort tier off a Cursor model id.
    ///
    /// Returns the base id with its lane kept (`gpt-5.5-high-fast` keeps
    /// `-fast`) and the tier (`minimal|low|medium|high|xhigh|max|none`).
    /// Matching is ASCII case-insensitive; the base keeps its original bytes.
    fn cursor_effort_split(wire_id: &str) -> Option<(String, String)> {
        const TIERS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max", "none"];
        const LANE: &str = "-fast";
        let lower = wire_id.to_ascii_lowercase();
        let mut rest = lower.as_str();
        let mut lane = "";
        if let Some(trimmed) = rest.strip_suffix(LANE) {
            lane = LANE;
            rest = trimmed;
        }
        for tier in TIERS {
            let suffix = format!("-{tier}");
            if let Some(base) = rest.strip_suffix(suffix.as_str()) {
                if base.is_empty() {
                    continue;
                }
                let original_base = &wire_id[..base.len()];
                return Some((format!("{original_base}{lane}"), (*tier).to_owned()));
            }
        }
        None
    }

    /// Whether a Cursor base id is OpenAI-class: a `gpt-` prefix followed by
    /// an ASCII version digit.
    fn is_cursor_openai_base(base: &str) -> bool {
        let bytes = base.as_bytes();
        bytes.len() > 4 && bytes[..4].eq_ignore_ascii_case(b"gpt-") && bytes[4].is_ascii_digit()
    }

    /// Falls back the max-mode flag to the wire slug when no discovery row
    /// carries a marker: an `xhigh`/`max` tier or the Opus fast lane.
    fn cursor_max_mode(wire_id: &str, effort: Option<&str>) -> bool {
        if matches!(effort, Some("xhigh" | "max")) {
            return true;
        }
        let lower = wire_id.to_ascii_lowercase();
        lower.contains("opus") && lower.contains("-fast")
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
            Family::Chat
            | Family::Anthropic
            | Family::Bedrock
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
            | Family::PiMessages => {
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
        let owner_for_attempt = Arc::clone(owner);
        let attempt = move |credential: &Credential, attempt_cancel: &CancellationToken| {
            let ctx = ctx_for_attempt.clone();
            let credential = credential.clone();
            let attempt_cancel = attempt_cancel.clone();
            let owner = Arc::clone(&owner_for_attempt);
            Box::pin(request_mod::stream_attempt(
                ctx,
                credential,
                attempt_cancel,
                owner,
            ))
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
        // The subprocess family has no model catalog to fetch: its one
        // configured row is the display projection.
        if let Self::GeminiCli(http) = self {
            let owner = http
                .owner
                .upgrade()
                .ok_or_else(|| ProviderError::InvalidRequest {
                    message: String::from("provider host has ended"),
                })?;
            return Ok(vec![request_mod::model_info(
                &http.resolved.entry,
                http.entry.family,
                owner.config.thinking,
            )]);
        }
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
        if matches!(self, Self::GeminiCli(_)) {
            return Err(UsageCheckReason::Transport {
                reason: String::from("account usage checks are available only for Codex"),
            });
        }
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
    fn check_compact_preconditions(
        http: &Http,
        request: &ModelRequest,
    ) -> Result<Option<CompactOutcome>, ProviderError> {
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
        // A row's compaction capability describes the plain endpoint it was
        // built for. An Azure- or Copilot-routed entry sends its turns
        // through its own adapter (deployment routing, `api-version`, the
        // identity headers), and no remote compaction request is built for
        // either, so the plain `/responses/compact` call would reach the
        // wrong surface: history compacts locally instead.
        if request_mod::is_azure_entry(&http.entry) || request_mod::is_copilot_entry(&http.entry) {
            return Ok(Some(CompactOutcome::Unsupported));
        }
        if http.entry.family == Family::Codex && !matches!(&http.credential, Credential::OAuth(_)) {
            return Err(ProviderError::InvalidRequest {
                message: format!("{} needs an OAuth credential", http.entry.id),
            });
        }
        Ok(None)
    }

    /// Compacts one session's context remotely through the provider when the
    /// family supports it, returning the outcome. `None` means the caller
    /// should compact locally.
    ///
    /// # Errors
    /// Returns `InvalidRequest` when this provider is not HTTP-backed or the
    /// request is not a compact-purpose request, `UnresolvedBlob` when the
    /// request still carries unresolved content, and `Provider` on wire or
    /// remote failures.
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
        // The subprocess family has no remote-compaction endpoint.
        if let Self::GeminiCli(http) = self {
            request_mod::ensure_request(http, request)?;
            if request.purpose != Purpose::Compact {
                return Err(ProviderError::InvalidRequest {
                    message: String::from("remote compaction needs a compact-purpose request"),
                });
            }
            return Ok(Some(CompactOutcome::Unsupported));
        }
        let http = self.http().ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from("provider is not HTTP-backed"),
        })?;
        if let Some(outcome) = Self::check_compact_preconditions(http, request)? {
            return Ok(Some(outcome));
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
