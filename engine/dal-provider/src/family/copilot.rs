//! GitHub Copilot inference over the Chat, Responses, and Anthropic codecs:
//! proxy authentication, the Copilot client identity, base-URL resolution, and
//! model-to-surface mapping.
//!
//! Copilot is a proxy, not a wire family. One account serves three request
//! surfaces behind one host, chosen *by model id*: Claude ids speak Anthropic
//! Messages, the GPT-5/GPT-6/Grok-4/`oswe`/`mai` rosters speak Responses, and
//! everything else speaks Chat Completions ([`surface_for_model`], the pinned
//! oh-my-pi `api-routes provider="github-copilot"` rule). Request bodies are the
//! unmodified codec bodies ([`chat::request_body`], [`responses::request_body`],
//! [`anthropic::build`]); this module owns only what Copilot adds:
//!
//! 1. **Credential.** The key is a GitHub token, stored as an API-key credential
//!    that is either the raw token or the structured envelope
//!    `{"token":..,"enterpriseUrl":..,"apiEndpoint":..}` ([`CopilotKey::parse`],
//!    oh-my-pi `parseGitHubCopilotApiKey`). It travels as `authorization: Bearer`.
//! 2. **Host.** [`resolve_base_url`]: the account's `apiEndpoint`, then the host
//!    named by a session token's `proxy-ep` (senpi `githubCopilotBaseUrlFromToken`),
//!    then `https://copilot-api.<enterprise domain>`, then
//!    `https://api.githubcopilot.com`. A configured base that is not a
//!    `githubcopilot.com` host (a corporate proxy) always wins. A request sent to
//!    another plan's host answers `421 Misdirected Request` ([`failure_note`]).
//! 3. **Identity headers.** [`identity_headers`] mirrors oh-my-pi
//!    `buildCopilotDynamicHeaders`: `editor-version`, `copilot-integration-id`,
//!    `copilot-harness-id`, `openai-intent`, `x-github-api-version`,
//!    `x-initiator` (`agent` when the last context item is not a user message),
//!    `x-interaction-type`, and `copilot-vision-request` for image input.
//!    3a. **Identity decision.** The constants below impersonate the
//!    Copilot CLI exactly as the pinned reference does, including the
//!    `user-agent` override ([`USER_AGENT`]). Identity markers remain
//!    a user decision; swapping the constants is the whole change if
//!    the identity decision changes.
//! 4. **Integration retry.** Business organisations gate premium models per
//!    client surface. Chat traffic starts as `copilot-chat` (enterprise
//!    credentials as `copilot-developer-cli`), and a denial (`403`, or `400`
//!    `model_not_supported`, [`identity_denied`]) is resent once with the other
//!    identity ([`Integration::fallback`], or [`Integration::fallback_learned`]
//!    when the denied identity came from the cache). The identity that worked is
//!    learned per credential and host in [`IdentityCache`]; an explicit pin
//!    ([`Integration::pinned`]) is never second-guessed.
//! 5. **Model wire ids.** `<id>-1m` rows are local long-context siblings that
//!    send their base id ([`wire_model_id`]); replay stays bound to the local
//!    catalog id, exactly as for Azure deployments.
//! 6. **Connection-bound history.** Copilot answers `401 input item does not
//!    belong to this connection` for Responses items from an earlier
//!    connection (oh-my-pi `connection-bound-native-history`). Responses bodies
//!    therefore carry no reasoning replay ([`without_native_history`]);
//!    reasoning continuity is the cost of never sending an item the proxy may
//!    refuse.
//! 7. **Model policy.** Claude and Grok ids must be enabled per account once
//!    (`POST <base>/models/<id>/policy`, [`enable_model_policy`]), the sign-in
//!    epilogue of the reference.
//! 8. **Failure text.** Copilot quota codes ([`is_quota_failure`]) are not in the
//!    shared retry table, and a bare `403`/`421` carries no body text
//!    ([`failure_note`]). [`failure_error`] maps both onto `ProviderError`; a
//!    quota failure is never an identity denial.
//! 9. **One identity negotiation per request.** [`IdentityRetry`] owns the
//!    start identity, the single resend, and the cache update, so the transport
//!    only reports statuses.
//! 10. **Strict pairing.** Chat and Responses bodies carry only paired tool
//!     exchanges ([`azure::repair_pairing`]); the Copilot proxy rejects the rest.
//! 11. **Catalog.** [`decode_models`] reads the account's `/models` list into
//!     [`CopilotModel`] rows (windows, output cap, vision, tool support, price
//!     tiers, premium multiplier, the `-1m` siblings, the surface); the join maps
//!     them to catalog entries. [`models_request`] and
//!     [`endpoint_discovery_request`] are the two requests that feed it.
//! 12. **Premium requests.** [`premium_requests`] is the pure count of premium
//!     requests one request costs; the counter itself needs a `Usage` field
//!     (`dal-core`), see the join.
//! 13. **Sign-in.** The device flow's requests, response decoders, poll pacing,
//!     and stored key ([`login_target`], [`device_code_request`],
//!     [`decode_device_code`], [`access_token_request`], [`decode_poll`],
//!     [`Pacing`], [`stored_key`]) are pure; the login owner drives the HTTP and
//!     the timers.
//!
//! # Completed single-ID ownership
//!
//! * Canonical provider: single built-in provider `github-copilot` in `VENDOR_ROWS`
//!   defaulting to `Family::Chat` with `base_url = "https://api.githubcopilot.com"`,
//!   `key_env = "COPILOT_GITHUB_TOKEN"`, and `oauth = None`.
//! * Surface multiplexing: a single provider ID multiplexes all three request
//!   surfaces (Chat, Responses, Anthropic) per-model through [`surface_for_model`].
//!   Speculative extra provider IDs (`github-copilot-responses`, `github-copilot-anthropic`)
//!   are not used.
//! * Credentials: the credential is stored as `Credential::ApiKey` (plain token or
//!   structured JSON envelope) via the GitHub device sign-in or environment key.
//! * Sign-in epilogue: after access token poll and endpoint discovery, the sign-in
//!   epilogue enables models via `enable_model_policy`, and persists the key last.
//! * `provider/request.rs`: a Copilot branch in `build_stream_request` and
//!   `stream_attempt`: [`build`], `wire.endpoint()`, `wire.into_request`, send
//!   with `wire.user_agent`, decode with `transport::decode_response(
//!   wire.surface.family(), provider, &wire.model, false, wire.prefix.as_deref(),
//!   vec![wire.secret.clone()])`. On an `AttemptFailure::Response` that
//!   [`is_quota_failure`] accepts, return `ProviderError::Quota`; on one that
//!   [`identity_denied`] accepts, rebuild with the identity from
//!   [`Integration::fallback`] (or [`Integration::fallback_learned`] when the start
//!   came from [`IdentityCache::get`]) and resend once inside the same attempt,
//!   then `remember` on success or `forget` on failure through [`IdentityCache`].
//! * `ProviderSetInner` owns one [`IdentityCache`] (no process-global state).
//! * Catalog rows: Copilot's chat surface does not take every effort level, so
//!   the row's thinking support decides whether `reasoning_effort` is sent;
//!   `remote_compact` stays false (no `/responses/compact` on Copilot).

use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    fmt,
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dal_core::{AssistantPart, ContextItem, Family, ModelRequest, ModelRoute, Part, RawJson};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sonic_rs::JsonValueTrait;
use url::Url;

use crate::{
    auth::credential::SecretString,
    error::ProviderError,
    family::{
        anthropic::{self, AnthropicAuth, AnthropicRequest},
        azure::{repair_pairing, rewrite_model},
        chat, responses,
    },
    http,
    thinking::{ThinkingPlan, WireThinking},
};

/// The personal-account Copilot API host.
pub(crate) const PERSONAL_BASE_URL: &str = "https://api.githubcopilot.com";

/// The `user-agent` Copilot requests send in place of the dalgon one.
pub(crate) const USER_AGENT: &str = "copilot/1.0.82";

const EDITOR_VERSION: &str = "copilot/1.0.82";
const CLI_INTEGRATION_ID: &str = "copilot-developer-cli";
const CHAT_INTEGRATION_ID: &str = "copilot-chat";
const HARNESS_ID: &str = "copilot-sdk";
const API_VERSION: &str = "2026-08-01";
const COPILOT_DOMAIN: &str = "githubcopilot.com";
const LONG_CONTEXT_SUFFIX: &str = "-1m";
const IDENTITY_CACHE_LIMIT: usize = 50;

const ANTHROPIC_PREFIXES: [&str; 5] = [
    "claude-haiku-",
    "claude-sonnet-",
    "claude-opus-",
    "claude-fable-",
    "claude-mythos-",
];
const RESPONSES_PREFIXES: [&str; 5] = ["grok-4.", "gpt-5", "gpt-6", "oswe", "mai-"];
const PUBLIC_GITHUB_HOSTS: [&str; 3] = ["api.github.com", "github.com", "www.github.com"];

/// The Copilot error codes that report an exhausted plan or spend limit.
const QUOTA_CODES: [&str; 5] = [
    "quota_exceeded",
    "free_quota_exceeded",
    "overage_limit_reached",
    "billing_not_configured",
    "additional_spend_limit_reached",
];

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidRequest {
        message: message.into(),
    }
}

/// The request surface a Copilot model speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Surface {
    /// `POST <base>/chat/completions`.
    Chat,
    /// `POST <base>/responses`.
    Responses,
    /// `POST <base>/v1/messages`.
    Anthropic,
}

impl Surface {
    /// The surface of a route family; every family Copilot does not proxy has
    /// none. Each is named, so a new `Family` variant breaks this match.
    pub(crate) const fn of_family(family: Family) -> Option<Self> {
        match family {
            Family::Chat => Some(Self::Chat),
            Family::Responses => Some(Self::Responses),
            Family::Anthropic => Some(Self::Anthropic),
            Family::Codex
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
            | Family::PiMessages => None,
        }
    }

    /// The family whose codec lowers and decodes this surface.
    pub(crate) const fn family(self) -> Family {
        match self {
            Self::Chat => Family::Chat,
            Self::Responses => Family::Responses,
            Self::Anthropic => Family::Anthropic,
        }
    }

    const fn path(self) -> &'static str {
        match self {
            Self::Chat => chat::ENDPOINT_PATH,
            Self::Responses => responses::PATH,
            Self::Anthropic => anthropic::MESSAGES_PATH,
        }
    }
}

/// The surface a Copilot model id speaks: Anthropic for the Claude prefixes,
/// Responses for the reviewed roster prefixes (the synthesized `-1m` siblings
/// included), Chat for everything else.
pub(crate) fn surface_for_model(model: &str) -> Surface {
    let matches = |prefixes: &[&str]| prefixes.iter().any(|prefix| model.starts_with(prefix));
    if matches(&ANTHROPIC_PREFIXES) {
        Surface::Anthropic
    } else if matches(&RESPONSES_PREFIXES) {
        Surface::Responses
    } else {
        Surface::Chat
    }
}

/// The model id the wire carries: a local `<id>-1m` long-context sibling sends
/// its base id, since the tier is a client-side budget and not a served model.
pub(crate) fn wire_model_id(model: &str) -> &str {
    model
        .strip_suffix(LONG_CONTEXT_SUFFIX)
        .filter(|base| !base.is_empty())
        .unwrap_or(model)
}

/// A Copilot credential: the GitHub token and the account routing it carries.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct CopilotKey {
    token: Box<str>,
    enterprise_domain: Option<Box<str>>,
    api_endpoint: Option<Box<str>>,
}

impl fmt::Debug for CopilotKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopilotKey")
            .field("token", &"<redacted>")
            .field("enterprise_domain", &self.enterprise_domain)
            .field("api_endpoint", &self.api_endpoint)
            .finish()
    }
}

#[derive(Deserialize)]
struct Envelope {
    token: Option<sonic_rs::Value>,
    #[serde(rename = "enterpriseUrl")]
    enterprise_url: Option<sonic_rs::Value>,
    #[serde(rename = "apiEndpoint")]
    api_endpoint: Option<sonic_rs::Value>,
}

impl CopilotKey {
    /// Parses a stored key: the structured envelope when it is a JSON object
    /// with a string `token`, else the whole text as a raw token. Non-string
    /// routing members, public GitHub hosts, and endpoints that are not
    /// credential-free `https` URLs are dropped.
    pub(crate) fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        let envelope = raw
            .starts_with('{')
            .then(|| sonic_rs::from_str::<Envelope>(raw).ok())
            .flatten();
        let token = envelope
            .as_ref()
            .and_then(|envelope| string_member(envelope.token.as_ref()));
        let (Some(envelope), Some(token)) = (envelope, token) else {
            return Self {
                token: raw.into(),
                enterprise_domain: None,
                api_endpoint: None,
            };
        };
        Self {
            token: token.into(),
            enterprise_domain: string_member(envelope.enterprise_url.as_ref())
                .and_then(|domain| enterprise_domain(&domain))
                .map(Into::into),
            api_endpoint: string_member(envelope.api_endpoint.as_ref())
                .and_then(|endpoint| api_endpoint(&endpoint))
                .map(Into::into),
        }
    }

    /// The bearer token.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// The key with `endpoint` as its account endpoint, when it names none and
    /// `endpoint` is a credential-free `https` URL; otherwise unchanged. This is
    /// how a discovered or signed-in endpoint joins the stored key.
    pub(crate) fn with_api_endpoint(mut self, endpoint: &str) -> Self {
        if self.api_endpoint.is_none() {
            self.api_endpoint = api_endpoint(endpoint).map(Into::into);
        }
        self
    }

    /// Refuses a token that cannot be a header value: blank, or holding a
    /// control character.
    fn ensure_usable(&self) -> Result<(), ProviderError> {
        if self.token.trim().is_empty() || self.token.chars().any(char::is_control) {
            return Err(invalid(
                "the GitHub Copilot token is blank or holds a control character",
            ));
        }
        Ok(())
    }

    /// Whether the account is a GitHub Enterprise one.
    pub(crate) fn enterprise(&self) -> bool {
        self.enterprise_domain.is_some()
    }
}

fn string_member(value: Option<&sonic_rs::Value>) -> Option<String> {
    value.and_then(JsonValueTrait::as_str).map(str::to_owned)
}

fn host_of(input: &str) -> Option<String> {
    let url = if input.contains("://") {
        Url::parse(input)
    } else {
        Url::parse(&format!("https://{input}"))
    }
    .ok()?;
    url.host_str().map(str::to_ascii_lowercase)
}

