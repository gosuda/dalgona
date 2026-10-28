//! The Factory Droid family: one proxy, one `WorkOS` credential, four wire routes.
//!
//! Factory's LLM proxy multiplexes four wire protocols behind one host and one
//! `WorkOS` bearer token. The per-model wire is the route table ([`wire_for`]).
//! Three routes reuse the shared body encoders and stream decoders of
//! [`crate::family::chat`], [`crate::family::responses`], and
//! [`crate::family::anthropic`]; the Gemini route owns its `generateContent`
//! body and decoder ([`gemini`]) because no shared family speaks it. What is
//! Droid's own is the auth, the identity headers, the system-prefix contract,
//! the session attribution ids, and the proxy lifecycle ([`open`]). A shared
//! wire is never lowered twice.
//!
//! Ground truth is the pinned oh-my-pi source at `a4dc3302`:
//! `packages/ai/src/providers/factory-droid.ts`, `factory-droid/gemini.ts`,
//! `factory-droid/droid-identity.md`, the route table `api-routes
//! provider="factory-droid"` in `compat/rules/runtime/behavior.kdl`, and
//! `packages/catalog/src/wire/factory-droid.ts`. senpi `b8c1d3a8` holds no
//! Droid provider. The contract row is "Factory Droid and GitLab Duo" of the
//! provider-contract table: shared model APIs do not erase provider-specific
//! authentication or lifecycle rules.
//!
//! # Auth
//!
//! `authorization: Bearer <WorkOS access token>`, obtained by the device-code
//! sign-in. Factory API keys are control-plane only and answer 403 on the
//! inference proxy, so [`require_bearer`] refuses an API key before any
//! request exists. An OAuth credential is the only accepted kind.
//!
//! # Session lifecycle
//!
//! One dal session maps to one stable Droid session UUID ([`session_uuid`])
//! and one upstream router (`x-api-provider`). The upstream locks for the
//! session once an assistant turn exists (`x-provider-routing-source:
//! session_lock`); the assistant-message id rotates per
//! turn. The session UUID also rides the prompt cache key of the completions
//! and responses routes.
//!
//! # Replay binding
//!
//! An assistant entry of a Droid turn is journaled under [`Family::Droid`]
//! and the model id. The shared encoders bind replay data to their own family,
//! so [`derived_request`] rebinds those entries to the route's encoder family
//! before lowering, and [`open`] rebinds the family of every replay payload a
//! decoder emits back to [`Family::Droid`]. The model check stays exact, so
//! reasoning never replays to another model.

mod gemini;
mod schema;
#[cfg(test)]
mod tests;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use base64::Engine as _;
use dal_core::{ContextItem, Family, ModelRequest, ModelRoute, RawJson, ReplaySource, SessionId};
use futures::{Stream, StreamExt, future, stream::BoxStream};
use tokio_util::sync::CancellationToken;

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

/// The provider id every Droid failure names.
pub(crate) const PROVIDER_ID: &str = "factory-droid";

/// The Droid system sentence that precedes the caller's system text,
/// verbatim from `factory-droid/droid-identity.md`. The proxy gates on its
/// presence: a bare system channel has historically served 403.
const DROID_SYSTEM_PREFIX: &str =
    "You are Droid, an AI software engineering agent built by Factory.";

/// The client version the proxy's SDK fingerprints expect.
const CLIENT_VERSION: &str = "0.237.0";

/// The Node build the Droid CLI reports in its `X-Stainless` headers.
const RUNTIME_VERSION: &str = "v26.3.0";

/// The chat-completions builder of the CLI pins this temperature on every
/// body; only an explicit request temperature displaces it.
const COMPLETIONS_TEMPERATURE: f32 = 1.0;

/// The `X-Stainless` package versions of the two SDK wires.
const STAINLESS_ANTHROPIC: &str = "0.70.1";
const STAINLESS_OPENAI: &str = "6.25.0";

/// The beta that streams Anthropic tool input eagerly.
const STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";

/// The beta that pairs Anthropic output effort with its field.
const EFFORT_BETA: &str = "effort-2025-11-24";

/// The Droid Anthropic client pins this request timeout, in seconds, in its
/// telemetry header.
const ANTHROPIC_TIMEOUT_HEADER: &str = "600";

/// Upstream routers the proxy dispatches to, sent as `x-api-provider`.
const UPSTREAMS: [&str; 14] = [
    "fireworks",
    "baseten",
    "mistral",
    "anthropic",
    "azure_anthropic",
    "vertex_anthropic",
    "bedrock_anthropic",
    "openai",
    "azure_openai",
    "bedrock_openai",
    "google",
    "xai",
    "snowflake",
    "databricks",
];

/// How a request reaches the proxy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Wire {
    /// `POST /api/llm/o/v1/chat/completions`: Kimi, GLM, `DeepSeek`, Qwen,
    /// `MiniMax`, Mistral, Nemotron, and every id the table does not list.
    Completions,
    /// `POST /api/llm/o/v1/responses`: GPT and Grok.
    Responses,
    /// `POST /api/llm/a/v1/messages`: Claude and the Anthropic-shaped rows.
    Anthropic,
    /// `POST /api/llm/g/v1/generate`: Gemini, native `generateContent` SSE.
    Gemini,
}

