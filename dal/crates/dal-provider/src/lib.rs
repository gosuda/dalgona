//! Provider transport values and SSE framing for dal.

mod auth;
mod catalog;
mod claude_fingerprint;
mod compact;
mod error;
mod family;
mod http;
mod lifecycle;
mod provider;
mod retry;
mod scripted;
mod sse;
mod stream;
mod thinking;
mod tool_names;
mod usage;
mod ws;

pub use auth::credential::{
    AuthStatus, AuthStore, CodexIdentity, Credential, CredentialKind, EnvSnapshot, OAuthCredential,
    SecretString, codex_identity, oauth_expires_at, resolve as resolve_credential,
};
pub use auth::login::{
    LoginIo, LoginSite, Method, PROGRESS_CAPACITY, StoredCredential, login, login_providers,
    sign_out, stored_credentials,
};
pub use auth::oauth::{
    LoginEndpoints, LoginFlow, LoginProgress, PASTE_HINT, logout, logout_with, store_api_key,
};
pub use auth::refresh::{
    OAuthProvider, PROACTIVE_WINDOW_SECS, RETRY_DELAY, RefreshReason, Refresher, TokenEndpoints,
};
pub use catalog::{
    Catalog, CatalogEntry, CatalogFetch, CatalogSource, ImageProfile, Listing, ModelFetch,
    ResolvedModel, ToolSupport, built_in_entries, compiled_price, compiled_temperature,
    load_models, price_source, resolve, resolve_route,
};
pub use compact::{CompactOutcome, CompactedHistory, items_for};
pub use error::{LimitError, ProviderError, ResolveError, UsageCheckReason};
pub use http::{
    BODY_LIMIT, COMPACT_TIMEOUT, CONNECT_TIMEOUT, Exchange, LOGIN_WAIT, NON_STREAM_TOTAL_TIMEOUT,
    OAUTH_TIMEOUT, RESPONSE_HEADER_TIMEOUT, STREAM_IDLE_TIMEOUT, USAGE_TIMEOUT, WS_MESSAGE_LIMIT,
    build_client, check_base_url, endpoint, read_body, send, user_agent,
};
pub use provider::{
    AuthStyle, Http, Provider, ProviderConfig, ProviderConfigError, ProviderEntry,
    ProviderIdentity, ProviderSet, ScriptedSelection, Transport,
};
pub use retry::{
    MAX_RETRY_WAIT, RequestState, RetryAfterTooLong, RetryDecision, classify, delay_for_attempt,
};
pub use scripted::{Operation, Script, ScriptError, ScriptStep, StepKind};
pub use sse::{SSE_EVENT_LIMIT, SSE_LINE_LIMIT, SseEvent, decode_stream, encode};
pub use stream::{
    EventStream, NoticeSink, ReplayPayload, StopReason, StreamEvent, ToolArgs, ToolCall,
};
pub use thinking::{
    AnthropicThinking, BeforeRequest, BeforeRequestInput, BeforeRequestPatch, CLAMP_NOTICE,
    DEFAULT_LEVEL, Effort, RequestLimits, SessionNotices, TEMPERATURE_NOTICE, ThinkingNotice,
    ThinkingPlan, ThinkingSupport, WireThinking, clamp, compose_hooks, level_name, levels_for,
    parse_level, plan,
};
pub use usage::{Cta, LUNA_RESERVE_DISPLAY, LUNA_RESERVE_MODEL, Offer, UsageVerdict};