fn enterprise_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let domain = host_of(trimmed).unwrap_or_else(|| trimmed.to_ascii_lowercase());
    (!domain.is_empty() && !PUBLIC_GITHUB_HOSTS.contains(&domain.as_str())).then_some(domain)
}

fn api_endpoint(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if !trimmed.starts_with("https://") {
        return None;
    }
    let url = Url::parse(trimmed).ok()?;
    let plain = url.host_str().is_some() && url.username().is_empty() && url.password().is_none();
    plain.then(|| trimmed.trim_end_matches('/').to_owned())
}

fn is_copilot_host(base: &str) -> bool {
    Url::parse(base)
        .ok()
        .and_then(|url| url.host_str().map(is_copilot_domain))
        .unwrap_or(false)
}

fn is_copilot_domain(host: &str) -> bool {
    host == COPILOT_DOMAIN || host.ends_with(".githubcopilot.com")
}

fn enterprise_base(domain: &str) -> String {
    if domain.starts_with("copilot-api.") {
        format!("https://{domain}")
    } else {
        format!("https://copilot-api.{domain}")
    }
}

/// The host a session token names in its `proxy-ep` field, as an API base.
/// Only a plain `githubcopilot.com` host is accepted, so a crafted token cannot
/// redirect itself to another server.
fn proxy_base(token: &str) -> Option<String> {
    let proxy = token
        .split(';')
        .find_map(|part| part.strip_prefix("proxy-ep="))
        .filter(|host| !host.is_empty())?;
    let host = proxy
        .strip_prefix("proxy.")
        .map_or_else(|| proxy.to_owned(), |rest| format!("api.{rest}"));
    let plain = host.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
    });
    (plain && is_copilot_domain(&host)).then(|| format!("https://{host}"))
}

/// The effective API base for `key` under the `configured` base.
///
/// A configured base outside `githubcopilot.com` (a corporate proxy) is kept.
/// On the Copilot hosts the account decides: its `apiEndpoint`, a session
/// token's `proxy-ep`, the enterprise host, then the personal host. The host
/// test is a parsed-host suffix match, stricter than the pinned `includes`.
pub(crate) fn resolve_base_url(configured: &str, key: &CopilotKey) -> String {
    let configured = configured.trim().trim_end_matches('/');
    if !configured.is_empty() && !is_copilot_host(configured) {
        return configured.to_owned();
    }
    if let Some(endpoint) = &key.api_endpoint {
        return endpoint.to_string();
    }
    if let Some(base) = proxy_base(&key.token) {
        return base;
    }
    if let Some(domain) = &key.enterprise_domain {
        return enterprise_base(domain);
    }
    if configured.is_empty() {
        PERSONAL_BASE_URL.to_owned()
    } else {
        configured.to_owned()
    }
}

/// The `copilot-integration-id` a request announces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Integration {
    /// The chat surface, `copilot-chat`: the default for personal accounts.
    Chat,
    /// The Copilot CLI, `copilot-developer-cli`: the default for enterprise
    /// accounts, whose private endpoint accepts it.
    Cli,
    /// An explicit caller pin, sent verbatim and never retried.
    Pinned(Box<str>),
}

impl Integration {
    /// A validated explicit pin: non-blank and free of control characters, so the
    /// header stays well-formed.
    pub(crate) fn pinned(value: &str) -> Option<Self> {
        let value = value.trim();
        (!value.is_empty() && !value.chars().any(char::is_control))
            .then(|| Self::Pinned(value.into()))
    }

    /// The header value.
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Chat => CHAT_INTEGRATION_ID,
            Self::Cli => CLI_INTEGRATION_ID,
            Self::Pinned(id) => id,
        }
    }

    /// The identity a request starts with: the pin, then the identity learned for
    /// this credential, then the account default (the CLI for an enterprise
    /// account, chat otherwise).
    pub(crate) fn initial(pinned: Option<Self>, learned: Option<Self>, key: &CopilotKey) -> Self {
        pinned.or(learned).unwrap_or(if key.enterprise() {
            Self::Cli
        } else {
            Self::Chat
        })
    }

    /// The identity to resend a request denied under its *default* start, once.
    /// Chat falls back to the CLI; the CLI has no fallback (an enterprise account
    /// that the CLI identity cannot serve has nothing better to try); a pin never
    /// falls back.
    pub(crate) fn fallback(&self) -> Option<Self> {
        match self {
            Self::Chat => Some(Self::Cli),
            Self::Cli | Self::Pinned(_) => None,
        }
    }

    /// The identity to resend a request denied under an identity *learned* from
    /// the cache, once: the other one, because the learned entry is stale (an
    /// organisation policy change); a pin never falls back.
    pub(crate) fn fallback_learned(&self) -> Option<Self> {
        match self {
            Self::Chat => Some(Self::Cli),
            Self::Cli => Some(Self::Chat),
            Self::Pinned(_) => None,
        }
    }
}

/// Who started the request: the user, or the agent continuing after tool output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Initiator {
    /// The last context item is a user message.
    User,
    /// The last context item is an assistant message or a tool result.
    Agent,
}

impl Initiator {
    const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

/// The initiator of `request`; an empty context is a user request.
pub(crate) fn initiator(request: &ModelRequest) -> Initiator {
    match request.context.last() {
        Some(ContextItem::Assistant { .. } | ContextItem::ToolResult { .. }) => Initiator::Agent,
        Some(ContextItem::User { .. }) | None => Initiator::User,
    }
}

fn has_vision_input(request: &ModelRequest) -> bool {
    request.context.iter().any(|item| match item {
        ContextItem::User { parts } | ContextItem::ToolResult { parts, .. } => {
            parts.iter().any(|part| matches!(part, Part::Image { .. }))
        }
        ContextItem::Assistant { .. } => false,
    })
}

/// The Copilot identity headers of `request` under `integration`, in send order.
/// The caller adds `authorization`.
pub(crate) fn identity_headers(
    request: &ModelRequest,
    integration: &Integration,
) -> Vec<(&'static str, String)> {
    let initiator = initiator(request).as_str();
    let mut headers = vec![
        ("editor-version", EDITOR_VERSION.to_owned()),
        ("copilot-integration-id", integration.id().to_owned()),
        ("copilot-harness-id", HARNESS_ID.to_owned()),
        ("openai-intent", "conversation-agent".to_owned()),
        ("x-github-api-version", API_VERSION.to_owned()),
        ("x-initiator", initiator.to_owned()),
        ("x-interaction-type", format!("conversation-{initiator}")),
    ];
    if has_vision_input(request) {
        headers.push(("copilot-vision-request", "true".to_owned()));
    }
    headers
}

/// The request without reasoning replay payloads. Borrowed when it carries none.
fn without_native_history(request: &ModelRequest) -> Cow<'_, ModelRequest> {
    let replays = |parts: &[AssistantPart]| {
        parts.iter().any(|part| {
            matches!(
                part,
                AssistantPart::Thinking {
                    replay: Some(_),
                    ..
                }
            )
        })
    };
    let carries = request.context.iter().any(|item| match item {
        ContextItem::Assistant { parts, .. } => replays(parts),
        ContextItem::User { .. } | ContextItem::ToolResult { .. } => false,
    });
    if !carries {
        return Cow::Borrowed(request);
    }
    let stripped = |part: &AssistantPart| match part {
        AssistantPart::Thinking { text, .. } => AssistantPart::Thinking {
            text: text.clone(),
            replay: None,
        },
        other => other.clone(),
    };
    let context: Vec<ContextItem> = request
        .context
        .iter()
        .map(|item| match item {
            ContextItem::Assistant {
                source,
                replay,
                parts,
            } => ContextItem::Assistant {
                source: source.clone(),
                replay: replay.clone(),
                parts: parts.iter().map(stripped).collect(),
            },
            other => other.clone(),
        })
        .collect();
    let mut owned = request.clone();
    owned.context = context.into();
    Cow::Owned(owned)
}

/// Everything one Copilot body needs.
pub(crate) struct CopilotRequest<'a> {
    /// The neutral request; its route names the local catalog model and the
    /// surface's family.
    pub(crate) request: &'a ModelRequest,
    /// The composed thinking plan for the route's family.
    pub(crate) thinking: &'a ThinkingPlan,
    /// The model's output ceiling, when the catalog knows it (Anthropic).
    pub(crate) max_output: Option<u32>,
    /// Whether the model supports the adaptive thinking display hint (Anthropic).
    pub(crate) display_supported: bool,
    /// Whether the model supports reasoning summaries (Responses).
    pub(crate) reasoning_summary: bool,
    /// The configured base URL.
    pub(crate) base_url: &'a str,
    /// The credential.
    pub(crate) key: &'a CopilotKey,
    /// The identity to announce.
    pub(crate) integration: &'a Integration,
}

/// One lowered Copilot request.
#[must_use]
pub(crate) struct CopilotWire {
    /// The surface, and so the decoder.
    pub(crate) surface: Surface,
    /// The effective base URL.
    pub(crate) base_url: String,
    /// The path relative to `base_url`.
    pub(crate) path: &'static str,
    /// Headers in send order; names are lowercase.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The `user-agent` the request must send instead of the dalgon one.
    pub(crate) user_agent: &'static str,
    /// The JSON body, with the wire model id.
    pub(crate) body: Vec<u8>,
    /// The raw token, for redacting provider-controlled messages.
    pub(crate) secret: Box<str>,
    /// The local catalog model id: the decoder's replay binding.
    pub(crate) model: Box<str>,
    /// The identity this request announced.
    pub(crate) integration: Integration,
    /// The Anthropic replay prefix; `None` on the other surfaces.
    pub(crate) prefix: Option<Box<str>>,
}

impl CopilotWire {
    /// The request URL, through the shared endpoint trust boundary.
    ///
    /// # Errors
    ///
    /// The [`http::endpoint`] failures.
    pub(crate) fn endpoint(&self) -> Result<Url, ProviderError> {
        http::endpoint(self.surface.family(), &self.base_url, self.path)
    }

    /// Attaches the headers and body to a `POST` of `url`.
    pub(crate) fn into_request(
        self,
        client: &reqwest::Client,
        url: Url,
    ) -> reqwest::RequestBuilder {
        let mut request = client.post(url);
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        request.body(self.body)
    }
}

impl fmt::Debug for CopilotWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopilotWire")
            .field("surface", &self.surface)
            .field("base_url", &self.base_url)
            .field("path", &self.path)
            .field("headers", &"<redacted>")
            .field("body_len", &self.body.len())
            .field("model", &self.model)
            .field("integration", &self.integration)
            .finish_non_exhaustive()
    }
}