impl Wire {
    /// The path joined after [`Wire::namespace`].
    const fn path(self) -> &'static str {
        match self {
            Self::Completions => "chat/completions",
            Self::Responses => "responses",
            Self::Anthropic => "v1/messages",
            Self::Gemini => "generate",
        }
    }

    /// The proxy namespace of the route, joined after the host.
    const fn namespace(self) -> &'static str {
        match self {
            Self::Completions | Self::Responses => "api/llm/o/v1",
            Self::Anthropic => "api/llm/a",
            Self::Gemini => "api/llm/g/v1",
        }
    }
}

/// The route table: model id to proxy wire, from the pinned
/// `api-routes provider="factory-droid"` rows. An id the table does not list
/// falls back to [`Wire::Completions`], as the pinned registry does.
const ROUTES: &[(&str, Wire)] = &[
    ("claude-fable-5.1", Wire::Anthropic),
    ("claude-fable-5", Wire::Anthropic),
    ("claude-opus-5-5", Wire::Anthropic),
    ("claude-opus-5-5-fast", Wire::Anthropic),
    ("claude-opus-5", Wire::Anthropic),
    ("claude-opus-5-fast", Wire::Anthropic),
    ("claude-opus-4-8", Wire::Anthropic),
    ("claude-opus-4-8-fast", Wire::Anthropic),
    ("claude-opus-4-7", Wire::Anthropic),
    ("claude-opus-4-6", Wire::Anthropic),
    ("claude-opus-4-5-20251101", Wire::Anthropic),
    ("claude-sonnet-5-5", Wire::Anthropic),
    ("claude-sonnet-5", Wire::Anthropic),
    ("claude-sonnet-4-6", Wire::Anthropic),
    ("claude-sonnet-4-5-20250929", Wire::Anthropic),
    ("claude-haiku-5-5", Wire::Anthropic),
    ("claude-haiku-4-5-20251001", Wire::Anthropic),
    ("atlas-07-21", Wire::Anthropic),
    ("aster-07-15", Wire::Anthropic),
    ("minimax-m2.7", Wire::Anthropic),
    ("gpt-6-astra", Wire::Responses),
    ("gpt-6.1-sol", Wire::Responses),
    ("gpt-6-sol", Wire::Responses),
    ("gpt-6-luna", Wire::Responses),
    ("gpt-5.6-sol", Wire::Responses),
    ("gpt-5.6-sol-fast", Wire::Responses),
    ("gpt-5.6-terra", Wire::Responses),
    ("gpt-5.6-luna", Wire::Responses),
    ("gpt-5.5", Wire::Responses),
    ("gpt-5.5-fast", Wire::Responses),
    ("gpt-5.5-pro", Wire::Responses),
    ("gpt-5.4", Wire::Responses),
    ("gpt-5.4-fast", Wire::Responses),
    ("gpt-5.4-mini", Wire::Responses),
    ("gpt-5.4-mini-fast", Wire::Responses),
    ("gpt-5.3-codex", Wire::Responses),
    ("gpt-5.3-codex-fast", Wire::Responses),
    ("gpt-5.2", Wire::Responses),
    ("grok-4.7", Wire::Responses),
    ("grok-4.6", Wire::Responses),
    ("grok-4.5", Wire::Responses),
    ("garnet-07-15", Wire::Gemini),
    ("gemini-3.1-pro-preview", Wire::Gemini),
    ("gemini-3.8-flash", Wire::Gemini),
    ("gemini-3.7-flash", Wire::Gemini),
    ("gemini-3.6-flash", Wire::Gemini),
    ("gemini-3.5-flash", Wire::Gemini),
    ("gemini-3-flash-preview", Wire::Gemini),
    ("inkling", Wire::Completions),
    ("mistral-large-4", Wire::Completions),
    ("mistral-medium-3.5", Wire::Completions),
    ("glm-5.3-flash", Wire::Completions),
    ("glm-5.3", Wire::Completions),
    ("glm-5.2", Wire::Completions),
    ("glm-5.2-fast", Wire::Completions),
    ("kimi-k3", Wire::Completions),
    ("qwen3.8-max", Wire::Completions),
    ("nemotron-3-ultra", Wire::Completions),
    ("deepseek-v4.1-flash", Wire::Completions),
    ("deepseek-v4-flash-0731", Wire::Completions),
    ("deepseek-v4-pro", Wire::Completions),
    ("minimax-m3", Wire::Completions),
];

/// The proxy wire of one model id.
#[must_use]
pub(crate) fn wire_for(model: &str) -> Wire {
    ROUTES
        .iter()
        .find(|(id, _)| *id == model)
        .map_or(Wire::Completions, |(_, wire)| *wire)
}

/// How the chat-completions route encodes the reasoning switch, from the
/// pinned `completions-reasoning-mode` matrix
/// ([`ReasoningDialect::for_completions`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReasoningDialect {
    /// `reasoning_effort` carries the planned effort, `none` when it is off.
    Effort,
    /// The route cannot turn thinking off: off maps to the lowest rung.
    ForcedOn,
    /// A toggle-only dialect: `chat_template_args.enable_thinking` is the
    /// only control and `reasoning_effort` is omitted.
    OptIn,
    /// The route takes no reasoning member at all.
    Omit,
}

