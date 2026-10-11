//! Reason enums for limits, usage reads, and model resolution.
//!
//! Each renders one exact sentence; fixes name the single next action.

use std::{error::Error, fmt};

use super::capped_message_line;

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