type Parts = (Vec<(&'static str, String)>, Vec<u8>, Option<Box<str>>);

fn bearer(key: &CopilotKey) -> (&'static str, String) {
    ("authorization", format!("Bearer {}", key.token()))
}

fn chat_parts(input: &CopilotRequest<'_>) -> Result<Parts, ProviderError> {
    let request = repair_pairing(input.request);
    let body =
        chat::request_body(&request, input.thinking).map_err(|error| invalid(error.to_string()))?;
    Ok((vec![bearer(input.key)], body, None))
}

fn responses_parts(input: &CopilotRequest<'_>) -> Result<Parts, ProviderError> {
    let paired = repair_pairing(input.request);
    let request = without_native_history(&paired);
    let body = responses::request_body(&request, input.thinking.wire, input.reasoning_summary)?;
    Ok((vec![bearer(input.key)], body, None))
}

fn anthropic_parts(input: &CopilotRequest<'_>) -> Result<Parts, ProviderError> {
    let WireThinking::Anthropic { thinking, effort } = input.thinking.wire else {
        return Err(invalid("the thinking plan does not belong to Anthropic"));
    };
    let wire = anthropic::build(
        &AnthropicRequest {
            request: input.request,
            max_output: input.max_output,
            thinking,
            effort,
            display_supported: input.display_supported,
            temperature: input.thinking.temperature,
            compaction: None,
            summarize: false,
        },
        AnthropicAuth::Bearer(input.key.token()),
    )?;
    Ok((wire.headers, wire.body, Some(wire.prefix)))
}

/// Lowers one request to its Copilot endpoint, headers, and body.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for a route outside the three surfaces, a
/// thinking plan of the wrong family, or a body the codec rejects.
pub(crate) fn build(input: &CopilotRequest<'_>) -> Result<CopilotWire, ProviderError> {
    let ModelRoute::Api { family, model } = &input.request.model else {
        return Err(invalid("a Copilot request needs an API model route"));
    };
    let Some(surface) = Surface::of_family(*family) else {
        return Err(invalid(
            "GitHub Copilot serves the openai_chat, openai_responses, and anthropic families",
        ));
    };
    input.key.ensure_usable()?;
    let (mut headers, body, prefix) = match surface {
        Surface::Chat => chat_parts(input)?,
        Surface::Responses => responses_parts(input)?,
        Surface::Anthropic => anthropic_parts(input)?,
    };
    headers.extend(identity_headers(input.request, input.integration));
    Ok(CopilotWire {
        surface,
        base_url: resolve_base_url(input.base_url, input.key),
        path: surface.path(),
        headers,
        user_agent: USER_AGENT,
        body: rewrite_model(body, model, wire_model_id(model))?,
        secret: input.key.token.clone(),
        model: model.clone(),
        integration: input.integration.clone(),
        prefix,
    })
}

/// One lowered model-policy request.
#[must_use]
pub(crate) struct PolicyWire {
    /// The effective base URL.
    pub(crate) base_url: String,
    /// The path relative to `base_url`.
    pub(crate) path: String,
    /// Headers in send order; names are lowercase.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The `user-agent` the request must send.
    pub(crate) user_agent: &'static str,
    /// The JSON body.
    pub(crate) body: Vec<u8>,
}

impl PolicyWire {
    /// The request URL, through the shared endpoint trust boundary.
    ///
    /// # Errors
    ///
    /// The [`http::endpoint`] failures.
    pub(crate) fn endpoint(&self) -> Result<Url, ProviderError> {
        http::endpoint(Family::Chat, &self.base_url, &self.path)
    }

    /// Attaches the headers and body to a `POST` of `url`.
    pub(crate) fn into_request(
        self,
        client: &reqwest::Client,
        url: Url,
    ) -> reqwest::RequestBuilder {
        let mut request = client.post(url);
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        request.body(self.body)
    }
}

impl fmt::Debug for PolicyWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolicyWire")
            .field("base_url", &self.base_url)
            .field("path", &self.path)
            .field("headers", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// The request that enables `model` for the account (`POST <base>/models/<id>/policy`
/// with `{"state":"enabled"}`). Claude and Grok ids answer `model_not_supported`
/// until it has succeeded once.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the wire model id is not one path
/// segment of letters, digits, `-`, `_`, or `.`.
pub(crate) fn enable_model_policy(
    configured_base: &str,
    key: &CopilotKey,
    model: &str,
    integration: &Integration,
) -> Result<PolicyWire, ProviderError> {
    key.ensure_usable()?;
    let model = wire_model_id(model);
    let segment = !matches!(model, "" | "." | "..")
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !segment {
        return Err(invalid(format!(
            "copilot model id {model:?} must be one path segment of letters, digits, '-', '_', or '.'"
        )));
    }
    Ok(PolicyWire {
        base_url: resolve_base_url(configured_base, key),
        path: format!("models/{model}/policy"),
        headers: vec![
            bearer(key),
            ("editor-version", EDITOR_VERSION.to_owned()),
            ("copilot-integration-id", integration.id().to_owned()),
            ("copilot-harness-id", HARNESS_ID.to_owned()),
            ("openai-intent", "chat-policy".to_owned()),
            ("x-github-api-version", API_VERSION.to_owned()),
            ("x-initiator", "user".to_owned()),
            ("x-interaction-type", "chat-policy".to_owned()),
        ],
        user_agent: USER_AGENT,
        body: br#"{"state":"enabled"}"#.to_vec(),
    })
}

/// Whether a failed response is a client-identity denial: `403`, or `400` with
/// the code `model_not_supported`. A response whose code is a quota code is an
/// exhausted plan, not a wrong identity, and is never resent.
pub(crate) fn identity_denied(status: u16, code: Option<&str>) -> bool {
    if code.is_some_and(|code| QUOTA_CODES.contains(&code)) {
        return false;
    }
    status == 403 || (status == 400 && code == Some("model_not_supported"))
}

/// Whether a failed response reports an exhausted plan or spend limit: a Copilot
/// quota code, a `402`, or a `429` whose message says `quota exceeded`.
pub(crate) fn is_quota_failure(status: u16, code: Option<&str>, message: &str) -> bool {
    code.is_some_and(|code| QUOTA_CODES.contains(&code))
        || status == 402
        || (status == 429 && message.to_ascii_lowercase().contains("quota exceeded"))
}

/// A plain-language note for the Copilot refusals whose body says little.
pub(crate) fn failure_note(status: u16) -> Option<&'static str> {
    match status {
        421 => Some(
            "GitHub Copilot sent this account to a different API host (HTTP 421 Misdirected \
             Request): Business and Enterprise accounts are served from their own host, which \
             the stored credential must name as its apiEndpoint.",
        ),
        403 => Some(
            "GitHub Copilot refused the request (HTTP 403). Copilot checks access per model and \
             per client; sign in again if the model was only just enabled for the account.",
        ),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct CacheState {
    order: VecDeque<Box<str>>,
    learned: HashMap<Box<str>, Integration>,
}

/// The integration identity that worked, learned per credential and host.
///
/// A process-local, bounded (first in, first out) cache: a stale entry costs at
/// most one retry, which relearns it. The provider host owns it; there is no
/// global instance.
#[derive(Debug, Default)]
pub(crate) struct IdentityCache {
    state: Mutex<CacheState>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl IdentityCache {
    /// The cache key of a credential on an effective host. It holds a digest of
    /// the token, never the token.
    pub(crate) fn key(key: &CopilotKey, base_url: &str) -> Box<str> {
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(key.token.as_bytes()));
        format!(
            "{}\0{}\0{}\0{digest}",
            key.enterprise_domain.as_deref().unwrap_or(""),
            key.api_endpoint.as_deref().unwrap_or(""),
            base_url.trim_end_matches('/').to_ascii_lowercase(),
        )
        .into()
    }

    /// The identity learned for `key`.
    pub(crate) fn get(&self, key: &str) -> Option<Integration> {
        lock(&self.state).learned.get(key).cloned()
    }

    /// Records the identity that cleared the gate; a pin is never recorded.
    pub(crate) fn remember(&self, key: &str, integration: &Integration) {
        if matches!(integration, Integration::Pinned(_)) {
            return;
        }
        let mut state = lock(&self.state);
        if state
            .learned
            .insert(key.into(), integration.clone())
            .is_none()
        {
            state.order.push_back(key.into());
        }
        while state.order.len() > IDENTITY_CACHE_LIMIT {
            if let Some(oldest) = state.order.pop_front() {
                state.learned.remove(&oldest);
            }
        }
    }

    /// Forgets `key`, after a retry that did not clear the gate.
    pub(crate) fn forget(&self, key: &str) {
        let mut state = lock(&self.state);
        state.learned.remove(key);
        state.order.retain(|entry| entry.as_ref() != key);
    }
}

/// One request's identity negotiation: the start identity, the single resend a
/// denial earns, and the cache update that follows.
///
/// The transport reports each failed status to [`IdentityRetry::denied`] and
/// resends once when it returns an identity; after the attempt it calls
/// [`IdentityRetry::succeeded`] or [`IdentityRetry::failed`]. Only a *resent*
/// request touches the cache: a first-try success learns nothing, a resend that
/// worked records its identity, and a resend that failed erases the entry so the
/// next request rediscovers instead of pinning a shape that just failed.
#[derive(Debug)]
pub(crate) struct IdentityRetry {
    cache_key: Box<str>,
    current: Integration,
    learned_start: bool,
    resent: bool,
}

impl IdentityRetry {
    /// Starts a negotiation for `key` under `configured_base`: the pin when set
    /// (and then the cache is not read), else the identity learned for this
    /// credential and host, else the account default.
    pub(crate) fn begin(
        cache: &IdentityCache,
        key: &CopilotKey,
        configured_base: &str,
        pinned: Option<Integration>,
    ) -> Self {
        let cache_key = IdentityCache::key(key, &resolve_base_url(configured_base, key));
        let learned = if pinned.is_some() {
            None
        } else {
            cache.get(&cache_key)
        };
        Self {
            cache_key,
            learned_start: learned.is_some(),
            current: Integration::initial(pinned, learned, key),
            resent: false,
        }
    }

    /// The identity the next attempt announces.
    pub(crate) fn integration(&self) -> &Integration {
        &self.current
    }

    /// Reports a failed response. Returns the identity to resend with when the
    /// response is an identity denial and no resend happened yet; `None`
    /// otherwise, including for a pinned identity.
    pub(crate) fn denied(&mut self, status: u16, code: Option<&str>) -> Option<&Integration> {
        if self.resent || !identity_denied(status, code) {
            return None;
        }
        let fallback = if self.learned_start {
            self.current.fallback_learned()
        } else {
            self.current.fallback()
        };
        let next = fallback?;
        self.current = next;
        self.resent = true;
        Some(&self.current)
    }

    /// Records the end of a successful request.
    pub(crate) fn succeeded(&self, cache: &IdentityCache) {
        if self.resent {
            cache.remember(&self.cache_key, &self.current);
        }
    }

    /// Records the end of a failed request.
    pub(crate) fn failed(&self, cache: &IdentityCache) {
        if self.resent {
            cache.forget(&self.cache_key);
        }
    }
}

/// The `ProviderError` of a failed Copilot response whose meaning the shared
/// retry table lacks: an exhausted plan ([`is_quota_failure`]) is
/// [`ProviderError::Quota`], and `403`/`421` keep their status with
/// [`failure_note`] appended. `None` leaves the response to the shared table.
/// The caller redacts `message` with the wire secret first.
pub(crate) fn failure_error(
    family: Family,
    status: u16,
    code: Option<&str>,
    message: &str,
) -> Option<ProviderError> {
    if is_quota_failure(status, code, message) {
        return Some(ProviderError::Quota {
            message: message.to_owned(),
        });
    }
    let note = failure_note(status)?;
    let message = if message.is_empty() {
        note.to_owned()
    } else {
        format!("{message}\n{note}")
    };
    Some(ProviderError::Status {
        family,
        status,
        message,
    })
}

/// The premium requests one request costs. An agent-initiated request (the
/// model continuing after tool output) costs none; a user-initiated one costs
/// the model's multiplier (one when the row has none or a non-positive value).
pub(crate) fn premium_requests(initiator: Initiator, multiplier: Option<f64>) -> f64 {
    if initiator == Initiator::Agent {
        return 0.0;
    }
    match multiplier {
        None => 1.0,
        Some(value) if !value.is_finite() || value <= 0.0 => 1.0,
        Some(value) => value,
    }
}

/// The price of a model tier, in USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Price {
    /// Input tokens.
    pub(crate) input: f64,
    /// Output tokens.
    pub(crate) output: f64,
    /// Cache-read input tokens.
    pub(crate) cache_read: f64,
    /// Cache-write input tokens, when the tier states it (Haiku overrides).
    pub(crate) cache_write: Option<f64>,
}

/// Named cache rates in USD per million tokens, for the Haiku overrides.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CacheRates {
    read: f64,
    write: f64,
}

const HAIKU_BASE_ID: &str = "claude-haiku-5.5";
const HAIKU_LONG_ID: &str = "claude-haiku-5.5-1m";
const HAIKU_BASE_CACHE: CacheRates = CacheRates {
    read: 0.01,
    write: 0.125,
};
const HAIKU_LONG_CACHE: CacheRates = CacheRates {
    read: 0.05,
    write: 0.625,
};

fn haiku_base_cache(model_id: &str) -> Option<CacheRates> {
    match model_id {
        HAIKU_BASE_ID => Some(HAIKU_BASE_CACHE),
        HAIKU_LONG_ID => Some(HAIKU_LONG_CACHE),
        _ => None,
    }
}

fn haiku_long_cache(model_id: &str) -> Option<CacheRates> {
    match model_id {
        HAIKU_BASE_ID | HAIKU_LONG_ID => Some(HAIKU_LONG_CACHE),
        _ => None,
    }
}

/// One usable model of the account's Copilot catalog.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CopilotModel {
    /// The local catalog id; a long-context sibling ends in `-1m`.
    pub(crate) id: Box<str>,
    /// The id the wire carries ([`wire_model_id`]).
    pub(crate) wire_id: Box<str>,
    /// The display name.
    pub(crate) name: Box<str>,
    /// The request surface of the model ([`surface_for_model`]).
    pub(crate) surface: Surface,
    /// The usable window in tokens, when the row states one.
    pub(crate) context_window: Option<u32>,
    /// The output ceiling in tokens, when the row states one.
    pub(crate) max_output: Option<u32>,
    /// Whether the model takes image input, when the row states it.
    pub(crate) image_input: Option<bool>,
    /// Whether the model supports tool calls, when the row states it.
    pub(crate) tool_calls: Option<bool>,
    /// Premium requests a user-initiated request costs, when the row states it.
    pub(crate) premium_multiplier: Option<f64>,
    /// The token price of this tier, when the row states it.
    pub(crate) price: Option<Price>,
    /// Whether this is the opt-in `-1m` long-context sibling.
    pub(crate) long_context: bool,
}

type Number<'a> = Option<&'a sonic_rs::Value>;

#[derive(Deserialize)]
struct ModelList {
    data: Vec<RawJson>,
}

#[derive(Deserialize)]
struct ModelRow {
    id: String,
    name: Option<String>,
    context_length: Option<sonic_rs::Value>,
    max_completion_tokens: Option<sonic_rs::Value>,
    capabilities: Option<RowCapabilities>,
    billing: Option<RowBilling>,
}

#[derive(Default, Deserialize)]
struct RowCapabilities {
    #[serde(rename = "type")]
    kind: Option<String>,
    limits: Option<RowLimits>,
    supports: Option<RowSupports>,
}

#[derive(Default, Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "mirrors Copilot API capabilities.limits JSON field names"
)]
struct RowLimits {
    max_context_window_tokens: Option<sonic_rs::Value>,
    max_prompt_tokens: Option<sonic_rs::Value>,
    max_output_tokens: Option<sonic_rs::Value>,
    max_non_streaming_output_tokens: Option<sonic_rs::Value>,
}

#[derive(Default, Deserialize)]
struct RowSupports {
    vision: Option<bool>,
    tool_calls: Option<bool>,
}

#[derive(Default, Deserialize)]
struct RowBilling {
    multiplier: Option<sonic_rs::Value>,
    token_prices: Option<RowTokenPrices>,
}

#[derive(Default, Deserialize)]
struct RowTokenPrices {
    default: Option<RowTier>,
    long_context: Option<RowTier>,
}

#[derive(Deserialize)]
struct RowTier {
    max_prompt_tokens: Option<sonic_rs::Value>,
    context_max: Option<sonic_rs::Value>,
    input_price: Option<sonic_rs::Value>,
    output_price: Option<sonic_rs::Value>,
    cache_price: Option<sonic_rs::Value>,
}

/// A token count: a non-negative integer that fits `u32`.
fn count(value: Number<'_>) -> Option<u32> {
    value
        .and_then(JsonValueTrait::as_u64)
        .and_then(|count| u32::try_from(count).ok())
}

/// A finite number.
fn number(value: Number<'_>) -> Option<f64> {
    value
        .and_then(JsonValueTrait::as_f64)
        .filter(|number| number.is_finite())
}

/// One price tier: its prompt ceiling and its price. Prices arrive in hundredths
/// of a dollar per million tokens.
struct Tier {
    context_max: Option<u32>,
    price: Option<Price>,
}

fn wire_cache_read(tier: &RowTier, cache_override: Option<CacheRates>) -> Option<f64> {
    if let Some(rates) = cache_override {
        return Some(rates.read);
    }
    number(tier.cache_price.as_ref())
        .filter(|rate| *rate >= 0.0)
        .map(|rate| rate / 100.0)
}

fn complete_price(tier: &RowTier, cache_override: Option<CacheRates>) -> Option<Price> {
    let input = number(tier.input_price.as_ref()).filter(|rate| *rate >= 0.0)?;
    let output = number(tier.output_price.as_ref()).filter(|rate| *rate >= 0.0)?;
    let cache_read = wire_cache_read(tier, cache_override)?;
    Some(Price {
        input: input / 100.0,
        output: output / 100.0,
        cache_read,
        cache_write: cache_override.map(|rates| rates.write),
    })
}