impl ReasoningDialect {
    /// The dialect of one completions model on one upstream.
    ///
    /// The completions roster defaults to no reasoning member; the
    /// `fireworks`, `baseten`, and `databricks` upstreams speak the effort
    /// field, as does `mistral` for its routed models. `nemotron-3-ultra` on
    /// `baseten` is toggle-only and `deepseek` on `baseten` cannot turn
    /// thinking off.
    #[must_use]
    fn for_completions(model: &str, upstream: &str) -> Self {
        if model == "nemotron-3-ultra" && upstream == "baseten" {
            return Self::OptIn;
        }
        if model.starts_with("deepseek-") && upstream == "baseten" {
            return Self::ForcedOn;
        }
        if matches!(upstream, "fireworks" | "baseten" | "databricks") {
            return Self::Effort;
        }
        if upstream == "mistral"
            && (model.starts_with("glm-")
                || model.starts_with("mistral-")
                || model == "mistral-medium-3.5"
                || model == "mistral-large-4")
        {
            return Self::Effort;
        }
        Self::Omit
    }
}

/// The per-model route facts the catalog row supplies.
#[derive(Clone, Copy, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent route capability flags mirrored from catalog row"
)]
pub(crate) struct RoutePolicy<'a> {
    /// Whether the Responses model supports reasoning summaries.
    pub(crate) reasoning_summary: bool,
    /// Whether the Anthropic model supports the adaptive display hint.
    pub(crate) display_supported: bool,
    /// Whether the Anthropic route streams tool input eagerly: the
    /// `fine-grained-tool-streaming-2025-05-14` beta rides with tools.
    pub(crate) anthropic_tool_streaming_beta: bool,
    /// Whether the Anthropic route pairs output effort with the
    /// `effort-2025-11-24` beta.
    pub(crate) anthropic_effort_beta: bool,
    /// The account's `OpenAI-Platform` organization hint, sent on the
    /// `openai` and `azure_openai` upstreams.
    pub(crate) openai_platform: Option<&'a str>,
    /// Whether the Responses route asks for a 24 h prompt cache retention.
    pub(crate) responses_cache_24h: bool,
    /// Whether the Responses route sends the session safety identifier.
    pub(crate) responses_safety_identifier: bool,
    /// The Responses text verbosity the route requests, when any.
    pub(crate) responses_verbosity: Option<&'a str>,
    /// The Responses service tier the route requests, when any.
    pub(crate) responses_service_tier: Option<&'a str>,
    /// Whether the Gemini route accepts the `MEDIUM` thinking level.
    pub(crate) gemini_medium: bool,
    /// Whether the route locks its upstream from the first turn on.
    pub(crate) routing_locked: bool,
    /// The model's output ceiling: the Anthropic `max_tokens` bound and the
    /// hard cap of a requested Responses output cap.
    pub(crate) max_output: Option<u32>,
}

/// Everything one Droid request needs.
///
/// It has no `Debug` implementation because it carries the bearer.
#[derive(Clone, Copy)]
pub(crate) struct DroidRequest<'a> {
    /// The neutral request; its route is `Family::Droid` and the model id.
    pub(crate) request: &'a ModelRequest,
    /// The composed thinking plan; its fragment matches the route's family.
    pub(crate) thinking: &'a ThinkingPlan,
    /// The proxy host; the registration join's provider row defaults it to
    /// `https://api.factory.ai`.
    pub(crate) base_url: &'a str,
    /// The upstream router pinned for this model, one of [`UPSTREAMS`].
    pub(crate) upstream: &'a str,
    /// The dal session; it maps to the stable Droid session UUID.
    pub(crate) session: SessionId,
    /// The per-turn assistant-message UUID.
    pub(crate) request_id: &'a str,
    /// The `WorkOS` access token, from [`require_bearer`].
    pub(crate) bearer: &'a str,
    /// The catalog-row route facts.
    pub(crate) policy: &'a RoutePolicy<'a>,
}

/// One lowered Droid request.
///
/// It has no `Debug` implementation because its headers carry the bearer.
#[must_use]
pub(crate) struct DroidWire {
    /// The route the model resolved to.
    pub(crate) route: Wire,
    /// The full proxy URL.
    pub(crate) url: url::Url,
    /// Headers in send order; `authorization` is first.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The `user-agent`; `http::send` writes it over any header.
    pub(crate) user_agent: String,
    /// The exact body bytes.
    pub(crate) body: Vec<u8>,
    /// The wire model id, for the decoder and error mapping.
    pub(crate) model: Box<str>,
    /// The Anthropic replay prefix the decoder binds signed thinking to.
    pub(crate) replay_prefix: Option<Box<str>>,
    /// The raw token, for redacting server messages.
    pub(crate) secret: Box<str>,
}

/// The `WorkOS` bearer of a Droid credential, or the refusal.
///
/// # Errors
///
/// [`ProviderError::NoCredentials`] without a usable credential and
/// [`ProviderError::InvalidRequest`] with an API key, which the inference
/// proxy answers with 403.
pub(crate) fn require_bearer(credential: &Credential) -> Result<&str, ProviderError> {
    match credential {
        Credential::OAuth(oauth) if !oauth.access_token.expose().trim().is_empty() => {
            Ok(oauth.access_token.expose())
        }
        Credential::ApiKey { .. } => Err(ProviderError::InvalidRequest {
            message: String::from(
                "Factory API keys do not serve the Droid inference proxy; run dalgon login factory-droid.",
            ),
        }),
        Credential::OAuth(_) | Credential::None => Err(ProviderError::NoCredentials {
            provider: String::from(PROVIDER_ID),
        }),
    }
}

