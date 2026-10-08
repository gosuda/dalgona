//! The provider failure enum with its exact display texts and fixes.
//!
//! Rendering reads stored data verbatim (first line, 300-byte UTF-8 cut);
//! [`ProviderError::fix`] returns at most one next-action sentence.

use std::{error::Error, fmt, path::PathBuf, time::Duration};

use dal_core::{Family, InferFailure};

use super::{CONTEXT_OVERFLOW_CODES, THINKING_LEVEL_NAMES, capped_message_line, family_label};
use crate::scripted::ScriptError;

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
    Limit(super::LimitError),
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
    /// Two advertised tools map to one provider wire name. Raised locally; no
    /// request is sent.
    ToolNameCollision {
        /// The wire name both tools map to.
        wire: String,
        /// The first advertised tool, by internal name.
        first: String,
        /// The second advertised tool, by internal name.
        second: String,
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
        reason: super::UsageCheckReason,
    },
    /// The scripted provider cannot serve the operation: its script is
    /// exhausted, the next step is for another operation, or the script
    /// breaks the stream grammar.
    Script(ScriptError),
    /// A synthetic model failed before or while producing its stream; the
    /// typed failure passes through to the fold unchanged.
    Synthetic(InferFailure),
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
            Self::ToolNameCollision {
                wire,
                first,
                second,
            } => write!(
                f,
                "tools {first} and {second} both map to the provider tool name {wire}; no request was sent."
            ),
            Self::Script(error) => write!(f, "{error}"),
            Self::Synthetic(failure) => write!(f, "{failure}"),
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
        match error {
            E::ContextOverflow { code, .. } => Self::Overflow {
                code: code.into_boxed_str(),
                message,
            },
            E::Status {
                family: _,
                status: 413,
                ..
            } => Self::Overflow {
                code: "request_too_large".into(),
                message,
            },
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
            E::Synthetic(failure) => failure,
            E::Status { .. }
            | E::RetryAfterTooLong { .. }
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
            | E::ToolNameCollision { .. }
            | E::Script(_) => Self::Fatal { message, fix },
        }
    }
}