impl Tier {
    fn read(tier: Option<&RowTier>, cache_override: Option<CacheRates>) -> Self {
        let Some(tier) = tier else {
            return Self {
                context_max: None,
                price: None,
            };
        };
        let context_max = count(tier.max_prompt_tokens.as_ref())
            .or_else(|| count(tier.context_max.as_ref()))
            .filter(|max| *max > 0);
        let price = complete_price(tier, cache_override);
        Self { context_max, price }
    }
}

impl ModelRow {
    /// The usable models of one row: none for a non-chat row, the base model,
    /// and the `-1m` sibling when the row prices a longer tier.
    fn into_models(self) -> Vec<CopilotModel> {
        let capabilities = self.capabilities.unwrap_or_default();
        let chat = capabilities
            .kind
            .as_deref()
            .is_none_or(|kind| kind == "chat");
        if !chat || self.id.is_empty() {
            return Vec::new();
        }
        let limits = capabilities.limits.unwrap_or_default();
        let billing = self.billing.unwrap_or_default();
        let prices = billing.token_prices.unwrap_or_default();
        let (default, long) = (
            Tier::read(prices.default.as_ref(), haiku_base_cache(&self.id)),
            Tier::read(prices.long_context.as_ref(), haiku_long_cache(&self.id)),
        );
        // `max_context_window_tokens` is the total window; `max_prompt_tokens`
        // is only the fallback when no total is stated.
        let full_window = count(limits.max_context_window_tokens.as_ref())
            .or_else(|| count(self.context_length.as_ref()))
            .or_else(|| count(limits.max_prompt_tokens.as_ref()));
        let max_output = count(limits.max_output_tokens.as_ref())
            .or_else(|| count(self.max_completion_tokens.as_ref()))
            .or_else(|| count(limits.max_non_streaming_output_tokens.as_ref()));
        // The served window is the long tier's ceiling; the base row is capped to
        // the default tier, and the long tier is the opt-in sibling. A row that
        // prices a longer tier may report a model-wide prompt budget tighter than
        // the billed default ceiling, and the tighter bound wins.
        let tiered_budget = long
            .context_max
            .and_then(|_| count(limits.max_prompt_tokens.as_ref()))
            .filter(|budget| *budget > 0);
        let default_max = match (tiered_budget, default.context_max) {
            (Some(budget), Some(billed)) if budget < billed => Some(budget),
            (Some(budget), None) => Some(budget),
            (_, billed) => billed,
        };
        let capped = |ceiling: Option<u32>| match (ceiling, full_window, max_output) {
            (Some(cap), Some(window), Some(output)) => Some(window.min(cap.saturating_add(output))),
            _ => None,
        };
        let default_window = capped(default_max).or(full_window);
        let long_window = capped(long.context_max);
        let wire_id: Box<str> = wire_model_id(&self.id).into();
        let id: Box<str> = self.id.into();
        let name: Box<str> = self
            .name
            .filter(|name| !name.trim().is_empty())
            .map_or_else(|| id.clone(), Into::into);
        let base = CopilotModel {
            surface: surface_for_model(&id),
            wire_id,
            name,
            context_window: default_window,
            max_output,
            image_input: capabilities
                .supports
                .as_ref()
                .and_then(|supports| supports.vision),
            tool_calls: capabilities
                .supports
                .as_ref()
                .and_then(|supports| supports.tool_calls),
            premium_multiplier: number(billing.multiplier.as_ref())
                .filter(|multiplier| *multiplier >= 0.0),
            price: default.price,
            long_context: false,
            id,
        };
        let long_price = long.price.or(base.price);
        let sibling = long_window
            .zip(default_window)
            .filter(|(served, default_served)| served > default_served)
            .map(|(served, _)| CopilotModel {
                id: format!("{}{LONG_CONTEXT_SUFFIX}", base.id).into(),
                name: format!("{} (1M)", base.name).into(),
                context_window: Some(served),
                price: long_price,
                long_context: true,
                ..base.clone()
            });
        std::iter::once(base).chain(sibling).collect()
    }
}

/// Reads a Copilot `/models` body into the account's usable chat models.
///
/// Only `capabilities.type` `chat` rows (or rows without a type) are models. A
/// row that is not a model object is skipped, as the reference mapper skips it:
/// one odd row must not hide the whole list. The wire id of a `-1m` sibling is
/// its base id.
///
/// # Errors
///
/// [`ProviderError::Protocol`] when the body is not an object with a `data`
/// array.
pub(crate) fn decode_models(bytes: &[u8]) -> Result<Vec<CopilotModel>, ProviderError> {
    let list: ModelList = sonic_rs::from_slice(bytes).map_err(|error| ProviderError::Protocol {
        family: Family::Chat,
        detail: format!("Copilot model list is not valid: {error}"),
    })?;
    let mut models = Vec::with_capacity(list.data.len());
    for raw in &list.data {
        if let Ok(row) = raw.decode_as::<ModelRow>() {
            models.extend(row.into_models());
        }
    }
    Ok(models)
}

#[derive(Deserialize)]
struct UserInfo {
    endpoints: Option<UserEndpoints>,
}

#[derive(Deserialize)]
struct UserEndpoints {
    api: Option<sonic_rs::Value>,
}

/// The account's plan-specific API endpoint, from the body of
/// `GET https://api.github.com/copilot_internal/user`. `None` when the body does
/// not carry a credential-free `https` endpoint.
pub(crate) fn decode_endpoint(bytes: &[u8]) -> Option<String> {
    let info: UserInfo = sonic_rs::from_slice(bytes).ok()?;
    api_endpoint(info.endpoints?.api?.as_str()?)
}

/// A lowered `GET` request.
#[must_use]
pub(crate) struct GetWire {
    /// The effective base URL.
    pub(crate) base_url: String,
    /// The path relative to `base_url`.
    pub(crate) path: &'static str,
    /// Headers in send order; names are lowercase.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The `user-agent` the request must send.
    pub(crate) user_agent: &'static str,
}

impl GetWire {
    /// The request URL, through the shared endpoint trust boundary.
    ///
    /// # Errors
    ///
    /// The [`http::endpoint`] failures.
    pub(crate) fn endpoint(&self) -> Result<Url, ProviderError> {
        http::endpoint(Family::Chat, &self.base_url, self.path)
    }

    /// Attaches the headers to a `GET` of `url`.
    pub(crate) fn into_request(
        self,
        client: &reqwest::Client,
        url: Url,
    ) -> reqwest::RequestBuilder {
        let mut request = client.get(url);
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        request
    }
}

impl fmt::Debug for GetWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GetWire")
            .field("base_url", &self.base_url)
            .field("path", &self.path)
            .field("headers", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// The account's model list request. Discovery keeps the CLI identity whatever
/// the chat identity is: it unlocks enterprise and experimental models, and
/// listing is not policy-gated the way chat is.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for an unusable token.
pub(crate) fn models_request(
    configured_base: &str,
    key: &CopilotKey,
) -> Result<GetWire, ProviderError> {
    key.ensure_usable()?;
    Ok(GetWire {
        base_url: resolve_base_url(configured_base, key),
        path: "models",
        headers: vec![
            bearer(key),
            ("editor-version", EDITOR_VERSION.to_owned()),
            ("copilot-integration-id", CLI_INTEGRATION_ID.to_owned()),
            ("copilot-harness-id", HARNESS_ID.to_owned()),
            ("openai-intent", "conversation-agent".to_owned()),
            ("x-github-api-version", API_VERSION.to_owned()),
            ("x-initiator", "user".to_owned()),
        ],
        user_agent: USER_AGENT,
    })
}

/// Whether the key still needs its plan endpoint discovered: its effective base
/// is the shared personal host, so a Business or Enterprise account would be
/// misdirected (`421`).
pub(crate) fn needs_endpoint_discovery(configured_base: &str, key: &CopilotKey) -> bool {
    resolve_base_url(configured_base, key) == PERSONAL_BASE_URL
}

/// The endpoint discovery request, `GET https://api.github.com/copilot_internal/user`;
/// decode its body with [`decode_endpoint`] and apply it with
/// [`CopilotKey::with_api_endpoint`].
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] for an unusable token.
pub(crate) fn endpoint_discovery_request(key: &CopilotKey) -> Result<GetWire, ProviderError> {
    key.ensure_usable()?;
    Ok(GetWire {
        base_url: String::from("https://api.github.com"),
        path: "copilot_internal/user",
        headers: vec![
            ("accept", String::from("application/json")),
            ("authorization", format!("token {}", key.token())),
        ],
        user_agent: USER_AGENT,
    })
}

const OPENCODE_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";
const COPILOT_CLI_CLIENT_ID: &str = "Ov23ctDVkRmgkPke0Mmm";
const DEVICE_SCOPE: &str = "read:user";
const LOGIN_USER_AGENT: &str = "copilot-developer-action/0.0.1";
const PUBLIC_GITHUB_LOGIN_HOST: &str = "github.com";

/// The bundled Copilot wire IDs the sign-in epilogue enables per account
/// (`POST <base>/models/<id>/policy`), matching the pinned reference's
/// `getBundledModels("github-copilot").map(model => model.requestModelId ?? model.id)`.
/// The checked-in `models.json` `github-copilot` entry holds exactly these 54 keys
/// with no `requestModelId` overrides (verified programmatically over the whole
/// set), so the wire IDs are the keys. The live `/models` list stays the
/// inference/catalog owner's source; sign-in never fetches it.
pub(crate) const BUNDLED_POLICY_MODEL_IDS: [&str; 54] = [
    "claude-fable-5",
    "claude-fable-5.1",
    "claude-haiku-4.5",
    "claude-haiku-5.5",
    "claude-opus-4.5",
    "claude-opus-4.6",
    "claude-opus-4.7",
    "claude-opus-4.8",
    "claude-opus-5",
    "claude-opus-5.5",
    "claude-sonnet-4",
    "claude-sonnet-4.5",
    "claude-sonnet-4.6",
    "claude-sonnet-5",
    "claude-sonnet-5.5",
    "gemini-2.5-pro",
    "gemini-3-flash-preview",
    "gemini-3-pro-preview",
    "gemini-3.1-pro-preview",
    "gemini-3.5-flash",
    "gemini-3.6-flash",
    "gemini-3.7-flash",
    "gemini-3.8-flash",
    "gpt-4.1",
    "gpt-4o",
    "gpt-5",
    "gpt-5-mini",
    "gpt-5.1",
    "gpt-5.1-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5.3-codex",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.5",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-6-astra",
    "gpt-6-luna",
    "gpt-6-sol",
    "gpt-6.1-sol",
    "grok-4.5",
    "grok-4.6",
    "grok-4.7",
    "grok-code-fast-1",
    "kimi-k2.7-code",
    "kimi-k3",
    "mai-code-1-flash-picker",
    "mai-code-1.1-flash",
    "raptor-mini",
];
const POLL_FLOOR: Duration = Duration::from_secs(1);
const POLL_CEILING: Duration = Duration::from_secs(300);
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
const POLL_FACTOR: f64 = 1.2;
const SLOW_DOWN_FACTOR: f64 = 1.4;

/// Where a sign-in runs: the GitHub host that serves the device flow, and the
/// enterprise domain the stored key must record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoginTarget {
    /// The host of the device-flow endpoints.
    pub(crate) host: Box<str>,
    /// The enterprise domain; `None` on public GitHub.
    pub(crate) enterprise_domain: Option<Box<str>>,
}

/// The sign-in target a user's answer to "GitHub Enterprise URL/domain (blank
/// for github.com)" names. A blank or public-GitHub answer is `github.com`.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the answer names no host.
pub(crate) fn login_target(answer: &str) -> Result<LoginTarget, ProviderError> {
    let answer = answer.trim();
    let public = LoginTarget {
        host: PUBLIC_GITHUB_LOGIN_HOST.into(),
        enterprise_domain: None,
    };
    if answer.is_empty() {
        return Ok(public);
    }
    let Some(host) = host_of(answer) else {
        return Err(invalid("invalid GitHub Enterprise URL or domain"));
    };
    if PUBLIC_GITHUB_HOSTS.contains(&host.as_str()) {
        return Ok(public);
    }
    Ok(LoginTarget {
        enterprise_domain: Some(host.as_str().into()),
        host: host.into(),
    })
}

/// A lowered form `POST`.
#[must_use]
pub(crate) struct FormWire {
    /// The base URL: `https://<host>`.
    pub(crate) base_url: String,
    /// The path relative to `base_url`.
    pub(crate) path: &'static str,
    /// Headers in send order; names are lowercase.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The `user-agent` the request must send.
    pub(crate) user_agent: &'static str,
    /// The `application/x-www-form-urlencoded` body.
    pub(crate) body: String,
}

impl FormWire {
    /// The request URL, through the shared endpoint trust boundary.
    ///
    /// # Errors
    ///
    /// The [`http::endpoint`] failures.
    pub(crate) fn endpoint(&self) -> Result<Url, ProviderError> {
        http::endpoint(Family::Chat, &self.base_url, self.path)
    }

    /// Attaches the headers and body to a `POST` of `url`.
    pub(crate) fn into_request(
        self,
        client: &reqwest::Client,
        url: Url,
    ) -> reqwest::RequestBuilder {
        let mut request = client.post(url);
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        request.body(self.body)
    }
}

impl fmt::Debug for FormWire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FormWire")
            .field("base_url", &self.base_url)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// The OAuth client of a sign-in: public GitHub uses the minimal-grant `OpenCode`
/// app, whose narrow consent keeps organisations that restrict OAuth apps from
/// blocking it; a private instance runs its own OAuth registry and keeps the
/// GitHub-owned Copilot CLI client.
fn client_id(target: &LoginTarget) -> &'static str {
    if target.enterprise_domain.is_some() {
        COPILOT_CLI_CLIENT_ID
    } else {
        OPENCODE_CLIENT_ID
    }
}

fn form(target: &LoginTarget, path: &'static str, pairs: &[(&str, &str)]) -> FormWire {
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        body.append_pair(name, value);
    }
    FormWire {
        base_url: format!("https://{}", target.host),
        path,
        headers: vec![
            ("accept", String::from("application/json")),
            (
                "content-type",
                String::from("application/x-www-form-urlencoded"),
            ),
        ],
        user_agent: LOGIN_USER_AGENT,
        body: body.finish(),
    }
}

/// The device-code request that starts a sign-in.
pub(crate) fn device_code_request(target: &LoginTarget) -> FormWire {
    form(
        target,
        "login/device/code",
        &[("client_id", client_id(target)), ("scope", DEVICE_SCOPE)],
    )
}

