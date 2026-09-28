//! Typed provider failures, their final display texts, and next-action fixes.
//!
//! Every variant stores the data it was given. [`ProviderError`] rendering
//! reads that data and never rewrites it: a server message appears as its
//! first line cut at 300 bytes on a UTF-8 boundary, and an empty message
//! renders as empty text. [`ProviderError::fix`] returns at most one
//! next-action sentence, and [`ProviderError::retryable_by_loop`] names the
//! failures an outer loop may replay after delivery. No text produced here
//! carries a token, a key, or an authorization header value.

use std::{error::Error, fmt, path::PathBuf, time::Duration};

use dal_core::{Family, InferFailure};

const MESSAGE_LINE_LIMIT: usize = 300;
const THINKING_LEVEL_NAMES: &str = "off, minimal, low, medium, high, xhigh, max";

/// Provider error codes that mean the request exceeds the model's context
/// window: `context_length_exceeded` (`OpenAI` Chat and Responses) and
/// `context_window_exceeded` (Codex). Anthropic reports an oversized prompt as
/// a plain `invalid_request_error`, so no Anthropic code is listed.
const CONTEXT_OVERFLOW_CODES: [&str; 2] = ["context_length_exceeded", "context_window_exceeded"];

fn family_label(family: Family) -> &'static str {
    match family {
        Family::Chat | Family::Responses => "openai",
        Family::Codex => "codex",
        Family::Anthropic => "anthropic",
    }
}

fn capped_message_line(message: &str) -> &str {
    let end = message.find(['\n', '\r']).unwrap_or(message.len());
    let line = &message[..end];
    if line.len() <= MESSAGE_LINE_LIMIT {
        return line;
    }
    let mut cut = MESSAGE_LINE_LIMIT;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    &line[..cut]
}

/// A failure of one provider request, credential file, sign-in flow, or model
/// resolution, with the exact text the caller may show.
///
/// Where present, family and provider fields keep the identifiers the request used.
/// Messages are stored verbatim; only rendering truncates them.
#[non_exhaustive]
#[derive(Debug)]
pub enum ProviderError {
    /// The connection, socket, or body transport failed.
    Transport {
        /// The provider API family of the request.
        family: Family,
        /// The transport-level failure text.
        reason: String,
    },
    /// The provider answered with an HTTP error status.
    Status {
        /// The provider API family of the request.
        family: Family,
        /// The HTTP status code.
        status: u16,
        /// The server message, stored verbatim.
        message: String,
    },
    /// The provider rejected the request body or parameters.
    InvalidRequest {
        /// The server message.
        message: String,
    },
    /// The provider rejected the request, by a typed error code, because it
    /// exceeds the model's context window.
    ContextOverflow {
        /// The provider API family of the request.
        family: Family,
        /// The provider error code, one of the known overflow codes.
        code: String,
        /// The server message, stored verbatim.
        message: String,
    },
    /// The requested thinking-level spelling is not supported.
    InvalidThinkingLevel {
        /// The spelling that was rejected.
        spelling: String,
    },
    /// The provider rate-limited the request and the wait budget ran out.
    RateLimited {
        /// The server message.
        message: String,
        /// The wait the final 429's `Retry-After` asked for, within the 60 s
        /// budget; `None` when that response named no usable wait.
        retry_after: Option<Duration>,
    },
    /// The requested wait is over the 60 s budget.
    RetryAfterTooLong {
        /// The wait the provider asked for, in seconds.
        seconds: u64,
        /// The server message.
        message: String,
    },
    /// The account quota or credit balance is exhausted.
    Quota {
        /// The server message.
        message: String,
    },
    /// The provider reports that its servers are overloaded.
    Overloaded,
    /// The provider rejected the API key.
    AuthRejected {
        /// The provider id the request named.
        provider: String,
    },
    /// The refresh token of the stored sign-in was rejected.
    SignInExpired {
        /// The provider id the request named.
        provider: String,
    },
    /// No credential is available for the provider.
    NoCredentials {
        /// The provider id the request named.
        provider: String,
    },
    /// The stream ended before the response was complete.
    StreamCut,
    /// The WebSocket closed before the terminal response event.
    WsClosed {
        /// The close code and reason, when the frame carried one.
        code: Option<(u16, String)>,
    },
    /// The provider sent a stream that violates its own grammar.
    Protocol {
        /// The provider API family of the stream.
        family: Family,
        /// The violated rule, in one short sentence.
        detail: String,
    },
    /// A protocol or body size limit was exceeded.
    Limit(LimitError),
    /// Plain HTTP is refused for a non-loopback host.
    PlainHttp {
        /// The host the request targeted.
        host: String,
    },
    /// A blob part of the request was not read from the session store before
    /// the request was built. Raised locally; no request is sent.
    UnresolvedBlob {
        /// The identity of the unread blob.
        blob_id: dal_core::BlobId,
    },
    /// Remote compaction returned no compaction item or block.
    CompactionMissing {
        /// The provider API family of the compaction request.
        family: Family,
        /// `item` for Codex, `block` for Anthropic.
        noun: &'static str,
    },
    /// The compacted history belongs to a different family or model.
    CompactionForeign {
        /// The family the compacted history is bound to.
        bound_family: Family,
        /// The model the compacted history is bound to.
        bound_model: String,
        /// The family of the current request.
        family: Family,
        /// The model of the current request.
        model: String,
    },
    /// The plan's usage limit for the model is reached.
    UsageLimit {
        /// The provider model id of the request.
        model: String,
        /// The server message.
        message: String,
    },
    /// The `ChatGPT` plan does not include Codex usage.
    UsageNotIncluded {
        /// The server message.
        message: String,
    },
    /// Luna Reserve is not available for this account.
    ReserveUnavailable {
        /// The HTTP status code that carried the refusal.
        status: u16,
        /// The server message.
        message: String,
    },
    /// The model reference does not resolve to any known model.
    UnknownModel {
        /// The model reference the caller named.
        reference: String,
    },
    /// A bare model id is listed by several providers.
    AmbiguousModel {
        /// The bare model id.
        id: String,
        /// The matching provider and model pairs, joined in catalog order.
        candidates: String,
    },
    /// `auth.json` has group or other permissions.
    AuthFilePerms {
        /// The credential file path.
        path: PathBuf,
    },
    /// `auth.json` is a symbolic link.
    AuthFileSymlink {
        /// The credential file path.
        path: PathBuf,
    },
    /// `auth.json` is not valid.
    AuthFileInvalid {
        /// The credential file path.
        path: PathBuf,
        /// The validation failure.
        message: String,
    },
    /// Writing `auth.json` failed.
    AuthWrite {
        /// The write failure.
        reason: String,
    },
    /// Binding the sign-in callback listener failed.
    CallbackBind {
        /// The loopback port.
        port: u16,
        /// The bind failure.
        reason: String,
    },
    /// The OAuth callback state does not match the pending sign-in.
    StateMismatch,
    /// No sign-in callback arrived within 15 minutes.
    LoginTimeout,
    /// The token endpoint rejected the exchange.
    TokenExchange {
        /// The HTTP status code.
        status: u16,
        /// The server message.
        message: String,
    },
    /// The device-code endpoint failed.
    DeviceCode {
        /// The HTTP status code.
        status: u16,
        /// The server message.
        message: String,
    },
    /// The ID token has no `chatgpt_account_id` claim.
    NoAccountId,
    /// The sign-in was cancelled before completion.
    LoginCancelled,
    /// The account usage read failed.
    UsageCheck {
        /// Why the usage read failed.
        reason: UsageCheckReason,
    },
    /// The scripted provider cannot serve the operation: its script is
    /// exhausted, the next step is for another operation, or the script
    /// breaks the stream grammar.
    Script(crate::scripted::ScriptError),
}