/// The stable Droid session UUID of a dal session.
///
/// The proxy expects v4-shaped ids and the dal id is a `UUIDv7`, so the version
/// and variant bits are rewritten; the result is a pure function of the
/// session, as the pinned `deterministicUuid` is.
#[must_use]
fn session_uuid(session: SessionId) -> String {
    let mut shaped = *session.as_uuid().as_bytes();
    shaped[6] = (shaped[6] & 0x0f) | 0x40;
    shaped[8] = (shaped[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(shaped).to_string()
}

/// The `external_org_id` claim of a `WorkOS` access token, read unverified; the
/// server verifies the token and the claim only fills `x-factory-org-id`.
#[must_use]
fn org_id_from_token(bearer: &str) -> Option<String> {
    use sonic_rs::JsonValueTrait;
    let segment = bearer.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .ok()?;
    let claims: sonic_rs::Value = sonic_rs::from_slice(&decoded).ok()?;
    let org = claims.get("external_org_id")?.as_str()?;
    (!org.is_empty()).then(|| String::from(org))
}

/// The fixed failure when the `whoami` endpoint answers outside 2xx.
///
/// The text never carries the response body, the bearer, or the
/// organization: a hostile body cannot leak through the login failure.
const VERIFY_DENIED: &str =
    "Factory sign-in verification failed; the Droid service refused the session.";

/// The fixed failure when a 2xx `whoami` answer resolves no organization.
///
/// Verbatim from the pinned reference (`attachFactoryDroidRegion`), minus
/// its dynamic status interpolation, which rides the error variant instead.
const VERIFY_UNRESOLVED: &str = "Factory login did not resolve an organization.";

/// Verifies one `WorkOS` bearer against the Factory session endpoint.
///
/// Sends `GET <base_url>/api/cli/whoami` with the bearer, `Accept:
/// application/json` (via [`Exchange::Json`]), and `x-factory-org-id` when
/// [`org_id_from_token`] reads one from the token. Success is the pinned
/// reference semantics, not just HTTP 200: the answer must be 2xx and carry
/// a resolvable organization, either the body's `orgId` or the token's
/// `external_org_id` claim. The helper owns no persistence and reads no
/// ambient state; the callers own the login validate-before-persist and the
/// refresh advisory policies.
///
/// The user agent is the crate default ([`http::user_agent`] over
/// compile-time constants), the same construction as the sign-in flow's.
/// The transport bound is [`http::OAUTH_TIMEOUT`], the reference 15 s
/// budget. Cancellation propagates: a `cancel` that fires before or during
/// the call ends it, and dropping the select drops the in-flight request.
///
/// # Errors
///
/// [`ProviderError::LoginCancelled`] when `cancel` fires, including a
/// transport failure that races cancellation; [`ProviderError::Status`]
/// with a fixed message when the endpoint answers outside 2xx;
/// [`ProviderError::TokenExchange`] with a fixed message when a 2xx answer
/// resolves no organization. Other transport failures propagate
/// family-attributed and redacted of the bearer. No error carries the
/// response body, the token, or the organization.
pub(crate) async fn verify_session(
    client: &reqwest::Client,
    base_url: &str,
    bearer: &str,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    if cancel.is_cancelled() {
        return Err(ProviderError::LoginCancelled);
    }
    let url = http::endpoint(Family::Droid, base_url, "api/cli/whoami")?;
    let mut request = client
        .get(url)
        .header("authorization", format!("Bearer {bearer}"));
    if let Some(org) = org_id_from_token(bearer) {
        request = request.header("x-factory-org-id", org);
    }
    let user_agent = crate::http::user_agent(
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        "unknown",
        std::env::consts::ARCH,
    );
    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        result = http::send(
            Family::Droid,
            request,
            &user_agent,
            Exchange::Json {
                total: http::OAUTH_TIMEOUT,
            },
            tokio::time::sleep,
        ) => result.map_err(|error| cancel_or(error, cancel, bearer))?,
    };
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(ProviderError::Status {
            family: Family::Droid,
            status,
            message: String::from(VERIFY_DENIED),
        });
    }
    let body = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProviderError::LoginCancelled),
        result = http::read_body(Family::Droid, response) => result.map_err(|error| cancel_or(error, cancel, bearer))?,
    };
    if cancel.is_cancelled() {
        return Err(ProviderError::LoginCancelled);
    }
    if resolved_org(&body, bearer).is_some() {
        Ok(())
    } else {
        Err(ProviderError::TokenExchange {
            status,
            message: String::from(VERIFY_UNRESOLVED),
        })
    }
}

/// A transport failure of the `whoami` call, redacted of the bearer.
///
/// `http` reasons omit the URL, but the bearer rides the request headers;
/// scrubbing keeps a hostile proxy message from echoing it back through a
/// failure the login flow stores or displays.
fn redacted(mut error: ProviderError, bearer: &str) -> ProviderError {
    redact_provider_error(&mut error, &[Box::<str>::from(bearer)]);
    error
}

/// A `whoami` transport failure, or [`ProviderError::LoginCancelled`] when
/// the failure raced a fired `cancel`.
///
/// The reference checks the abort signal in its catch before anything else,
/// so a failure that arrives alongside cancellation reports cancellation;
/// the refresh owner can then tell cancel from failure.
fn cancel_or(error: ProviderError, cancel: &CancellationToken, bearer: &str) -> ProviderError {
    if cancel.is_cancelled() {
        ProviderError::LoginCancelled
    } else {
        redacted(error, bearer)
    }
}