/// The access-token request that polls a pending sign-in.
pub(crate) fn access_token_request(target: &LoginTarget, device_code: &str) -> FormWire {
    form(
        target,
        "login/oauth/access_token",
        &[
            ("client_id", client_id(target)),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
    )
}

fn device_error(message: impl Into<String>) -> ProviderError {
    ProviderError::DeviceCode {
        status: 200,
        message: message.into(),
    }
}

/// The device-code response: what the user enters and where.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_field_names,
    reason = "RFC 8628 device authorization response field name"
)]
pub(crate) struct DeviceCode {
    /// The code the poll presents.
    pub(crate) device_code: SecretString,
    /// The code the user enters at the verification page.
    pub(crate) user_code: Box<str>,
    /// The verification page.
    pub(crate) verification_uri: Box<str>,
    /// The poll interval the server asks for.
    pub(crate) interval: Duration,
    /// How long the codes stay valid.
    pub(crate) expires_in: Duration,
}

#[derive(Deserialize)]
struct DeviceCodeBody {
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    interval: Option<u64>,
    expires_in: Option<u64>,
}

/// Reads a device-code response. Every field is required, and the verification
/// page must be an `https` URL, since the user is told to sign in there.
///
/// # Errors
///
/// [`ProviderError::DeviceCode`] for a body that is not valid, lacks a field, or
/// names a page that is not `https`.
pub(crate) fn decode_device_code(bytes: &[u8]) -> Result<DeviceCode, ProviderError> {
    let body: DeviceCodeBody = sonic_rs::from_slice(bytes)
        .map_err(|error| device_error(format!("device code response is not valid: {error}")))?;
    let (Some(device_code), Some(user_code), Some(page), Some(interval), Some(expires_in)) = (
        body.device_code.filter(|code| !code.is_empty()),
        body.user_code.filter(|code| !code.is_empty()),
        body.verification_uri,
        body.interval,
        body.expires_in,
    ) else {
        return Err(device_error("device code response lacks a field"));
    };
    if !page.starts_with("https://") {
        return Err(device_error(
            "device code response names a page that is not https",
        ));
    }
    Ok(DeviceCode {
        device_code: device_code.into(),
        user_code: user_code.into(),
        verification_uri: page.into(),
        interval: Duration::from_secs(interval),
        expires_in: Duration::from_secs(expires_in),
    })
}

/// One answer to an access-token poll.
#[derive(Debug)]
pub(crate) enum Poll {
    /// The user approved; the GitHub token.
    Token(SecretString),
    /// The user has not approved yet.
    Pending,
    /// The server asks for slower polling, naming the new interval when it can.
    SlowDown {
        /// The interval the server asked for.
        interval: Option<Duration>,
    },
}

#[derive(Deserialize)]
struct PollBody {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

/// Reads one access-token poll answer.
///
/// # Errors
///
/// [`ProviderError::DeviceCode`] for a body that is not valid, an error other
/// than `authorization_pending` and `slow_down` (the sign-in is over), or a body
/// with neither a token nor an error.
pub(crate) fn decode_poll(bytes: &[u8]) -> Result<Poll, ProviderError> {
    let body: PollBody = sonic_rs::from_slice(bytes)
        .map_err(|error| device_error(format!("access token response is not valid: {error}")))?;
    if let Some(token) = body.access_token.filter(|token| !token.is_empty()) {
        return Ok(Poll::Token(token.into()));
    }
    match body.error.as_deref() {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown {
            interval: body
                .interval
                .filter(|seconds| *seconds > 0)
                .map(Duration::from_secs),
        }),
        Some(error) => Err(device_error(match body.error_description.as_deref() {
            Some(description) if !description.is_empty() => format!("{error}: {description}"),
            _ => error.to_owned(),
        })),
        None => Err(device_error(
            "access token response has neither a token nor an error",
        )),
    }
}

/// The poll schedule of one sign-in. The wait is the interval times a factor
/// (1.2, then 1.4 once the server has asked to slow down), so the poll never
/// outruns the server's interval; the interval is kept between one second and
/// five minutes, whatever the server sends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pacing {
    interval: Duration,
    factor: f64,
}

impl Pacing {
    /// The schedule for the interval the device-code response asked for.
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval: interval.clamp(POLL_FLOOR, POLL_CEILING),
            factor: POLL_FACTOR,
        }
    }

    /// How long to wait before the next poll.
    pub(crate) fn wait(&self) -> Duration {
        self.interval.mul_f64(self.factor)
    }

    /// Applies a `slow_down` answer: the interval the server named, else five
    /// seconds longer.
    pub(crate) fn slow_down(&mut self, interval: Option<Duration>) {
        self.interval = interval
            .unwrap_or_else(|| self.interval.saturating_add(SLOW_DOWN_STEP))
            .clamp(POLL_FLOOR, POLL_CEILING);
        self.factor = SLOW_DOWN_FACTOR;
    }
}

#[derive(Serialize)]
struct StoredKey<'a> {
    token: &'a str,
    #[serde(rename = "enterpriseUrl", skip_serializing_if = "Option::is_none")]
    enterprise_url: Option<&'a str>,
    #[serde(rename = "apiEndpoint", skip_serializing_if = "Option::is_none")]
    api_endpoint: Option<&'a str>,
}

/// The API-key text a finished sign-in stores: the structured envelope
/// [`CopilotKey::parse`] reads. A GitHub token never expires and never
/// refreshes, so it is stored as an API key (the Chat and Responses families
/// accept nothing else), carrying the enterprise domain and the account
/// endpoint the requests need.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the envelope cannot be written.
pub(crate) fn stored_key(
    token: &SecretString,
    enterprise_domain: Option<&str>,
    api_endpoint: Option<&str>,
) -> Result<SecretString, ProviderError> {
    sonic_rs::to_string(&StoredKey {
        token: token.expose(),
        enterprise_url: enterprise_domain,
        api_endpoint,
    })
    .map(SecretString::from)
    .map_err(|error| invalid(format!("the Copilot key did not serialize: {error}")))
}

#[cfg(test)]
mod tests {
    use dal_core::{
        CallId, ModelToolSpec, Purpose, RawJson, ReplaySource, RequestParams, ThinkingLevel,
    };

    use futures::{StreamExt, executor::block_on, stream};

    use super::*;
    use crate::{
        sse::decode_stream as sse_events,
        stream::{StopReason, StreamEvent, ToolArgs},
        thinking::AnthropicThinking,
    };

    fn request(family: Family, model: &str, context: Vec<ContextItem>) -> ModelRequest {
        ModelRequest {
            purpose: Purpose::Turn,
            model: ModelRoute::Api {
                family,
                model: model.into(),
            },
            system: "sys".into(),
            tools: Vec::<ModelToolSpec>::new().into(),
            context: context.into(),
            params: RequestParams {
                thinking: ThinkingLevel::High,
                effort: None,
                temperature: None,
                max_output_tokens: None,
            },
            cache_key: Some("sess-1".into()),
        }
    }

    fn text(value: &str) -> Part {
        Part::Text { text: value.into() }
    }

    fn user(value: &str) -> ContextItem {
        ContextItem::User {
            parts: vec![text(value)],
        }
    }

    fn openai_plan() -> ThinkingPlan {
        ThinkingPlan {
            level: ThinkingLevel::High,
            wire: WireThinking::OpenAi {
                effort: Some("high"),
            },
            temperature: None,
            notices: Vec::new(),
        }
    }

    fn anthropic_plan() -> ThinkingPlan {
        ThinkingPlan {
            level: ThinkingLevel::High,
            wire: WireThinking::Anthropic {
                thinking: AnthropicThinking::Omit,
                effort: None,
            },
            temperature: None,
            notices: Vec::new(),
        }
    }

    fn lower(
        request: &ModelRequest,
        plan: &ThinkingPlan,
        key: &CopilotKey,
        integration: &Integration,
    ) -> Result<CopilotWire, ProviderError> {
        build(&CopilotRequest {
            request,
            thinking: plan,
            max_output: Some(8192),
            display_supported: false,
            reasoning_summary: false,
            base_url: PERSONAL_BASE_URL,
            key,
            integration,
        })
    }