impl ProviderError {
    /// The typed overflow error for a provider error `code`, or `None` when
    /// `code` is not a context-overflow code.
    ///
    /// Matching is exact on the code; the message is never inspected, so an
    /// ordinary invalid request is never mistaken for an overflow.
    #[must_use]
    pub fn context_overflow(family: Family, code: &str, message: &str) -> Option<Self> {
        CONTEXT_OVERFLOW_CODES
            .contains(&code)
            .then(|| Self::ContextOverflow {
                family,
                code: String::from(code),
                message: String::from(message),
            })
    }

    /// The one next-action sentence for this failure, when one exists.
    #[must_use]
    pub fn fix(&self) -> Option<String> {
        match self {
            Self::AuthRejected { provider }
            | Self::SignInExpired { provider }
            | Self::NoCredentials { provider } => Some(format!("Run dalgon login {provider}.")),
            Self::UsageLimit { .. } => Some(String::from(
                "Wait for the limit to reset, or add credits at https://chatgpt.com/codex/settings/usage?credits_modal=true.",
            )),
            Self::UsageNotIncluded { .. } => Some(String::from(
                "Upgrade the ChatGPT plan at https://chatgpt.com/explore/plus, or run dalgon login openai-codex with another account.",
            )),
            Self::ReserveUnavailable { .. } => Some(String::from(
                "Luna Reserve opens only when the included usage of your ChatGPT plan runs out. Switch to another model to continue.",
            )),
            Self::UnknownModel { .. } => {
                Some(String::from("Run dalgon models to list the models."))
            }
            Self::AmbiguousModel { candidates, .. } => {
                let first = candidates.split(", ").next().unwrap_or_default();
                Some(format!("Use {first}."))
            }
            Self::AuthFilePerms { path } => Some(format!("Run chmod 600 {}.", path.display())),
            Self::AuthFileSymlink { path } => {
                Some(format!("Replace {} with a regular file.", path.display()))
            }
            Self::AuthFileInvalid { path, .. } => Some(format!(
                "Fix or delete {}, then run dalgon login.",
                path.display()
            )),
            _ => None,
        }
    }

    /// Whether the outer loop may replay the request after delivery.
    ///
    /// The request lifecycle consults this only for failures after the first
    /// delivered event; before delivery the lifecycle owns its own retries.
    #[must_use]
    pub fn retryable_by_loop(&self) -> bool {
        matches!(
            self,
            Self::StreamCut | Self::WsClosed { .. } | Self::Overloaded | Self::Transport { .. }
        )
    }
}