/// The organization a `whoami` answer resolves: the body's `orgId` when it
/// is a nonempty string, else the token's `external_org_id` claim.
///
/// This is the pinned reference rule (`attachFactoryDroidRegion`): a login
/// succeeds only when one of the two yields an organization, so a 2xx
/// answer alone never verifies a session. A body that is not JSON resolves
/// nothing even when the token carries a claim, because the reference
/// rethrows the parse failure on login; a body that parses but is not a
/// record still falls back to the claim.
#[must_use]
fn resolved_org(body: &[u8], bearer: &str) -> Option<String> {
    let value: sonic_rs::Value = sonic_rs::from_slice(body).ok()?;
    body_org(&value).or_else(|| org_id_from_token(bearer))
}

/// The nonempty `orgId` member of one parsed `whoami` body, if any.
#[must_use]
fn body_org(value: &sonic_rs::Value) -> Option<String> {
    use sonic_rs::JsonValueTrait;
    let org = value.get("orgId")?.as_str()?;
    (!org.is_empty()).then(|| String::from(org))
}

/// The Droid system text: the identity sentence, one newline, then the
/// caller's text unchanged.
#[must_use]
fn droid_system(system: &str) -> String {
    if system.is_empty() {
        return String::from(DROID_SYSTEM_PREFIX);
    }
    format!("{DROID_SYSTEM_PREFIX}\n{system}")
}

/// Lowers one request to its URL, headers, and body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the route is not a Droid route, the
/// upstream is unknown, the thinking plan belongs to another family, a body
/// encoder rejects the request, or an extra member is not valid JSON;
/// [`ProviderError::Transport`] when the proxy URL does not build.
pub(crate) fn build_wire(input: &DroidRequest<'_>) -> Result<DroidWire, ProviderError> {
    let ModelRoute::Api {
        family: Family::Droid,
        model,
        ..
    } = &input.request.model
    else {
        return Err(invalid("a Droid request needs a factory-droid model route"));
    };
    if !UPSTREAMS.contains(&input.upstream) {
        return Err(invalid(format!(
            "unknown Droid upstream `{}`",
            input.upstream
        )));
    }
    let route = wire_for(model);
    let lowered = match route {
        Wire::Completions => lower_completions(input)?,
        Wire::Responses => lower_responses(input)?,
        Wire::Anthropic => lower_anthropic(input)?,
        Wire::Gemini => Lowered::plain(gemini::body(input)?),
    };
    let base = format!(
        "{}/{}",
        input.base_url.trim_end_matches('/'),
        route.namespace()
    );
    let url = http::endpoint(Family::Droid, &base, route.path())?;
    let mut headers = vec![("authorization", format!("Bearer {}", input.bearer))];
    headers.extend(identity_headers(input, route));
    headers.extend(lowered.headers);
    Ok(DroidWire {
        route,
        url,
        headers,
        user_agent: format!("factory-cli/{CLIENT_VERSION}"),
        body: lowered.body,
        model: model.clone(),
        replay_prefix: lowered.prefix,
        secret: Box::from(input.bearer),
    })
}