    fn header<'a>(headers: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, value)| value.as_str())
    }

    fn reasoning_replay_context() -> Vec<ContextItem> {
        let item = r#"{"type":"reasoning","id":"rs_1","encrypted_content":"abc"}"#;
        vec![
            user("hi"),
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Responses,
                    model: "gpt-5.1-codex".into(),
                },
                replay: None,
                parts: vec![
                    AssistantPart::Thinking {
                        text: "t".into(),
                        replay: Some(RawJson::parse(item).unwrap()),
                    },
                    AssistantPart::Text {
                        text: "ok".into(),
                        replay: None,
                    },
                ],
            },
            user("more"),
        ]
    }

    #[test]
    fn model_ids_map_to_the_pinned_surfaces() {
        for (model, surface) in [
            ("claude-sonnet-4.5", Surface::Anthropic),
            ("claude-haiku-4.5", Surface::Anthropic),
            ("claude-opus-4.6-1m", Surface::Anthropic),
            ("claude-fable-5", Surface::Anthropic),
            ("claude-mythos-5", Surface::Anthropic),
            ("gpt-5.1-codex", Surface::Responses),
            ("gpt-6-astra-1m", Surface::Responses),
            ("grok-4.6-1m", Surface::Responses),
            ("oswe-vscode-prime", Surface::Responses),
            ("mai-code-1-flash-picker", Surface::Responses),
            ("gpt-4.1", Surface::Chat),
            ("gemini-2.5-pro", Surface::Chat),
            ("grok-code-fast-1", Surface::Chat),
            ("claude-3.5-sonnet", Surface::Chat),
        ] {
            assert_eq!(surface_for_model(model), surface, "{model}");
        }
        for family in [
            Family::Codex,
            Family::Bedrock,
            Family::Cursor,
            Family::Devin,
            Family::Droid,
            Family::GitLab,
            Family::GoogleGenerativeAi,
            Family::GoogleVertex,
            Family::GoogleGeminiCli,
            Family::GoogleAntigravity,
            Family::Mistral,
            Family::Ollama,
            Family::PiMessages,
        ] {
            assert_eq!(Surface::of_family(family), None, "{family:?}");
        }
        for surface in [Surface::Chat, Surface::Responses, Surface::Anthropic] {
            assert_eq!(Surface::of_family(surface.family()), Some(surface));
        }
    }

    #[test]
    fn a_long_context_sibling_sends_its_base_model_id() {
        assert_eq!(wire_model_id("gpt-5.6-sol-1m"), "gpt-5.6-sol");
        assert_eq!(wire_model_id("gpt-4.1"), "gpt-4.1");
        assert_eq!(wire_model_id("-1m"), "-1m");
        let key = CopilotKey::parse("gho_tok");
        let request = request(Family::Responses, "grok-4.6-1m", vec![user("hi")]);
        let wire = lower(&request, &openai_plan(), &key, &Integration::Chat).unwrap();
        assert_eq!(wire.model.as_ref(), "grok-4.6-1m");
        let body = String::from_utf8(wire.body).unwrap();
        assert!(
            body.starts_with(r#"{"model":"grok-4.6","instructions":"#),
            "{body}"
        );
    }

    #[test]
    fn a_key_is_a_raw_token_or_a_structured_envelope() {
        let raw = CopilotKey::parse("  gho_abc ");
        assert_eq!(raw.token(), "gho_abc");
        assert!(!raw.enterprise());

        let envelope = CopilotKey::parse(
            r#"{"token":"t1","enterpriseUrl":"https://Corp.GHE.com/path","apiEndpoint":"https://copilot-api.corp.ghe.com/"}"#,
        );
        assert_eq!(envelope.token(), "t1");
        assert_eq!(envelope.enterprise_domain.as_deref(), Some("corp.ghe.com"));
        assert_eq!(
            envelope.api_endpoint.as_deref(),
            Some("https://copilot-api.corp.ghe.com")
        );
        assert!(envelope.enterprise());

        let dropped = CopilotKey::parse(
            r#"{"token":"t2","enterpriseUrl":"github.com","apiEndpoint":"http://x.example"}"#,
        );
        assert_eq!(
            (dropped.enterprise_domain, dropped.api_endpoint),
            (None, None)
        );
        let creds = CopilotKey::parse(r#"{"token":"t3","apiEndpoint":"https://u:p@x.example"}"#);
        assert_eq!(creds.api_endpoint, None);
        let numeric = CopilotKey::parse(r#"{"token":"t4","enterpriseUrl":5}"#);
        assert_eq!(numeric.token(), "t4");
        assert_eq!(numeric.enterprise_domain, None);
        assert_eq!(CopilotKey::parse(r#"{"x":1}"#).token(), r#"{"x":1}"#);
        let secret = CopilotKey::parse(r#"{"token":"envelope-secret-token"}"#);
        assert!(!format!("{secret:?}").contains("envelope-secret-token"));
    }

    #[test]
    fn the_base_url_follows_the_pinned_resolution_order() {
        let personal = |raw: &str| resolve_base_url(PERSONAL_BASE_URL, &CopilotKey::parse(raw));
        assert_eq!(personal("gho_tok"), PERSONAL_BASE_URL);
        assert_eq!(
            personal(r#"{"token":"t","apiEndpoint":"https://api.business.githubcopilot.com"}"#),
            "https://api.business.githubcopilot.com"
        );
        assert_eq!(
            personal(r#"{"token":"t","enterpriseUrl":"corp.ghe.com"}"#),
            "https://copilot-api.corp.ghe.com"
        );
        assert_eq!(
            personal(r#"{"token":"t","enterpriseUrl":"copilot-api.corp.ghe.com"}"#),
            "https://copilot-api.corp.ghe.com"
        );
        assert_eq!(
            personal("tid=1;exp=2;proxy-ep=proxy.individual.githubcopilot.com;sku=x"),
            "https://api.individual.githubcopilot.com"
        );
        assert_eq!(
            personal("tid=1;proxy-ep=proxy.evil.example"),
            PERSONAL_BASE_URL
        );
        assert_eq!(
            personal("proxy-ep=x@evil.example/.githubcopilot.com"),
            PERSONAL_BASE_URL
        );
        assert_eq!(
            resolve_base_url("", &CopilotKey::parse("t")),
            PERSONAL_BASE_URL
        );
    }

    #[test]
    fn a_configured_proxy_outside_the_copilot_hosts_always_wins() {
        let envelope = CopilotKey::parse(
            r#"{"token":"t","enterpriseUrl":"corp.ghe.com","apiEndpoint":"https://api.business.githubcopilot.com"}"#,
        );
        for custom in [
            "https://proxy.example.com/copilot/",
            "https://evil.example/githubcopilot.com",
        ] {
            assert_eq!(
                resolve_base_url(custom, &envelope),
                custom.trim_end_matches('/')
            );
        }
        assert_eq!(
            resolve_base_url("https://api.githubcopilot.com/", &envelope),
            "https://api.business.githubcopilot.com"
        );
    }

    #[test]
    fn the_integration_identity_has_a_precedence_and_one_fallback() {
        let pin = Integration::pinned(" vscode-chat ").unwrap();
        assert_eq!(pin.id(), "vscode-chat");
        assert_eq!(Integration::pinned(""), None);
        assert_eq!(Integration::pinned("a\nb"), None);

        let personal = CopilotKey::parse("gho_tok");
        let enterprise = CopilotKey::parse(r#"{"token":"t","enterpriseUrl":"corp.ghe.com"}"#);
        let initial = Integration::initial;
        assert_eq!(
            initial(Some(pin.clone()), Some(Integration::Cli), &enterprise),
            pin
        );
        assert_eq!(
            initial(None, Some(Integration::Chat), &enterprise),
            Integration::Chat
        );
        assert_eq!(initial(None, None, &enterprise), Integration::Cli);
        assert_eq!(initial(None, None, &personal), Integration::Chat);

        assert_eq!(Integration::Chat.fallback(), Some(Integration::Cli));
        assert_eq!(Integration::Cli.fallback(), None);
        assert_eq!(pin.fallback(), None);
        assert_eq!(Integration::Chat.fallback_learned(), Some(Integration::Cli));
        assert_eq!(Integration::Cli.fallback_learned(), Some(Integration::Chat));
        assert_eq!(pin.fallback_learned(), None);
        assert_eq!(Integration::Chat.id(), "copilot-chat");
        assert_eq!(Integration::Cli.id(), "copilot-developer-cli");
    }

    #[test]
    fn only_identity_denials_are_resent() {
        assert!(identity_denied(403, None));
        assert!(identity_denied(400, Some("model_not_supported")));
        assert!(!identity_denied(400, Some("invalid_request")));
        assert!(!identity_denied(400, None));
        assert!(!identity_denied(401, None));
        assert!(!identity_denied(429, None));
    }

    #[test]
    fn a_chat_wire_carries_the_copilot_identity() {
        let key = CopilotKey::parse("gho_tok");
        let request = request(Family::Chat, "gpt-4.1", vec![user("hi")]);
        let wire = lower(&request, &openai_plan(), &key, &Integration::Chat).unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://api.githubcopilot.com/chat/completions"
        );
        assert_eq!(wire.user_agent, "copilot/1.0.82");
        assert_eq!(wire.secret.as_ref(), "gho_tok");
        assert_eq!(wire.prefix, None);
        assert_eq!(
            wire.headers.first(),
            Some(&("authorization", String::from("Bearer gho_tok")))
        );
        for (name, value) in [
            ("editor-version", "copilot/1.0.82"),
            ("copilot-integration-id", "copilot-chat"),
            ("copilot-harness-id", "copilot-sdk"),
            ("openai-intent", "conversation-agent"),
            ("x-github-api-version", "2026-08-01"),
            ("x-initiator", "user"),
            ("x-interaction-type", "conversation-user"),
        ] {
            assert_eq!(header(&wire.headers, name), Some(value), "{name}");
        }
        assert_eq!(header(&wire.headers, "copilot-vision-request"), None);
        let body = String::from_utf8(wire.body).unwrap();
        assert!(
            body.starts_with(r#"{"model":"gpt-4.1","messages":["#),
            "{body}"
        );
    }

    #[test]
    fn tool_output_makes_an_agent_request_and_images_add_the_vision_header() {
        let png = Part::Image {
            mime: "image/png".into(),
            bytes: b"\x89PNG".as_slice().into(),
        };
        let context = vec![
            ContextItem::User {
                parts: vec![text("look"), png],
            },
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Chat,
                    model: "gpt-4.1".into(),
                },
                replay: None,
                parts: vec![AssistantPart::ToolCall {
                    call: CallId::new("call_1"),
                    name: "read".into(),
                    args: RawJson::parse("{}").unwrap(),
                    replay: None,
                }],
            },
            ContextItem::ToolResult {
                call: CallId::new("call_1"),
                name: "read".into(),
                is_error: false,
                parts: vec![text("done")],
            },
        ];
        let request = request(Family::Chat, "gpt-4.1", context);
        assert_eq!(initiator(&request), Initiator::Agent);
        let headers = identity_headers(&request, &Integration::Cli);
        assert_eq!(header(&headers, "x-initiator"), Some("agent"));
        assert_eq!(
            header(&headers, "x-interaction-type"),
            Some("conversation-agent")
        );
        assert_eq!(header(&headers, "copilot-vision-request"), Some("true"));
        assert_eq!(
            header(&headers, "copilot-integration-id"),
            Some("copilot-developer-cli")
        );

        let empty = self::request(Family::Chat, "gpt-4.1", Vec::new());
        assert_eq!(initiator(&empty), Initiator::User);
    }

    #[test]
    fn a_responses_wire_never_replays_connection_bound_reasoning() {
        let key = CopilotKey::parse("gho_tok");
        let request = request(
            Family::Responses,
            "gpt-5.1-codex",
            reasoning_replay_context(),
        );
        let wire = lower(&request, &openai_plan(), &key, &Integration::Chat).unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://api.githubcopilot.com/responses"
        );
        let body = String::from_utf8(wire.body).unwrap();
        assert!(
            !body.contains(r#""id":"rs_1""#),
            "the reasoning item is gone: {body}"
        );
        assert!(
            body.contains(r#""text":"ok""#),
            "assistant text survives: {body}"
        );

        // The caller's request is untouched, and a request without replay is
        // borrowed rather than copied.
        assert!(matches!(without_native_history(&request), Cow::Owned(_)));
        let plain = self::request(Family::Responses, "gpt-5.1-codex", vec![user("hi")]);
        assert!(matches!(without_native_history(&plain), Cow::Borrowed(_)));
        let still_replays = request.context.iter().any(|item| {
            matches!(item, ContextItem::Assistant { parts, .. }
                if parts.iter().any(|part| matches!(part, AssistantPart::Thinking { replay: Some(_), .. })))
        });
        assert!(still_replays);
    }

    #[test]
    fn an_anthropic_wire_sends_the_bearer_once_beside_the_identity() {
        let key = CopilotKey::parse("gho_tok");
        let request = request(Family::Anthropic, "claude-opus-4.6-1m", vec![user("hi")]);
        let wire = lower(&request, &anthropic_plan(), &key, &Integration::Chat).unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://api.githubcopilot.com/v1/messages"
        );
        let bearers = wire
            .headers
            .iter()
            .filter(|(name, _)| *name == "authorization")
            .count();
        assert_eq!(bearers, 1);
        assert_eq!(
            header(&wire.headers, "authorization"),
            Some("Bearer gho_tok")
        );
        assert_eq!(
            header(&wire.headers, "anthropic-version"),
            Some("2023-06-01")
        );
        assert_eq!(
            header(&wire.headers, "copilot-integration-id"),
            Some("copilot-chat")
        );
        assert!(wire.prefix.is_some());
        assert_eq!(wire.model.as_ref(), "claude-opus-4.6-1m");
        let body = String::from_utf8(wire.body).unwrap();
        assert!(
            body.starts_with(r#"{"model":"claude-opus-4.6","max_tokens":8192"#),
            "{body}"
        );

        let mismatch = lower(&request, &openai_plan(), &key, &Integration::Chat).unwrap_err();
        assert!(
            matches!(mismatch, ProviderError::InvalidRequest { .. }),
            "{mismatch:?}"
        );
    }

    #[test]
    fn a_codex_route_is_refused() {
        let key = CopilotKey::parse("gho_tok");
        let request = request(Family::Codex, "gpt-5.1-codex", vec![user("hi")]);
        let error = lower(&request, &openai_plan(), &key, &Integration::Chat).unwrap_err();
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn the_wire_debug_output_hides_the_token() {
        let key = CopilotKey::parse("gho_secret");
        let request = request(Family::Chat, "gpt-4.1", vec![user("hi")]);
        let wire = lower(&request, &openai_plan(), &key, &Integration::Chat).unwrap();
        let shown = format!("{wire:?}");
        assert!(!shown.contains("gho_secret"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn the_policy_wire_enables_the_wire_model_id() {
        let key = CopilotKey::parse("gho_tok");
        let wire = enable_model_policy(
            PERSONAL_BASE_URL,
            &key,
            "claude-opus-4.6-1m",
            &Integration::Chat,
        )
        .unwrap();
        assert_eq!(
            wire.endpoint().unwrap().as_str(),
            "https://api.githubcopilot.com/models/claude-opus-4.6/policy"
        );
        assert_eq!(wire.body, br#"{"state":"enabled"}"#.to_vec());
        assert_eq!(wire.user_agent, "copilot/1.0.82");
        for (name, value) in [
            ("authorization", "Bearer gho_tok"),
            ("copilot-integration-id", "copilot-chat"),
            ("openai-intent", "chat-policy"),
            ("x-initiator", "user"),
            ("x-interaction-type", "chat-policy"),
        ] {
            assert_eq!(header(&wire.headers, name), Some(value), "{name}");
        }
        for bad in ["", "a/b", "..", "x y"] {
            let error =
                enable_model_policy(PERSONAL_BASE_URL, &key, bad, &Integration::Chat).unwrap_err();
            assert!(
                matches!(error, ProviderError::InvalidRequest { .. }),
                "{bad:?}"
            );
        }
        assert!(!format!("{wire:?}").contains("gho_tok"));
    }

    #[test]
    fn quota_failures_and_status_notes_are_classified() {
        assert!(is_quota_failure(402, None, ""));
        assert!(is_quota_failure(429, Some("quota_exceeded"), ""));
        assert!(is_quota_failure(
            500,
            Some("additional_spend_limit_reached"),
            ""
        ));
        assert!(is_quota_failure(
            429,
            None,
            "Premium Quota Exceeded for this plan"
        ));
        assert!(!is_quota_failure(429, None, "slow down"));
        assert!(!is_quota_failure(
            429,
            Some("rate_limit_exceeded"),
            "slow down"
        ));
        assert!(failure_note(421).is_some_and(|note| note.contains("421")));
        assert!(failure_note(403).is_some_and(|note| note.contains("403")));
        assert_eq!(failure_note(500), None);
    }

    #[test]
    fn the_identity_cache_learns_forgets_and_stays_bounded() {
        let key = CopilotKey::parse("gho_secret");
        let cache_key = IdentityCache::key(&key, "https://API.githubcopilot.com/");
        assert!(!cache_key.contains("gho_secret"), "{cache_key:?}");
        assert_eq!(
            cache_key,
            IdentityCache::key(&key, "https://api.githubcopilot.com")
        );
        assert_ne!(
            cache_key,
            IdentityCache::key(
                &CopilotKey::parse("gho_other"),
                "https://api.githubcopilot.com"
            )
        );
        assert_ne!(
            cache_key,
            IdentityCache::key(&key, "https://proxy.example.com")
        );

        let cache = IdentityCache::default();
        assert_eq!(cache.get(&cache_key), None);
        cache.remember(&cache_key, &Integration::Cli);
        assert_eq!(cache.get(&cache_key), Some(Integration::Cli));
        cache.remember(&cache_key, &Integration::Chat);
        assert_eq!(cache.get(&cache_key), Some(Integration::Chat));
        cache.remember("pinned", &Integration::Pinned("x".into()));
        assert_eq!(cache.get("pinned"), None);
        cache.forget(&cache_key);
        assert_eq!(cache.get(&cache_key), None);

        for index in 0..=IDENTITY_CACHE_LIMIT {
            cache.remember(&format!("k{index}"), &Integration::Cli);
        }
        assert_eq!(cache.get("k0"), None, "the oldest entry was evicted");
        assert_eq!(cache.get("k1"), Some(Integration::Cli));
        assert_eq!(
            cache.get(&format!("k{IDENTITY_CACHE_LIMIT}")),
            Some(Integration::Cli)
        );
    }

    fn near(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "{actual} is not {expected}"
        );
    }

    fn near_duration(actual: Duration, expected: Duration) {
        assert!(
            actual.abs_diff(expected) < Duration::from_millis(1),
            "{actual:?} is not {expected:?}"
        );
    }

    #[test]
    fn a_denied_chat_identity_is_resent_once_as_the_cli_and_the_win_is_learned() {
        let cache = IdentityCache::default();
        let key = CopilotKey::parse("gho_tok");
        let mut retry = IdentityRetry::begin(&cache, &key, PERSONAL_BASE_URL, None);
        assert_eq!(retry.integration(), &Integration::Chat);
        assert_eq!(retry.denied(403, None), Some(&Integration::Cli));
        assert_eq!(retry.integration(), &Integration::Cli);
        assert_eq!(retry.denied(403, None), None, "a second denial ends it");
        retry.succeeded(&cache);

        let mut next = IdentityRetry::begin(&cache, &key, PERSONAL_BASE_URL, None);
        assert_eq!(
            next.integration(),
            &Integration::Cli,
            "starts where the last request won"
        );
        // A learned CLI identity goes stale when the organisation flips its
        // policy: one resend as chat, and a failed resend forgets the entry.
        assert_eq!(
            next.denied(400, Some("model_not_supported")),
            Some(&Integration::Chat)
        );
        next.failed(&cache);
        let after = IdentityRetry::begin(&cache, &key, PERSONAL_BASE_URL, None);
        assert_eq!(
            after.integration(),
            &Integration::Chat,
            "the failed resend forgot it"
        );
    }

    #[test]
    fn a_first_try_success_learns_nothing_and_an_enterprise_cli_start_has_no_resend() {
        let cache = IdentityCache::default();
        let key = CopilotKey::parse("gho_tok");
        let retry = IdentityRetry::begin(&cache, &key, PERSONAL_BASE_URL, None);
        retry.succeeded(&cache);
        assert_eq!(
            cache.get(&IdentityCache::key(&key, PERSONAL_BASE_URL)),
            None
        );

        let enterprise = CopilotKey::parse(r#"{"token":"t","enterpriseUrl":"corp.ghe.com"}"#);
        let mut retry = IdentityRetry::begin(&cache, &enterprise, PERSONAL_BASE_URL, None);
        assert_eq!(retry.integration(), &Integration::Cli);
        assert_eq!(retry.denied(403, None), None);
    }

    #[test]
    fn a_pinned_identity_neither_reads_nor_writes_the_cache_and_never_resends() {
        let cache = IdentityCache::default();
        let key = CopilotKey::parse("gho_tok");
        let learned = IdentityCache::key(&key, PERSONAL_BASE_URL);
        cache.remember(&learned, &Integration::Cli);
        let mut retry = IdentityRetry::begin(
            &cache,
            &key,
            PERSONAL_BASE_URL,
            Integration::pinned("vscode-chat"),
        );
        assert_eq!(retry.integration().id(), "vscode-chat");
        assert_eq!(retry.denied(403, None), None);
        retry.failed(&cache);
        retry.succeeded(&cache);
        assert_eq!(cache.get(&learned), Some(Integration::Cli));
    }

    #[test]
    fn only_a_real_identity_denial_earns_a_resend() {
        let cache = IdentityCache::default();
        let key = CopilotKey::parse("gho_tok");
        for (status, code) in [
            (500, None),
            (401, None),
            (402, None),
            (429, Some("quota_exceeded")),
            (403, Some("quota_exceeded")),
            (400, Some("invalid_request")),
        ] {
            let mut retry = IdentityRetry::begin(&cache, &key, PERSONAL_BASE_URL, None);
            assert_eq!(retry.denied(status, code), None, "{status} {code:?}");
        }
    }

    #[test]
    fn quota_and_misdirection_become_provider_errors_and_the_rest_is_left_alone() {
        let quota = failure_error(
            Family::Responses,
            429,
            Some("quota_exceeded"),
            "plan used up",
        );
        assert!(
            matches!(&quota, Some(ProviderError::Quota { message }) if message == "plan used up"),
            "{quota:?}"
        );
        let Some(ProviderError::Status {
            family,
            status,
            message,
        }) = failure_error(Family::Chat, 421, None, "")
        else {
            panic!("a 421 keeps its status");
        };
        assert_eq!((family, status), (Family::Chat, 421));
        assert!(message.contains("421"), "{message}");
        let Some(ProviderError::Status { message, .. }) =
            failure_error(Family::Chat, 403, None, "denied")
        else {
            panic!("a 403 keeps its status");
        };
        assert!(
            message.starts_with("denied\n") && message.contains("403"),
            "{message}"
        );
        assert!(failure_error(Family::Chat, 500, None, "boom").is_none());
    }

    #[test]
    fn premium_requests_follow_the_initiator_and_the_free_tier_multiplier() {
        near(premium_requests(Initiator::Agent, Some(3.0)), 0.0);
        near(premium_requests(Initiator::Agent, None), 0.0);
        near(premium_requests(Initiator::User, Some(0.33)), 0.33);
        near(premium_requests(Initiator::User, None), 1.0);
        near(premium_requests(Initiator::User, Some(0.0)), 1.0);
        near(premium_requests(Initiator::User, Some(3.0)), 3.0);
        near(premium_requests(Initiator::User, Some(f64::NAN)), 1.0);
        near(premium_requests(Initiator::User, Some(f64::INFINITY)), 1.0);
        near(premium_requests(Initiator::User, Some(-2.0)), 1.0);

        // The count follows the request the model actually saw last.
        let user_turn = request(Family::Chat, "gpt-4.1", vec![user("hi")]);
        let after_tool = request(
            Family::Chat,
            "gpt-4.1",
            vec![
                user("hi"),
                ContextItem::ToolResult {
                    call: CallId::new("c"),
                    name: "read".into(),
                    is_error: false,
                    parts: vec![text("ok")],
                },
            ],
        );
        assert_eq!(initiator(&user_turn), Initiator::User);
        assert_eq!(initiator(&after_tool), Initiator::Agent);
        near(premium_requests(initiator(&user_turn), Some(1.0)), 1.0);
        near(premium_requests(initiator(&after_tool), Some(1.0)), 0.0);
        let empty = request(Family::Chat, "gpt-4.1", Vec::new());
        assert_eq!(initiator(&empty), Initiator::User);
        near(premium_requests(initiator(&empty), None), 1.0);
    }

    const MODELS: &str = r#"{"data":[
        {"id":"gpt-4.1","name":"GPT-4.1","capabilities":{"type":"chat","limits":{"max_context_window_tokens":128000,"max_output_tokens":16384,"max_prompt_tokens":128000},"supports":{"tool_calls":true,"vision":true}},"billing":{"is_premium":false,"multiplier":0}},
        {"id":"text-embedding-3-small","capabilities":{"type":"embeddings"}},
        {"id":"claude-opus-4.6","name":"Claude Opus 4.6","capabilities":{"type":"chat","limits":{"max_context_window_tokens":1000000,"max_output_tokens":64000,"max_prompt_tokens":936000},"supports":{"vision":true,"tool_calls":true}},"billing":{"multiplier":3,"token_prices":{"default":{"max_prompt_tokens":200000,"input_price":500,"output_price":2500,"cache_price":50},"long_context":{"max_prompt_tokens":936000,"input_price":1000,"output_price":3750,"cache_price":100}}}},
        {"id":42},
        "not an object",
        {"id":"no-caps"}
    ]}"#;

    #[test]
    fn the_model_list_decodes_chat_rows_windows_prices_and_the_long_context_sibling() {
        let models = decode_models(MODELS.as_bytes()).unwrap();
        let ids: Vec<&str> = models.iter().map(|model| &*model.id).collect();
        assert_eq!(
            ids,
            [
                "gpt-4.1",
                "claude-opus-4.6",
                "claude-opus-4.6-1m",
                "no-caps"
            ],
            "the embedding row and the two odd rows are dropped"
        );

        let gpt = &models[0];
        assert_eq!(gpt.surface, Surface::Chat);
        assert_eq!(gpt.context_window, Some(128_000));
        assert_eq!(gpt.max_output, Some(16_384));
        assert_eq!((gpt.image_input, gpt.tool_calls), (Some(true), Some(true)));
        assert_eq!(gpt.price, None);
        near(gpt.premium_multiplier.unwrap_or(f64::NAN), 0.0);
        assert!(!gpt.long_context);

        // The served window is the long ceiling; the base row is the default
        // tier (200000 + 64000 output), and the sibling is the whole window.
        let (base, sibling) = (&models[1], &models[2]);
        assert_eq!(base.surface, Surface::Anthropic);
        assert_eq!(base.context_window, Some(264_000));
        assert_eq!(sibling.context_window, Some(1_000_000));
        assert!(sibling.long_context && !base.long_context);
        assert_eq!(&*sibling.name, "Claude Opus 4.6 (1M)");
        assert_eq!(&*sibling.wire_id, "claude-opus-4.6");
        assert_eq!(&*base.wire_id, "claude-opus-4.6");
        assert_eq!(sibling.surface, Surface::Anthropic);
        near(base.premium_multiplier.unwrap_or(f64::NAN), 3.0);
        let (default, long) = (base.price.unwrap(), sibling.price.unwrap());
        near(default.input, 5.0);
        near(default.output, 25.0);
        near(default.cache_read, 0.5);
        assert_eq!(default.cache_write, None);
        near(long.input, 10.0);
        near(long.output, 37.5);
        near(long.cache_read, 1.0);
        assert_eq!(long.cache_write, None);

        let plain = &models[3];
        assert_eq!((&*plain.name, plain.surface), ("no-caps", Surface::Chat));
        assert_eq!(
            (plain.context_window, plain.max_output, plain.image_input),
            (None, None, None)
        );
    }

    #[test]
    fn a_tighter_prompt_budget_than_the_billed_default_wins_and_a_flat_row_has_no_sibling() {
        let tight = r#"{"data":[{"id":"gpt-5.6-sol","capabilities":{"type":"chat","limits":{"max_context_window_tokens":1050000,"max_output_tokens":128000,"max_prompt_tokens":272000}},"billing":{"token_prices":{"default":{"max_prompt_tokens":400000},"long_context":{"max_prompt_tokens":922000}}}},
            {"id":"claude-sonnet-4.5","capabilities":{"type":"chat","limits":{"max_context_window_tokens":200000,"max_output_tokens":32000}}}]}"#;
        let models = decode_models(tight.as_bytes()).unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(
            models[0].context_window,
            Some(400_000),
            "272000 + 128000 output"
        );
        assert_eq!(models[0].surface, Surface::Responses);
        assert_eq!(&*models[1].id, "gpt-5.6-sol-1m");
        assert_eq!(&*models[1].wire_id, "gpt-5.6-sol");
        assert_eq!(models[1].context_window, Some(1_050_000));
        assert_eq!(
            &*models[2].id, "claude-sonnet-4.5",
            "no longer tier, no sibling"
        );
        assert_eq!(models[2].context_window, Some(200_000));
    }

    #[test]
    fn a_tier_missing_any_cache_input_or_output_leg_has_no_price() {
        let body = r#"{"data":[
            {"id":"gpt-cache-missing","capabilities":{"type":"chat"},"billing":{"multiplier":1,"token_prices":{"default":{"input_price":500,"output_price":2500}}}},
            {"id":"gpt-cache-partial","capabilities":{"type":"chat"},"billing":{"multiplier":1,"token_prices":{"default":{"input_price":500,"output_price":2500,"cache_price":50},"long_context":{"input_price":1000,"output_price":3750}}}}
        ]}"#;
        let models = decode_models(body.as_bytes()).expect("valid list decodes");
        let missing = models
            .iter()
            .find(|model| &*model.id == "gpt-cache-missing")
            .expect("base row present");
        assert_eq!(
            missing.price, None,
            "a missing cache leg is unknown, never zero"
        );
        let partial = models
            .iter()
            .find(|model| &*model.id == "gpt-cache-partial")
            .expect("partial row present");
        let base_price = partial.price.expect("complete default tier prices");
        near(base_price.cache_read, 0.5);
        assert_eq!(base_price.cache_write, None);
    }

    #[test]
    fn haiku_rows_pin_both_cache_legs_before_completeness() {
        let body = r#"{"data":[
            {"id":"claude-haiku-5.5","name":"Haiku","capabilities":{"type":"chat","limits":{"max_context_window_tokens":200000,"max_output_tokens":32000,"max_prompt_tokens":100000}},"billing":{"multiplier":1,"token_prices":{"default":{"max_prompt_tokens":100000,"input_price":100,"output_price":500},"long_context":{"max_prompt_tokens":168000,"input_price":200,"output_price":1000}}}}
        ]}"#;
        let models = decode_models(body.as_bytes()).expect("valid list decodes");
        assert_eq!(models.len(), 2);
        let base = &models[0];
        let sibling = &models[1];
        assert_eq!(&*sibling.id, "claude-haiku-5.5-1m");
        assert_eq!(&*sibling.wire_id, "claude-haiku-5.5");
        let base_price = base
            .price
            .expect("haiku base completes through the override");
        near(base_price.input, 1.0);
        near(base_price.output, 5.0);
        near(base_price.cache_read, 0.01);
        assert_eq!(base_price.cache_write, Some(0.125));
        let long_price = sibling
            .price
            .expect("haiku sibling completes through the override");
        near(long_price.input, 2.0);
        near(long_price.output, 10.0);
        near(long_price.cache_read, 0.05);
        assert_eq!(long_price.cache_write, Some(0.625));
    }

    #[test]
    fn an_unpriced_long_tier_falls_back_to_the_base_price() {
        let body = r#"{"data":[
            {"id":"gpt-long-fallback","capabilities":{"type":"chat","limits":{"max_context_window_tokens":500000,"max_output_tokens":64000,"max_prompt_tokens":400000}},"billing":{"multiplier":2,"token_prices":{"default":{"max_prompt_tokens":200000,"input_price":500,"output_price":2500,"cache_price":50},"long_context":{"max_prompt_tokens":436000}}}}
        ]}"#;
        let models = decode_models(body.as_bytes()).expect("valid list decodes");
        assert_eq!(models.len(), 2);
        let base = &models[0];
        let sibling = &models[1];
        let base_price = base.price.expect("complete default tier prices");
        let sibling_price = sibling.price.expect("sibling inherits the base card");
        assert_eq!(sibling_price, base_price);
        assert_eq!(sibling_price.cache_write, None);
    }

    #[test]
    fn an_invalid_multiplier_is_unknown_at_the_catalog_boundary() {
        let body = r#"{"data":[
            {"id":"gpt-neg-mult","capabilities":{"type":"chat"},"billing":{"multiplier":-3}},
            {"id":"gpt-str-mult","capabilities":{"type":"chat"},"billing":{"multiplier":"x"}}
        ]}"#;
        let models = decode_models(body.as_bytes()).expect("valid list decodes");
        for model in &models {
            assert_eq!(
                model.premium_multiplier, None,
                "invalid metadata never reaches the counter"
            );
        }
        near(
            premium_requests(Initiator::User, models[0].premium_multiplier),
            1.0,
        );
    }

    #[test]
    fn a_model_list_that_is_not_a_list_is_a_protocol_error() {
        for body in ["{}", "[1]", "not json", r#"{"data":"x"}"#] {
            let error = decode_models(body.as_bytes()).unwrap_err();
            assert!(
                matches!(error, ProviderError::Protocol { .. }),
                "{body}: {error:?}"
            );
        }
        assert_eq!(decode_models(br#"{"data":[]}"#).unwrap(), []);
    }

    #[test]
    fn discovery_requests_keep_the_cli_identity_and_the_endpoint_is_read_from_the_account() {
        let key = CopilotKey::parse("gho_tok");
        let models = models_request(PERSONAL_BASE_URL, &key).unwrap();
        assert_eq!(
            models.endpoint().unwrap().as_str(),
            "https://api.githubcopilot.com/models"
        );
        assert_eq!(models.user_agent, "copilot/1.0.82");
        for (name, value) in [
            ("authorization", "Bearer gho_tok"),
            ("copilot-integration-id", "copilot-developer-cli"),
            ("x-initiator", "user"),
            ("x-github-api-version", "2026-08-01"),
        ] {
            assert_eq!(header(&models.headers, name), Some(value), "{name}");
        }
        assert_eq!(header(&models.headers, "x-interaction-type"), None);

        let discovery = endpoint_discovery_request(&key).unwrap();
        assert_eq!(
            discovery.endpoint().unwrap().as_str(),
            "https://api.github.com/copilot_internal/user"
        );
        assert_eq!(
            header(&discovery.headers, "authorization"),
            Some("token gho_tok")
        );

        assert_eq!(
            decode_endpoint(br#"{"endpoints":{"api":"https://api.business.githubcopilot.com/"}}"#)
                .as_deref(),
            Some("https://api.business.githubcopilot.com")
        );
        for body in [
            &br#"{"endpoints":{"api":"http://plain.example"}}"#[..],
            br#"{"endpoints":{"api":"https://u:p@x.example"}}"#,
            br#"{"endpoints":{}}"#,
            b"[]",
        ] {
            assert_eq!(decode_endpoint(body), None);
        }

        assert!(needs_endpoint_discovery(PERSONAL_BASE_URL, &key));
        let found = key.with_api_endpoint("https://api.individual.githubcopilot.com/");
        assert!(!needs_endpoint_discovery(PERSONAL_BASE_URL, &found));
        assert_eq!(
            resolve_base_url(PERSONAL_BASE_URL, &found),
            "https://api.individual.githubcopilot.com"
        );
        let kept = found.with_api_endpoint("https://other.githubcopilot.com");
        assert_eq!(
            resolve_base_url(PERSONAL_BASE_URL, &kept),
            "https://api.individual.githubcopilot.com",
            "a stored endpoint is not overridden"
        );
        let refused = CopilotKey::parse("t").with_api_endpoint("http://plain.example");
        assert!(needs_endpoint_discovery(PERSONAL_BASE_URL, &refused));
    }

    #[test]
    fn a_token_that_cannot_be_a_header_is_refused_before_any_request() {
        for raw in ["", "   ", "a\nb"] {
            let key = CopilotKey::parse(raw);
            let request = request(Family::Chat, "gpt-4.1", vec![user("hi")]);
            let error = lower(&request, &openai_plan(), &key, &Integration::Chat).unwrap_err();
            assert!(
                matches!(error, ProviderError::InvalidRequest { .. }),
                "{raw:?}: {error:?}"
            );
            assert!(models_request(PERSONAL_BASE_URL, &key).is_err());
            assert!(endpoint_discovery_request(&key).is_err());
            assert!(
                enable_model_policy(PERSONAL_BASE_URL, &key, "gpt-4.1", &Integration::Chat)
                    .is_err()
            );
        }
    }

    fn orphan_context() -> Vec<ContextItem> {
        vec![
            user("hi"),
            ContextItem::Assistant {
                source: ReplaySource {
                    family: Family::Chat,
                    model: "gpt-4.1".into(),
                },
                replay: None,
                parts: vec![AssistantPart::ToolCall {
                    call: CallId::new("c1"),
                    name: "read".into(),
                    args: RawJson::parse("{}").unwrap(),
                    replay: None,
                }],
            },
            ContextItem::ToolResult {
                call: CallId::new("ghost"),
                name: "read".into(),
                is_error: false,
                parts: vec![text("stray")],
            },
        ]
    }

    #[test]
    fn the_chat_and_responses_wires_send_only_paired_tool_exchanges() {
        let key = CopilotKey::parse("gho_tok");
        let chat = request(Family::Chat, "gpt-4.1", orphan_context());
        let body = lower(&chat, &openai_plan(), &key, &Integration::Chat)
            .unwrap()
            .body;
        let body = String::from_utf8(body).unwrap();
        assert!(
            body.contains(r#""role":"tool","tool_call_id":"c1""#),
            "{body}"
        );
        assert!(!body.contains(r#""tool_call_id":"ghost""#), "{body}");
        assert!(
            body.contains("[Orphan read result; call_id=ghost]: stray"),
            "{body}"
        );

        let responses = request(Family::Responses, "grok-4.6", orphan_context());
        let body = lower(&responses, &openai_plan(), &key, &Integration::Chat)
            .unwrap()
            .body;
        let body = String::from_utf8(body).unwrap();
        assert!(
            body.contains(r#""type":"function_call_output","call_id":"c1""#),
            "{body}"
        );
        assert!(!body.contains(r#""call_id":"ghost""#), "{body}");
    }

    fn sse_bytes(frames: &[&str]) -> Vec<u8> {
        let mut wire = String::new();
        for frame in frames {
            wire.push_str("data: ");
            wire.push_str(frame);
            wire.push_str("\n\n");
        }
        wire.into_bytes()
    }

    fn collect_events(items: Vec<Result<StreamEvent, ProviderError>>) -> Vec<StreamEvent> {
        items.into_iter().map(|item| item.unwrap()).collect()
    }

    #[test]
    fn a_copilot_responses_turn_binds_replay_to_the_long_context_catalog_id() {
        let key = CopilotKey::parse("gho_tok");
        let request = request(Family::Responses, "grok-4.6-1m", vec![user("hi")]);
        let wire = lower(&request, &openai_plan(), &key, &Integration::Chat).unwrap();
        assert_eq!(wire.model.as_ref(), "grok-4.6-1m");

        let reasoning =
            r#"{"type":"reasoning","id":"rs_1","encrypted_content":"ENC","summary":[]}"#;
        let reasoning_done = format!(
            r#"{{"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{reasoning}}}"#
        );
        let frames = [
            r#"{"type":"response.output_text.delta","sequence_number":1,"delta":"hi"}"#,
            reasoning_done.as_str(),
            r#"{"type":"response.completed","sequence_number":3,"response":{"output":[],"usage":{"input_tokens":7,"output_tokens":2}}}"#,
        ];
        let chunks: Vec<Vec<u8>> = sse_bytes(&frames).chunks(13).map(<[u8]>::to_vec).collect();
        let seen = collect_events(block_on(
            responses::decode(
                sse_events(stream::iter(chunks)),
                Family::Responses,
                &*wire.model,
            )
            .collect(),
        ));
        assert!(
            seen.iter()
                .any(|event| matches!(event, StreamEvent::Replay { payload }
                if payload.model.as_ref() == "grok-4.6-1m" && payload.family == Family::Responses)),
            "{seen:?}"
        );
        assert_eq!(
            seen.last(),
            Some(&StreamEvent::Stop {
                reason: StopReason::EndTurn
            })
        );
    }

    #[test]
    fn a_copilot_anthropic_turn_decodes_thinking_replay_and_a_tool_call() {
        let key = CopilotKey::parse("gho_tok");
        let request = request(Family::Anthropic, "claude-opus-4.6-1m", vec![user("hi")]);
        let wire = lower(&request, &anthropic_plan(), &key, &Integration::Chat).unwrap();
        let frames = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG=="}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"get_weather","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Oslo\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let chunks: Vec<Vec<u8>> = sse_bytes(&frames).chunks(9).map(<[u8]>::to_vec).collect();
        let seen = collect_events(block_on(
            anthropic::decode_stream(
                sse_events(stream::iter(chunks)),
                wire.model.clone(),
                false,
                wire.prefix.as_deref(),
            )
            .collect(),
        ));
        assert!(
            seen.iter().any(|event| matches!(event, StreamEvent::Replay { payload }
                if payload.model.as_ref() == "claude-opus-4.6-1m" && payload.family == Family::Anthropic)),
            "replay is bound to the catalog id, not the wire id: {seen:?}"
        );
        let calls = seen.iter().find_map(|event| match event {
            StreamEvent::ToolCallsDone { calls } => Some(calls),
            _ => None,
        });
        let Some([call]) = calls.map(Vec::as_slice) else {
            panic!("one tool call expected: {seen:?}");
        };
        assert_eq!(
            (call.id.as_str(), call.name.as_str()),
            ("toolu_01", "get_weather")
        );
        assert_eq!(call.args, ToolArgs::from_bytes(br#"{"city":"Oslo"}"#));
        assert!(
            seen.iter()
                .any(|event| matches!(event, StreamEvent::Usage { usage }
            if usage.input_tokens == 25))
        );
        assert_eq!(
            seen.last(),
            Some(&StreamEvent::Stop {
                reason: StopReason::ToolUse
            })
        );
    }

    #[test]
    fn the_sign_in_target_follows_the_answer() {
        let public = LoginTarget {
            host: "github.com".into(),
            enterprise_domain: None,
        };
        for answer in [
            "",
            "  ",
            "github.com",
            "https://www.github.com/",
            "API.GITHUB.COM",
        ] {
            assert_eq!(login_target(answer).unwrap(), public, "{answer:?}");
        }
        let corp = LoginTarget {
            host: "corp.ghe.com".into(),
            enterprise_domain: Some("corp.ghe.com".into()),
        };
        for answer in ["corp.ghe.com", "Corp.GHE.com", "https://corp.ghe.com/path"] {
            assert_eq!(login_target(answer).unwrap(), corp, "{answer:?}");
        }
        assert!(matches!(
            login_target("https://").unwrap_err(),
            ProviderError::InvalidRequest { .. }
        ));
    }

    #[test]
    fn the_device_flow_requests_pick_the_client_by_instance() {
        let public = login_target("").unwrap();
        let start = device_code_request(&public);
        assert_eq!(
            start.endpoint().unwrap().as_str(),
            "https://github.com/login/device/code"
        );
        assert_eq!(
            start.body,
            "client_id=Ov23li8tweQw6odWQebz&scope=read%3Auser"
        );
        assert_eq!(start.user_agent, "copilot-developer-action/0.0.1");
        assert_eq!(header(&start.headers, "accept"), Some("application/json"));
        assert_eq!(
            header(&start.headers, "content-type"),
            Some("application/x-www-form-urlencoded")
        );

        let corp = login_target("corp.ghe.com").unwrap();
        let poll = access_token_request(&corp, "dc 1");
        assert_eq!(
            poll.endpoint().unwrap().as_str(),
            "https://corp.ghe.com/login/oauth/access_token"
        );
        assert_eq!(
            poll.body,
            "client_id=Ov23ctDVkRmgkPke0Mmm&device_code=dc+1&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
        );
        assert!(!format!("{poll:?}").contains("dc+1"));
    }

    #[test]
    fn a_device_code_response_needs_every_field_and_an_https_page() {
        let ok = decode_device_code(
            br#"{"device_code":"dc","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","interval":5,"expires_in":900}"#,
        )
        .unwrap();
        assert_eq!(ok.device_code.expose(), "dc");
        assert_eq!(&*ok.user_code, "ABCD-1234");
        assert_eq!(&*ok.verification_uri, "https://github.com/login/device");
        assert_eq!(ok.interval, Duration::from_secs(5));
        assert_eq!(ok.expires_in, Duration::from_mins(15));
        assert!(
            !format!("{ok:?}").contains("\"dc\""),
            "the device code is redacted"
        );

        for body in [
            r#"{"device_code":"dc","user_code":"U","verification_uri":"https://x","interval":5}"#,
            r#"{"device_code":"","user_code":"U","verification_uri":"https://x","interval":5,"expires_in":9}"#,
            r#"{"device_code":"dc","user_code":"U","verification_uri":"http://x","interval":5,"expires_in":9}"#,
            r#"{"error":"bad_client"}"#,
            "not json",
        ] {
            let error = decode_device_code(body.as_bytes()).unwrap_err();
            assert!(
                matches!(error, ProviderError::DeviceCode { .. }),
                "{body}: {error:?}"
            );
        }
    }

    #[test]
    fn a_poll_answer_is_a_token_a_wait_a_slower_wait_or_the_end_of_the_sign_in() {
        let Ok(Poll::Token(token)) =
            decode_poll(br#"{"access_token":"gho_new","token_type":"bearer"}"#)
        else {
            panic!("a token answer");
        };
        assert_eq!(token.expose(), "gho_new");
        assert!(matches!(
            decode_poll(br#"{"error":"authorization_pending"}"#),
            Ok(Poll::Pending)
        ));
        assert!(matches!(
            decode_poll(br#"{"error":"slow_down","interval":10}"#),
            Ok(Poll::SlowDown { interval: Some(wait) }) if wait == Duration::from_secs(10)
        ));
        assert!(matches!(
            decode_poll(br#"{"error":"slow_down"}"#),
            Ok(Poll::SlowDown { interval: None })
        ));
        let Err(ProviderError::DeviceCode { message, .. }) = decode_poll(
            br#"{"error":"expired_token","error_description":"The device code has expired."}"#,
        ) else {
            panic!("an expired code ends the sign-in");
        };
        assert_eq!(message, "expired_token: The device code has expired.");
        let Err(ProviderError::DeviceCode { message, .. }) =
            decode_poll(br#"{"error":"access_denied"}"#)
        else {
            panic!("a denial ends the sign-in");
        };
        assert_eq!(message, "access_denied");
        for body in [&br"{}"[..], br#"{"access_token":""}"#, b"nope"] {
            assert!(matches!(
                decode_poll(body),
                Err(ProviderError::DeviceCode { .. })
            ));
        }
    }

    #[test]
    fn the_poll_pacing_never_outruns_the_server_and_stays_bounded() {
        let mut pacing = Pacing::new(Duration::from_secs(5));
        near_duration(pacing.wait(), Duration::from_secs(6));
        pacing.slow_down(None);
        near_duration(pacing.wait(), Duration::from_secs(14));
        pacing.slow_down(Some(Duration::from_secs(30)));
        near_duration(pacing.wait(), Duration::from_secs(42));
        near_duration(
            Pacing::new(Duration::ZERO).wait(),
            Duration::from_millis(1200),
        );
        near_duration(
            Pacing::new(Duration::from_secs(u64::MAX)).wait(),
            Duration::from_secs(360),
        );
        let mut huge = Pacing::new(Duration::from_secs(5));
        huge.slow_down(Some(Duration::from_secs(u64::MAX)));
        near_duration(huge.wait(), Duration::from_secs(420));
    }

    #[test]
    fn a_finished_sign_in_stores_a_key_the_provider_reads_back() {
        let token = SecretString::from("gho_tok");
        let stored = stored_key(
            &token,
            Some("corp.ghe.com"),
            Some("https://copilot-api.corp.ghe.com"),
        )
        .unwrap();
        let key = CopilotKey::parse(stored.expose());
        assert_eq!(key.token(), "gho_tok");
        assert!(key.enterprise());
        assert_eq!(
            resolve_base_url(PERSONAL_BASE_URL, &key),
            "https://copilot-api.corp.ghe.com"
        );

        let plain = stored_key(&token, None, None).unwrap();
        assert_eq!(plain.expose(), r#"{"token":"gho_tok"}"#);
        let key = CopilotKey::parse(plain.expose());
        assert_eq!((key.token(), key.enterprise()), ("gho_tok", false));
        assert!(!format!("{stored:?}").contains("gho_tok"));
    }
}