impl fmt::Display for ProviderError {
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per public error variant keeps every final text in one place"
    )]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { family, reason } => {
                write!(f, "{} request failed: {reason}", family_label(*family))
            }
            Self::Status {
                family,
                status,
                message,
            } => write!(
                f,
                "{} error {status}: {}",
                family_label(*family),
                capped_message_line(message)
            ),
            Self::InvalidRequest { message } => write!(f, "invalid request: {message}"),
            Self::ContextOverflow {
                family,
                code,
                message,
            } => write!(
                f,
                "{} context window exceeded ({code}): {}",
                family_label(*family),
                capped_message_line(message)
            ),
            Self::InvalidThinkingLevel { spelling } => write!(
                f,
                "unknown thinking level \"{spelling}\"; use one of {THINKING_LEVEL_NAMES}."
            ),
            Self::RateLimited { message, .. } => write!(f, "rate limited: {message}"),
            Self::RetryAfterTooLong { seconds, message } => write!(
                f,
                "rate limited for {seconds} s, which is over the 60 s wait limit: {message}"
            ),
            Self::Quota { message } => write!(f, "quota exhausted: {message}"),
            Self::Overloaded => f.write_str("server overloaded."),
            Self::AuthRejected { provider } => write!(f, "{provider} rejected the API key."),
            Self::SignInExpired { provider } => write!(
                f,
                "{provider} sign-in expired: the refresh token was rejected."
            ),
            Self::NoCredentials { provider } => write!(
                f,
                "{provider} has no credentials: auth.json has no entry for it"
            ),
            Self::StreamCut => f.write_str("stream cut off before completion."),
            Self::WsClosed { code } => match code {
                None => f.write_str("websocket closed by server before response.completed."),
                Some((code, reason)) => write!(
                    f,
                    "websocket closed by server before response.completed. (code {code}: {reason})"
                ),
            },
            Self::Protocol { family, detail } => {
                write!(
                    f,
                    "{} sent an invalid stream: {detail}",
                    family_label(*family)
                )
            }
            Self::Limit(error) => write!(f, "{error}"),
            Self::PlainHttp { host } => {
                write!(f, "refusing plain http for non-loopback host {host}.")
            }
            Self::CompactionMissing { family, noun } => write!(
                f,
                "{} compaction returned no compaction {noun}.",
                family_label(*family)
            ),
            Self::CompactionForeign {
                bound_family,
                bound_model,
                family,
                model,
            } => write!(
                f,
                "the compacted history belongs to {}/{bound_model}; this request uses {}/{model}.",
                family_label(*bound_family),
                family_label(*family),
            ),
            Self::UsageLimit { model, message } => {
                let head = if model == "gpt-reserve" {
                    "Luna Reserve usage limit reached"
                } else {
                    "usage limit reached"
                };
                write!(f, "{head}: {message}")
            }
            Self::UsageNotIncluded { message } => {
                write!(
                    f,
                    "this ChatGPT plan does not include Codex usage: {message}"
                )
            }
            Self::ReserveUnavailable { message, .. } => {
                write!(
                    f,
                    "Luna Reserve is not available for this account: {message}"
                )
            }
            Self::UnknownModel { reference } => write!(f, "unknown model {reference}"),
            Self::AmbiguousModel { id, candidates } => {
                write!(f, "model id {id} matches several providers: {candidates}")
            }
            Self::AuthFilePerms { .. } => f.write_str("auth.json has group or other permissions"),
            Self::AuthFileSymlink { .. } => {
                f.write_str("auth.json is a symbolic link; dalgon does not follow it")
            }
            Self::AuthFileInvalid { message, .. } => {
                write!(f, "auth.json is not valid: {message}")
            }
            Self::AuthWrite { reason } => write!(f, "could not write auth.json: {reason}"),
            Self::CallbackBind { port, reason } => {
                write!(f, "could not listen on 127.0.0.1:{port}: {reason}")
            }
            Self::StateMismatch => f.write_str("sign-in failed: the OAuth state does not match."),
            Self::LoginTimeout => f.write_str("sign-in timed out after 15 minutes."),
            Self::TokenExchange { status, message } => {
                write!(
                    f,
                    "sign-in failed: the token endpoint returned {status}: {message}"
                )
            }
            Self::DeviceCode { status, message } => write!(
                f,
                "sign-in failed: the device code endpoint returned {status}: {message}"
            ),
            Self::NoAccountId => {
                f.write_str("sign-in failed: the ID token has no chatgpt_account_id.")
            }
            Self::LoginCancelled => f.write_str("sign-in cancelled."),
            Self::UsageCheck { reason } => write!(f, "usage failed: {reason}"),
            Self::UnresolvedBlob { blob_id } => write!(
                f,
                "blob {blob_id} was not read from the session store before the request; no request was sent."
            ),
            Self::Script(error) => write!(f, "{error}"),
        }
    }
}

impl Error for ProviderError {}