/// One route's lowered body with the headers its family encoder owns.
struct Lowered {
    body: Vec<u8>,
    headers: Vec<(&'static str, String)>,
    prefix: Option<Box<str>>,
}

impl Lowered {
    fn plain(body: Vec<u8>) -> Self {
        Self {
            body,
            headers: Vec::new(),
            prefix: None,
        }
    }
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

/// The request one shared encoder lowers: the route's family and the wire
/// model id replace the Droid route, the system text carries the Droid
/// prefix, and the session UUID is the cache key.
///
/// The tool list is shared. The context is shared as well unless an assistant
/// entry journaled under [`Family::Droid`] needs its replay binding moved to
/// the encoder family, which is the one case that copies it.
fn derived_request(input: &DroidRequest<'_>, family: Family) -> ModelRequest {
    let request = input.request;
    let rebinds = request.context.iter().any(
        |item| matches!(item, ContextItem::Assistant { source, .. } if source.family == Family::Droid),
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
            model: Box::from(request.model.id()),
            provider: None,
        },
        system: Arc::from(droid_system(&request.system)),
        tools: Arc::clone(&request.tools),
        context,
        params: request.params.clone(),
        cache_key: Some(Box::from(session_uuid(input.session))),
    }
}

fn rebound(item: &ContextItem, family: Family) -> ContextItem {
    match item {
        ContextItem::Assistant {
            source,
            parts,
            replay,
        } if source.family == Family::Droid => ContextItem::Assistant {
            source: ReplaySource {
                family,
                model: source.model.clone(),
            },
            parts: parts.clone(),
            replay: replay.clone(),
        },
        other => other.clone(),
    }
}

fn lower_completions(input: &DroidRequest<'_>) -> Result<Lowered, ProviderError> {
    let WireThinking::OpenAi { effort } = input.thinking.wire else {
        return Err(foreign_thinking());
    };
    let model = input.request.model.id();
    let (effort, enable_thinking) = dialect(
        ReasoningDialect::for_completions(model, input.upstream),
        effort,
    );
    let plan = ThinkingPlan {
        level: input.thinking.level,
        wire: WireThinking::OpenAi { effort },
        temperature: input.thinking.temperature.or(Some(COMPLETIONS_TEMPERATURE)),
        notices: Vec::new(),
    };
    let derived = derived_request(input, Family::Chat);
    let body = chat::request_body(&derived, &plan, chat::Dialect::LEGACY_TOKENS)
        .map_err(|error| invalid(error.to_string()))?;
    let toggle = enable_thinking.map(|on| {
        (
            "chat_template_args",
            if on {
                r#"{"enable_thinking":true}"#
            } else {
                r#"{"enable_thinking":false}"#
            },
        )
    });
    Ok(Lowered::plain(splice_members(body, toggle)?))
}

/// The `reasoning_effort` and the `enable_thinking` toggle one dialect sends
/// for the planned effort. A planned effort of `none` or none at all is off.
fn dialect(
    dialect: ReasoningDialect,
    planned: Option<&'static str>,
) -> (Option<&'static str>, Option<bool>) {
    let on = planned.filter(|effort| *effort != "none");
    match dialect {
        ReasoningDialect::Effort => (planned, None),
        ReasoningDialect::ForcedOn => (on.or(Some("low")), None),
        ReasoningDialect::OptIn => (None, Some(on.is_some())),
        ReasoningDialect::Omit => (None, None),
    }
}

fn lower_responses(input: &DroidRequest<'_>) -> Result<Lowered, ProviderError> {
    let derived = derived_request(input, Family::Responses);
    // Native Droid sends no output cap on `gpt-*` Responses requests (the
    // pinned native traffic, and `omit-max-output-tokens` for `gpt-*` in the
    // pinned compat rules) and sends `max_output_tokens` on `grok-*`.
    let cap = if derived.model.id().starts_with("gpt-") {
        responses::OutputCap::Omit
    } else {
        responses::OutputCap::Honor {
            ceiling: input.policy.max_output,
        }
    };
    let mut body = responses::request_body(
        &derived,
        input.thinking.wire,
        input.policy.reasoning_summary,
        responses::BodyPolicy {
            output_cap: cap,
            tool_schemas: crate::family::schema::ToolSchemas::Verbatim,
        },
    )?;
    // The proxy declares every Responses tool non-strict and the shared
    // encoder writes no `strict` member, so the field is spliced into each
    // top-level tool object after lowering.
    body = mark_tools_non_strict(body)?;
    if input.policy.responses_cache_24h {
        body = splice_members(body, Some(("prompt_cache_retention", r#""24h""#)))?;
    }
    if input.policy.responses_safety_identifier {
        let identifier = sonic_rs::to_string(&session_uuid(input.session))
            .map_err(|error| invalid(error.to_string()))?;
        body = splice_members(body, Some(("safety_identifier", &identifier)))?;
    }
    if let Some(verbosity) = input.policy.responses_verbosity {
        let level = sonic_rs::to_string(verbosity).map_err(|error| invalid(error.to_string()))?;
        body = splice_members(body, Some(("text", &format!(r#"{{"verbosity":{level}}}"#))))?;
    }
    if let Some(tier) = input.policy.responses_service_tier {
        let tier = sonic_rs::to_string(tier).map_err(|error| invalid(error.to_string()))?;
        body = splice_members(body, Some(("service_tier", &tier)))?;
    }
    Ok(Lowered::plain(body))
}

/// Writes `"strict":false` into every top-level object of the body's
/// `tools` array.
///
/// The member scan is JSON-aware: braces inside strings and escaped
/// characters never change the depth, so a schema that mentions braces
/// cannot misdirect the splice.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the `tools` member is present but
/// one of its elements is not an object or the array never closes.
fn mark_tools_non_strict(body: Vec<u8>) -> Result<Vec<u8>, ProviderError> {
    let Some(array) = member_array(&body, "tools") else {
        return Ok(body);
    };
    let mut closers: Vec<usize> = Vec::new();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut element_start = 0_usize;
    let mut element_is_object = false;
    let mut index = array;
    while index < body.len() {
        let byte = body[index];
        if in_string {
            escaped = !escaped && byte == b'\\';
            in_string = escaped || byte != b'"';
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => {
                    if depth == 0 && byte == b'{' {
                        element_is_object = true;
                        element_start = index;
                    }
                    depth += 1;
                }
                b'}' | b']' => {
                    if depth == 0 {
                        if byte == b']' {
                            break;
                        }
                        return Err(invalid("the tools member is not an array"));
                    }
                    depth -= 1;
                    if depth == 0 && byte == b'}' {
                        if !element_is_object {
                            return Err(invalid("a tools element is not an object"));
                        }
                        closers.push(index);
                        element_is_object = false;
                    }
                }
                b',' if depth == 0 => {
                    if element_is_object {
                        return Err(invalid("a tools element is not an object"));
                    }
                    element_start = index + 1;
                }
                _ => {
                    if depth == 0
                        && !byte.is_ascii_whitespace()
                        && !element_is_object
                        && looks_like_value_start(&body, element_start, index)
                    {
                        return Err(invalid("a tools element is not an object"));
                    }
                }
            }
        }
        index += 1;
    }
    if depth != 0 {
        return Err(invalid("the tools member never closes"));
    }
    let mut spliced = body;
    for closer in closers.into_iter().rev() {
        let empty = is_empty_object(&spliced, closer);
        let member = if empty {
            r#""strict":false"#
        } else {
            r#","strict":false"#
        };
        spliced.splice(closer..closer, member.bytes());
    }
    Ok(spliced)
}

/// The index just past the `[` that opens the top-level `member` array, or
/// `None` when the body carries no such member.
fn member_array(body: &[u8], member: &str) -> Option<usize> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0_usize;
    while index < body.len() {
        let byte = body[index];
        if in_string {
            escaped = !escaped && byte == b'\\';
            in_string = escaped || byte != b'"';
            index += 1;
            continue;
        }
        match byte {
            b'"' => {
                let start = index + 1;
                let mut end = start;
                while end < body.len() && body[end] != b'"' {
                    end += if body[end] == b'\\' { 2 } else { 1 };
                }
                index = end + 1;
                if depth != 1 || &body[start..end] != member.as_bytes() {
                    continue;
                }
                if let Some(array) = member_array_value(body, end + 1) {
                    return Some(array);
                }
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        index += 1;
    }
    None
}

/// The index just past the `[` that opens the array value of an
/// already-matched member key, or `None` when the key's value is not an
/// array. The colon between the key and its value is skipped here.
fn member_array_value(body: &[u8], at: usize) -> Option<usize> {
    let colon = skip_value_index(body, at)?;
    if body[colon] != b':' {
        return None;
    }
    let open = skip_value_index(body, colon + 1)?;
    (body[open] == b'[').then_some(open + 1)
}

/// The index of the first non-whitespace byte at or after `at`.
fn skip_value_index(body: &[u8], at: usize) -> Option<usize> {
    body.iter()
        .enumerate()
        .skip(at)
        .find(|(_, byte)| !byte.is_ascii_whitespace())
        .map(|(index, _)| index)
}

/// Whether the object closing at `closer` holds no members.
fn is_empty_object(body: &[u8], closer: usize) -> bool {
    body[..closer]
        .iter()
        .rev()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'{')
}

/// Whether a non-whitespace scalar already started the current element.
fn looks_like_value_start(body: &[u8], element_start: usize, index: usize) -> bool {
    body[element_start..index]
        .iter()
        .any(|byte| !byte.is_ascii_whitespace())
}

fn lower_anthropic(input: &DroidRequest<'_>) -> Result<Lowered, ProviderError> {
    let WireThinking::Anthropic { thinking, effort } = input.thinking.wire else {
        return Err(foreign_thinking());
    };
    let derived = derived_request(input, Family::Anthropic);
    let lowered = anthropic::build(
        &AnthropicRequest {
            request: &derived,
            max_output: input.policy.max_output,
            thinking,
            effort,
            display_supported: input.policy.display_supported,
            temperature: input.thinking.temperature,
            compaction: None,
            summarize: false,
        },
        AnthropicAuth::Bearer(input.bearer),
    )?;
    // The family builder signs with the bearer it was given; only its
    // non-auth headers (the beta list) survive, because the Droid identity
    // set below owns authorization and the version header.
    let mut headers: Vec<(&'static str, String)> = lowered
        .headers
        .into_iter()
        .filter(|(name, _)| !matches!(*name, "authorization" | "x-api-key" | "anthropic-version"))
        .collect();
    let mut betas: Vec<&str> = Vec::new();
    if input.policy.anthropic_tool_streaming_beta && !derived.tools.is_empty() {
        betas.push(STREAMING_BETA);
    }
    if input.policy.anthropic_effort_beta && effort.is_some() {
        betas.push(EFFORT_BETA);
    }
    merge_betas(&mut headers, &betas);
    Ok(Lowered {
        body: lowered.body,
        headers,
        prefix: Some(lowered.prefix),
    })
}

/// Appends beta names to the single `anthropic-beta` header, creating it
/// when the family builder sent none, without duplicating a name.
fn merge_betas(headers: &mut Vec<(&'static str, String)>, betas: &[&str]) {
    if betas.is_empty() {
        return;
    }
    match headers
        .iter_mut()
        .find(|(name, _)| *name == "anthropic-beta")
    {
        Some((_, value)) => {
            for beta in betas {
                if !value.split(',').any(|listed| listed == *beta) {
                    value.push(',');
                    value.push_str(beta);
                }
            }
        }
        None => headers.push(("anthropic-beta", betas.join(","))),
    }
}

fn foreign_thinking() -> ProviderError {
    invalid("the thinking plan does not belong to the Droid route")
}

/// Splices one top-level member into a JSON object body, after its last
/// member. The value is validated as JSON and written as given.
fn splice_members(
    mut body: Vec<u8>,
    extra: Option<(&'static str, &str)>,
) -> Result<Vec<u8>, ProviderError> {
    let Some((key, json)) = extra else {
        return Ok(body);
    };
    let value = RawJson::parse(json)
        .map_err(|error| invalid(format!("body member `{key}` is not JSON: {error}")))?;
    let key = sonic_rs::to_string(key).map_err(|error| invalid(error.to_string()))?;
    if body.pop() != Some(b'}') {
        return Err(invalid("a family body is not a JSON object"));
    }
    if body.last() != Some(&b'{') {
        body.push(b',');
    }
    body.extend_from_slice(key.as_bytes());
    body.push(b':');
    body.extend_from_slice(value.as_str().as_bytes());
    body.push(b'}');
    Ok(body)
}

/// The identity headers every Droid wire sends after `authorization`, then
/// the wire's own set.
fn identity_headers(input: &DroidRequest<'_>, route: Wire) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("x-client-version", String::from(CLIENT_VERSION)),
        ("x-factory-client", String::from("cli")),
    ];
    if let Some(org) = org_id_from_token(input.bearer) {
        headers.push(("x-factory-org-id", org));
    }
    headers.push(("x-api-provider", String::from(input.upstream)));
    headers.push((
        "x-provider-routing-source",
        String::from(routing_source(input)),
    ));
    headers.push(("x-session-id", session_uuid(input.session)));
    headers.push(("x-assistant-message-id", String::from(input.request_id)));
    if let Some(platform) = input.policy.openai_platform {
        headers.push(("openai-platform", String::from(platform)));
    }
    headers.extend(sdk_headers(route));
    headers
}

/// `session_lock` once an assistant turn exists or the route locks its
/// upstream from the start; `registry_default` before.
fn routing_source(input: &DroidRequest<'_>) -> &'static str {
    let served = input
        .request
        .context
        .iter()
        .any(|item| matches!(item, ContextItem::Assistant { .. }));
    if served || input.policy.routing_locked {
        "session_lock"
    } else {
        "registry_default"
    }
}

/// The `X-Stainless` fingerprint of the SDK behind each wire. The Gemini
/// client adds nothing beyond the shared identity set.
fn sdk_headers(route: Wire) -> Vec<(&'static str, String)> {
    let package = match route {
        Wire::Gemini => return Vec::new(),
        Wire::Anthropic => STAINLESS_ANTHROPIC,
        Wire::Completions | Wire::Responses => STAINLESS_OPENAI,
    };
    let mut headers = Vec::new();
    if route == Wire::Anthropic {
        headers.push((
            "anthropic-version",
            String::from(anthropic::ANTHROPIC_VERSION),
        ));
        // The Anthropic SDK demands a key header; the bearer stays the
        // credential the proxy reads.
        headers.push(("x-api-key", String::from("placeholder")));
    }
    headers.extend([
        ("x-stainless-lang", String::from("js")),
        ("x-stainless-package-version", String::from(package)),
        ("x-stainless-runtime", String::from("node")),
        ("x-stainless-runtime-version", String::from(RUNTIME_VERSION)),
        ("x-stainless-retry-count", String::from("0")),
    ]);
    if route == Wire::Anthropic {
        headers.push((
            "x-stainless-timeout",
            String::from(ANTHROPIC_TIMEOUT_HEADER),
        ));
    }
    headers
}

// --- The proxy lifecycle -------------------------------------------------

/// Sends one Droid request and decodes its stream.
///
/// A non-success response becomes the [`AttemptFailure::Response`] the
/// request lifecycle classifies, except a region refusal, which fails with
/// the pinned explanation and no retry. A success decodes through the route's
/// decoder; a replay payload is rebound to [`Family::Droid`]. A body that
/// stops reading mid-stream ends as a transport failure, never as a clean cut.
/// Dropping the stream drops the response and cancels the transfer.
///
/// # Errors
///
/// [`AttemptFailure`] for a transport failure before the response head or a
/// rejected response.
pub(crate) async fn open(
    client: &reqwest::Client,
    wire: DroidWire,
) -> Result<EventStream, AttemptFailure> {
    let DroidWire {
        route,
        url,
        headers,
        user_agent,
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
    let response = http::send(
        Family::Droid,
        request.body(body),
        &user_agent,
        Exchange::Stream,
        tokio::time::sleep,
    )
    .await?;
    if !response.status().is_success() {
        return Err(rejected(response, &model, &secrets).await);
    }
    let read_failed = Arc::new(AtomicBool::new(false));
    let chunks = body_chunks(response.bytes_stream(), Arc::clone(&read_failed));
    let events = sse::decode_stream(chunks);
    let decoded = decode_route(route, events, &model, replay_prefix.as_deref());
    let guarded = decoded.map(move |item| match item {
        Err(_) if read_failed.load(Ordering::Acquire) => {
            let mut error = ProviderError::Transport {
                family: Family::Droid,
                reason: String::from("Droid response body read failed"),
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
    bytes.scan(failed, move |failed, chunk| {
        let next = if let Ok(bytes) = chunk {
            Some(bytes.to_vec())
        } else {
            failed.store(true, Ordering::Release);
            None
        };
        future::ready(next)
    })
}

/// The route's decoder over the SSE events of one response.
fn decode_route<S>(
    route: Wire,
    events: S,
    model: &str,
    replay_prefix: Option<&str>,
) -> BoxStream<'static, Result<StreamEvent, ProviderError>>
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static,
{
    match route {
        Wire::Completions => {
            chat::decode_events(events, chat::ChatDecoder::new(PROVIDER_ID, model)).boxed()
        }
        Wire::Responses => {
            responses::decode(events, Family::Responses, Box::<str>::from(model)).boxed()
        }
        Wire::Anthropic => {
            anthropic::decode_stream(events, model.into(), false, replay_prefix).boxed()
        }
        Wire::Gemini => gemini::decode(events, model).boxed(),
    }
}

/// Binds a decoded replay payload to [`Family::Droid`], the family the turn
/// is journaled under.
fn rebind_replay(event: StreamEvent) -> StreamEvent {
    match event {
        StreamEvent::Replay { payload } => StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Droid,
                ..payload
            },
        },
        other => other,
    }
}

/// The failure of a non-success response.
async fn rejected(
    response: reqwest::Response,
    model: &str,
    secrets: &[Box<str>],
) -> AttemptFailure {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(String::from);
    let body = match http::read_body(Family::Droid, response).await {
        Ok(body) => body,
        Err(error) => return AttemptFailure::Provider(error),
    };
    let text = redact(&String::from_utf8_lossy(&body), secrets).into_owned();
    let (code, message) = error_fields(&text);
    if let Some(explained) = region_unavailable(model, &message) {
        return AttemptFailure::Provider(invalid(explained));
    }
    AttemptFailure::Response {
        status,
        code,
        message,
        retry_after,
    }
}

/// The error code and message of a proxy error body. A body that is not a
/// JSON object is the message itself.
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

/// The pinned explanation of a region refusal; the catalog never persists an
/// exclusion from it.
fn region_unavailable(model: &str, message: &str) -> Option<String> {
    message
        .to_ascii_lowercase()
        .contains("not available in this region")
        .then(|| format!("{model} is not served from your network's region. Choose another model."))
}