/// Classifies a final provider failure for the fold.
///
/// [`ProviderError::ContextOverflow`] becomes [`InferFailure::Overflow`] with
/// its code. Rate limits, overload, stream cuts (idle stalls included),
/// WebSocket closes, and transport failures (read timeouts included) become
/// [`InferFailure::Retryable`]. Only a rate limit carries a hint: the wait
/// its final 429 asked for; every other retryable failure has none. A refused
/// `Retry-After` over 60 s ([`ProviderError::RetryAfterTooLong`]) fails at
/// once and is [`InferFailure::Fatal`], like every other failure, with its
/// fix. Each message is the error's display text.
impl From<ProviderError> for InferFailure {
    fn from(error: ProviderError) -> Self {
        use ProviderError as E;

        let message = error.to_string().into_boxed_str();
        let fix = error.fix().map(String::into_boxed_str);
        // Exhaustive on purpose: a new variant must choose its class here.
        match error {
            E::ContextOverflow { code, .. } => Self::Overflow {
                code: code.into_boxed_str(),
                message,
            },
            E::RetryAfterTooLong { .. } => Self::Fatal { message, fix },
            E::RateLimited { retry_after, .. } => Self::Retryable {
                hint: retry_after,
                message,
            },
            E::Overloaded | E::StreamCut | E::WsClosed { .. } | E::Transport { .. } => {
                Self::Retryable {
                    hint: None,
                    message,
                }
            }
            E::Status { .. }
            | E::InvalidRequest { .. }
            | E::InvalidThinkingLevel { .. }
            | E::Quota { .. }
            | E::AuthRejected { .. }
            | E::SignInExpired { .. }
            | E::NoCredentials { .. }
            | E::Protocol { .. }
            | E::Limit(_)
            | E::PlainHttp { .. }
            | E::CompactionMissing { .. }
            | E::CompactionForeign { .. }
            | E::UsageLimit { .. }
            | E::UsageNotIncluded { .. }
            | E::ReserveUnavailable { .. }
            | E::UnknownModel { .. }
            | E::AmbiguousModel { .. }
            | E::AuthFilePerms { .. }
            | E::AuthFileSymlink { .. }
            | E::AuthFileInvalid { .. }
            | E::AuthWrite { .. }
            | E::CallbackBind { .. }
            | E::StateMismatch
            | E::LoginTimeout
            | E::TokenExchange { .. }
            | E::DeviceCode { .. }
            | E::NoAccountId
            | E::LoginCancelled
            | E::UsageCheck { .. }
            | E::UnresolvedBlob { .. }
            | E::Script(_) => Self::Fatal { message, fix },
        }
    }
}

/// A protocol or body size limit that was exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitError {
    /// One SSE line grew past 1 MiB.
    SseLine,
    /// One SSE event grew past 8 MiB.
    SseEvent,
    /// One WebSocket message grew past 16 MiB.
    WsMessage,
    /// One non-streaming response body grew past 16 MiB.
    Body,
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SseLine => f.write_str("SSE line exceeds 1 MiB."),
            Self::SseEvent => f.write_str("SSE event exceeds 8 MiB."),
            Self::WsMessage => f.write_str("WebSocket message exceeds 16 MiB."),
            Self::Body => f.write_str("response body exceeds 16 MiB."),
        }
    }
}

impl Error for LimitError {}

/// Why an account usage read failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageCheckReason {
    /// The usage endpoint did not reply within 15 s.
    Timeout,
    /// The usage body is not a JSON object.
    NotJsonObject,
    /// The usage endpoint answered with an HTTP error status.
    Status {
        /// The HTTP status code.
        status: u16,
        /// The server message, stored verbatim.
        message: String,
    },
    /// The usage request failed before a complete reply: a connect, TLS, or
    /// I/O failure, a body over the size limit, or the request task was
    /// cancelled. The reason never carries a token or an authorization value.
    Transport {
        /// The redacted transport failure.
        reason: String,
    },
}

impl fmt::Display for UsageCheckReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("no reply within 15 s"),
            Self::NotJsonObject => f.write_str("the body is not a JSON object"),
            Self::Status { status, message } => {
                write!(f, "{status} {}", capped_message_line(message))
            }
            Self::Transport { reason } => f.write_str(capped_message_line(reason)),
        }
    }
}

impl Error for UsageCheckReason {}

/// A model reference that did not resolve to exactly one model.
///
/// [`ResolveError::NoDefault`] has no user-facing wording here: the CLI part
/// owns the sentence for a headless run without a model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    /// The model reference matches no known model.
    UnknownModel {
        /// The model reference the caller named.
        reference: String,
    },
    /// A bare model id is listed by several providers.
    AmbiguousModel {
        /// The bare model id.
        id: String,
        /// The matching provider and model pairs, joined in catalog order.
        candidates: String,
    },
    /// No model was given and no default model is configured.
    NoDefault,
}

impl ResolveError {
    /// The one next-action sentence for this failure, when one exists.
    #[must_use]
    pub fn fix(&self) -> Option<String> {
        match self {
            Self::UnknownModel { .. } => {
                Some(String::from("Run dalgon models to list the models."))
            }
            Self::AmbiguousModel { candidates, .. } => {
                let first = candidates.split(", ").next().unwrap_or_default();
                Some(format!("Use {first}."))
            }
            Self::NoDefault => None,
        }
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModel { reference } => write!(f, "unknown model {reference}"),
            Self::AmbiguousModel { id, candidates } => {
                write!(f, "model id {id} matches several providers: {candidates}")
            }
            Self::NoDefault => f.write_str("no default model is configured"),
        }
    }
}

impl Error for ResolveError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[expect(
        clippy::too_many_lines,
        reason = "one row per public error variant pins its exact display text and fix"
    )]
    #[test]
    fn error_display_texts() {
        let cases: &[(ProviderError, &str, Option<&str>)] = &[
            (
                ProviderError::Transport {
                    family: Family::Chat,
                    reason: String::from("connection refused"),
                },
                "openai request failed: connection refused",
                None,
            ),
            (
                ProviderError::Status {
                    family: Family::Responses,
                    status: 429,
                    message: String::from("slow down"),
                },
                "openai error 429: slow down",
                None,
            ),
            (
                ProviderError::InvalidRequest {
                    message: String::from("bad tool"),
                },
                "invalid request: bad tool",
                None,
            ),
            (
                ProviderError::InvalidThinkingLevel {
                    spelling: String::from("Ultra"),
                },
                "unknown thinking level \"Ultra\"; use one of off, minimal, low, medium, high, xhigh, max.",
                None,
            ),
            (
                ProviderError::RateLimited {
                    message: String::from("try later"),
                    retry_after: Some(Duration::from_secs(3)),
                },
                "rate limited: try later",
                None,
            ),
            (
                ProviderError::RetryAfterTooLong {
                    seconds: 120,
                    message: String::from("busy"),
                },
                "rate limited for 120 s, which is over the 60 s wait limit: busy",
                None,
            ),
            (
                ProviderError::Quota {
                    message: String::from("no credit"),
                },
                "quota exhausted: no credit",
                None,
            ),
            (ProviderError::Overloaded, "server overloaded.", None),
            (
                ProviderError::AuthRejected {
                    provider: String::from("anthropic"),
                },
                "anthropic rejected the API key.",
                Some("Run dalgon login anthropic."),
            ),
            (
                ProviderError::SignInExpired {
                    provider: String::from("openai-codex"),
                },
                "openai-codex sign-in expired: the refresh token was rejected.",
                Some("Run dalgon login openai-codex."),
            ),
            (
                ProviderError::NoCredentials {
                    provider: String::from("openai"),
                },
                "openai has no credentials: auth.json has no entry for it",
                Some("Run dalgon login openai."),
            ),
            (
                ProviderError::StreamCut,
                "stream cut off before completion.",
                None,
            ),
            (
                ProviderError::WsClosed { code: None },
                "websocket closed by server before response.completed.",
                None,
            ),
            (
                ProviderError::Protocol {
                    family: Family::Codex,
                    detail: String::from("binary WebSocket frame"),
                },
                "codex sent an invalid stream: binary WebSocket frame",
                None,
            ),
            (
                ProviderError::Limit(LimitError::SseLine),
                "SSE line exceeds 1 MiB.",
                None,
            ),
            (
                ProviderError::Limit(LimitError::SseEvent),
                "SSE event exceeds 8 MiB.",
                None,
            ),
            (
                ProviderError::Limit(LimitError::WsMessage),
                "WebSocket message exceeds 16 MiB.",
                None,
            ),
            (
                ProviderError::Limit(LimitError::Body),
                "response body exceeds 16 MiB.",
                None,
            ),
            (
                ProviderError::PlainHttp {
                    host: String::from("example.com"),
                },
                "refusing plain http for non-loopback host example.com.",
                None,
            ),
            (
                ProviderError::CompactionMissing {
                    family: Family::Codex,
                    noun: "item",
                },
                "codex compaction returned no compaction item.",
                None,
            ),
            (
                ProviderError::CompactionMissing {
                    family: Family::Anthropic,
                    noun: "block",
                },
                "anthropic compaction returned no compaction block.",
                None,
            ),
            (
                ProviderError::CompactionForeign {
                    bound_family: Family::Anthropic,
                    bound_model: String::from("claude-sonnet-5"),
                    family: Family::Responses,
                    model: String::from("gpt-5.6-luna"),
                },
                "the compacted history belongs to anthropic/claude-sonnet-5; this request uses openai/gpt-5.6-luna.",
                None,
            ),
            (
                ProviderError::UsageLimit {
                    model: String::from("gpt-reserve"),
                    message: String::from("try tomorrow"),
                },
                "Luna Reserve usage limit reached: try tomorrow",
                Some(
                    "Wait for the limit to reset, or add credits at https://chatgpt.com/codex/settings/usage?credits_modal=true.",
                ),
            ),
            (
                ProviderError::UsageNotIncluded {
                    message: String::from("plan"),
                },
                "this ChatGPT plan does not include Codex usage: plan",
                Some(
                    "Upgrade the ChatGPT plan at https://chatgpt.com/explore/plus, or run dalgon login openai-codex with another account.",
                ),
            ),
            (
                ProviderError::ReserveUnavailable {
                    status: 404,
                    message: String::from("no such model"),
                },
                "Luna Reserve is not available for this account: no such model",
                Some(
                    "Luna Reserve opens only when the included usage of your ChatGPT plan runs out. Switch to another model to continue.",
                ),
            ),
            (
                ProviderError::UnknownModel {
                    reference: String::from("openai/gpt-reserve"),
                },
                "unknown model openai/gpt-reserve",
                Some("Run dalgon models to list the models."),
            ),
            (
                ProviderError::AmbiguousModel {
                    id: String::from("gpt-5.6-luna"),
                    candidates: String::from("p1/gpt-5.6-luna, p2/gpt-5.6-luna"),
                },
                "model id gpt-5.6-luna matches several providers: p1/gpt-5.6-luna, p2/gpt-5.6-luna",
                Some("Use p1/gpt-5.6-luna."),
            ),
            (
                ProviderError::AuthFilePerms {
                    path: PathBuf::from("/data/auth.json"),
                },
                "auth.json has group or other permissions",
                Some("Run chmod 600 /data/auth.json."),
            ),
            (
                ProviderError::AuthFileSymlink {
                    path: PathBuf::from("/data/auth.json"),
                },
                "auth.json is a symbolic link; dalgon does not follow it",
                Some("Replace /data/auth.json with a regular file."),
            ),
            (
                ProviderError::AuthFileInvalid {
                    path: PathBuf::from("/data/auth.json"),
                    message: String::from("trailing bytes"),
                },
                "auth.json is not valid: trailing bytes",
                Some("Fix or delete /data/auth.json, then run dalgon login."),
            ),
            (
                ProviderError::AuthWrite {
                    reason: String::from("disk full"),
                },
                "could not write auth.json: disk full",
                None,
            ),
            (
                ProviderError::CallbackBind {
                    port: 7437,
                    reason: String::from("address in use"),
                },
                "could not listen on 127.0.0.1:7437: address in use",
                None,
            ),
            (
                ProviderError::StateMismatch,
                "sign-in failed: the OAuth state does not match.",
                None,
            ),
            (
                ProviderError::LoginTimeout,
                "sign-in timed out after 15 minutes.",
                None,
            ),
            (
                ProviderError::TokenExchange {
                    status: 400,
                    message: String::from("bad code"),
                },
                "sign-in failed: the token endpoint returned 400: bad code",
                None,
            ),
            (
                ProviderError::DeviceCode {
                    status: 500,
                    message: String::from("boom"),
                },
                "sign-in failed: the device code endpoint returned 500: boom",
                None,
            ),
            (
                ProviderError::NoAccountId,
                "sign-in failed: the ID token has no chatgpt_account_id.",
                None,
            ),
            (ProviderError::LoginCancelled, "sign-in cancelled.", None),
            (
                ProviderError::UsageCheck {
                    reason: UsageCheckReason::Timeout,
                },
                "usage failed: no reply within 15 s",
                None,
            ),
            (
                ProviderError::UsageCheck {
                    reason: UsageCheckReason::NotJsonObject,
                },
                "usage failed: the body is not a JSON object",
                None,
            ),
            (
                ProviderError::UsageCheck {
                    reason: UsageCheckReason::Status {
                        status: 401,
                        message: String::from("denied"),
                    },
                },
                "usage failed: 401 denied",
                None,
            ),
            (
                ProviderError::ContextOverflow {
                    family: Family::Responses,
                    code: String::from("context_length_exceeded"),
                    message: String::from("input exceeds the window\ndetail"),
                },
                "openai context window exceeded (context_length_exceeded): input exceeds the window",
                None,
            ),
        ];
        for (error, display, fix) in cases {
            assert_eq!(error.to_string(), *display);
            assert_eq!(error.fix().as_deref(), *fix);
        }
    }

    #[test]
    fn status_message_first_line_and_utf8_cap() {
        let message = format!("{}\u{e9}{}\nsecond line", "a".repeat(299), "b".repeat(10));
        let error = ProviderError::Status {
            family: Family::Chat,
            status: 429,
            message: message.clone(),
        };
        let expected = format!("openai error 429: {}", "a".repeat(299));
        assert_eq!(error.to_string(), expected);
        assert_eq!(error.to_string(), expected);
        match &error {
            ProviderError::Status {
                message: stored, ..
            } => assert_eq!(stored, &message),
            other => panic!("unexpected variant {other:?}"),
        }
    }

    #[test]
    fn status_message_at_the_byte_cap_stays_whole() {
        let error = ProviderError::Status {
            family: Family::Chat,
            status: 429,
            message: format!("{}zzz\nmore", "a".repeat(300)),
        };
        assert_eq!(
            error.to_string(),
            format!("openai error 429: {}", "a".repeat(300))
        );
    }

    #[test]
    fn empty_messages_stay_empty() {
        let status = ProviderError::Status {
            family: Family::Responses,
            status: 401,
            message: String::new(),
        };
        assert_eq!(status.to_string(), "openai error 401: ");
        let reason = UsageCheckReason::Status {
            status: 401,
            message: String::new(),
        };
        assert_eq!(reason.to_string(), "401 ");
    }

    #[test]
    fn ws_closed_renders_only_the_close_code_it_carries() {
        let bare = ProviderError::WsClosed { code: None };
        assert_eq!(
            bare.to_string(),
            "websocket closed by server before response.completed."
        );
        let coded = ProviderError::WsClosed {
            code: Some((1011, String::from("busy"))),
        };
        assert_eq!(
            coded.to_string(),
            "websocket closed by server before response.completed. (code 1011: busy)"
        );
    }

    #[test]
    fn usage_limit_head_names_luna_reserve_for_gpt_reserve() {
        let reserve = ProviderError::UsageLimit {
            model: String::from("gpt-reserve"),
            message: String::from("try tomorrow"),
        };
        assert_eq!(
            reserve.to_string(),
            "Luna Reserve usage limit reached: try tomorrow"
        );
        let other = ProviderError::UsageLimit {
            model: String::from("gpt-6-luna"),
            message: String::from("try tomorrow"),
        };
        assert_eq!(other.to_string(), "usage limit reached: try tomorrow");
    }

    #[test]
    fn family_labels_distinguish_openai_codex_and_anthropic() {
        let cases = [
            (Family::Chat, "openai sent an invalid stream: x"),
            (Family::Responses, "openai sent an invalid stream: x"),
            (Family::Codex, "codex sent an invalid stream: x"),
            (Family::Anthropic, "anthropic sent an invalid stream: x"),
        ];
        for (family, expected) in cases {
            let error = ProviderError::Protocol {
                family,
                detail: String::from("x"),
            };
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn fix_sentences_keep_first_candidates_and_path_values() {
        let ambiguous = ProviderError::AmbiguousModel {
            id: String::from("gpt-5.6-luna"),
            candidates: String::from("p1/gpt-5.6-luna, p2/gpt-5.6-luna"),
        };
        assert_eq!(ambiguous.fix().as_deref(), Some("Use p1/gpt-5.6-luna."));
        let path = PathBuf::from("/data/dir with space/auth.json");
        let perms = ProviderError::AuthFilePerms { path: path.clone() };
        assert_eq!(
            perms.fix().as_deref(),
            Some("Run chmod 600 /data/dir with space/auth.json.")
        );
        match &perms {
            ProviderError::AuthFilePerms { path: stored } => assert_eq!(stored, &path),
            other => panic!("unexpected variant {other:?}"),
        }
    }

    #[test]
    fn retryable_by_loop_names_only_the_four_replayable_failures() {
        let retryable = [
            ProviderError::StreamCut,
            ProviderError::Overloaded,
            ProviderError::WsClosed { code: None },
            ProviderError::Transport {
                family: Family::Chat,
                reason: String::from("reset"),
            },
        ];
        for error in retryable {
            assert!(error.retryable_by_loop(), "{error}");
        }
        let terminal = [
            ProviderError::Status {
                family: Family::Chat,
                status: 500,
                message: String::new(),
            },
            ProviderError::InvalidRequest {
                message: String::from("bad"),
            },
            ProviderError::InvalidThinkingLevel {
                spelling: String::from("ultra"),
            },
            ProviderError::RateLimited {
                message: String::from("busy"),
                retry_after: None,
            },
            ProviderError::Quota {
                message: String::from("empty"),
            },
            ProviderError::Limit(LimitError::SseLine),
            ProviderError::AuthRejected {
                provider: String::from("openai"),
            },
        ];
        for error in terminal {
            assert!(!error.retryable_by_loop(), "{error}");
        }
    }

    #[test]
    fn context_overflow_is_typed_by_code_never_by_message() {
        for (family, code) in [
            (Family::Chat, "context_length_exceeded"),
            (Family::Codex, "context_window_exceeded"),
        ] {
            let error = ProviderError::context_overflow(family, code, "too long");
            assert!(
                matches!(&error, Some(ProviderError::ContextOverflow { family: f, code: c, message })
                    if *f == family && c == code && message == "too long"),
                "{error:?}"
            );
        }
        for code in ["invalid_request_error", "Context_Length_Exceeded", ""] {
            assert!(
                ProviderError::context_overflow(
                    Family::Anthropic,
                    code,
                    "prompt is too long: context window exceeded"
                )
                .is_none(),
                "{code}"
            );
        }
    }

    #[test]
    fn infer_failure_separates_overflow_from_an_ordinary_bad_request() {
        let overflow = ProviderError::context_overflow(
            Family::Chat,
            "context_length_exceeded",
            "maximum context length is 8192 tokens",
        )
        .map(InferFailure::from);
        assert!(matches!(
            &overflow,
            Some(InferFailure::Overflow { code, message })
                if &**code == "context_length_exceeded"
                    && &**message == "openai context window exceeded (context_length_exceeded): maximum context length is 8192 tokens"
        ));
        let ordinary = InferFailure::from(ProviderError::InvalidRequest {
            message: String::from("maximum context length is 8192 tokens"),
        });
        assert!(matches!(
            &ordinary,
            InferFailure::Fatal { message, fix: None }
                if &**message == "invalid request: maximum context length is 8192 tokens"
        ));
        assert_eq!(
            ordinary.to_string(),
            "invalid request: maximum context length is 8192 tokens"
        );
    }

    #[test]
    fn infer_failure_retries_only_transient_provider_failures() {
        let transient = [
            ProviderError::RateLimited {
                message: String::from("slow down"),
                retry_after: None,
            },
            ProviderError::Overloaded,
            ProviderError::StreamCut,
            ProviderError::WsClosed {
                code: Some((1011, String::from("busy"))),
            },
            ProviderError::Transport {
                family: Family::Anthropic,
                reason: String::from("connection reset"),
            },
        ];
        for error in transient {
            let text = error.to_string();
            let failure = InferFailure::from(error);
            assert!(
                matches!(&failure, InferFailure::Retryable { hint: None, message } if **message == *text),
                "{failure:?}"
            );
            assert_eq!(failure.to_string(), text);
        }
    }

    #[test]
    fn infer_failure_hints_only_the_rate_limit_wait() {
        let limited = ProviderError::RateLimited {
            message: String::from("slow down"),
            retry_after: Some(Duration::from_millis(2_500)),
        };
        let failure = InferFailure::from(limited);
        assert!(
            matches!(
                &failure,
                InferFailure::Retryable { hint: Some(hint), message }
                    if *hint == Duration::from_millis(2_500) && &**message == "rate limited: slow down"
            ),
            "{failure:?}"
        );
    }

    #[test]
    fn infer_failure_ends_auth_quota_and_exhausted_status_with_their_fix() {
        let cases = [
            (
                ProviderError::AuthRejected {
                    provider: String::from("p1"),
                },
                "p1 rejected the API key.",
                Some("Run dalgon login p1."),
            ),
            (
                ProviderError::SignInExpired {
                    provider: String::from("openai-codex"),
                },
                "openai-codex sign-in expired: the refresh token was rejected.",
                Some("Run dalgon login openai-codex."),
            ),
            (
                ProviderError::Quota {
                    message: String::from("out of credit"),
                },
                "quota exhausted: out of credit",
                None,
            ),
            (
                ProviderError::Status {
                    family: Family::Responses,
                    status: 503,
                    message: String::from("unavailable"),
                },
                "openai error 503: unavailable",
                None,
            ),
            (
                ProviderError::RetryAfterTooLong {
                    seconds: u64::MAX,
                    message: String::from("later"),
                },
                "rate limited for 18446744073709551615 s, which is over the 60 s wait limit: later",
                None,
            ),
        ];
        for (error, text, expected_fix) in cases {
            let failure = InferFailure::from(error);
            assert!(
                matches!(&failure, InferFailure::Fatal { message, fix }
                    if &**message == text && fix.as_deref() == expected_fix),
                "{failure:?}"
            );
        }
    }

    #[test]
    fn unresolved_blob_is_a_local_terminal_failure_naming_the_blob() {
        let blob_id = dal_core::BlobId::from_bytes(b"image bytes");
        let error = ProviderError::UnresolvedBlob { blob_id };
        let text = format!(
            "blob {blob_id} was not read from the session store before the request; no request was sent."
        );
        assert_eq!(error.to_string(), text);
        assert_eq!(error.fix(), None);
        assert!(!error.retryable_by_loop());
        assert!(matches!(
            InferFailure::from(error),
            InferFailure::Fatal { message, fix: None } if *message == *text
        ));
    }

    #[test]
    fn usage_check_reason_renderings() {
        assert_eq!(
            UsageCheckReason::Timeout.to_string(),
            "no reply within 15 s"
        );
        assert_eq!(
            UsageCheckReason::NotJsonObject.to_string(),
            "the body is not a JSON object"
        );
        let long = UsageCheckReason::Status {
            status: 401,
            message: format!("{}\u{e9}tail\nnext", "m".repeat(299)),
        };
        assert_eq!(long.to_string(), format!("401 {}", "m".repeat(299)));
        let transport = UsageCheckReason::Transport {
            reason: String::from("connection refused\nsecond line"),
        };
        assert_eq!(transport.to_string(), "connection refused");
    }

    #[test]
    fn resolve_error_texts_and_fixes() {
        let unknown = ResolveError::UnknownModel {
            reference: String::from("openai/gpt-reserve"),
        };
        assert_eq!(unknown.to_string(), "unknown model openai/gpt-reserve");
        assert_eq!(
            unknown.fix().as_deref(),
            Some("Run dalgon models to list the models.")
        );
        let ambiguous = ResolveError::AmbiguousModel {
            id: String::from("gpt-5.6-luna"),
            candidates: String::from("p1/id, p2/id"),
        };
        assert_eq!(
            ambiguous.to_string(),
            "model id gpt-5.6-luna matches several providers: p1/id, p2/id"
        );
        assert_eq!(ambiguous.fix().as_deref(), Some("Use p1/id."));
        let no_default = ResolveError::NoDefault;
        assert_eq!(no_default.to_string(), "no default model is configured");
        assert_eq!(no_default.fix(), None);
    }
}
