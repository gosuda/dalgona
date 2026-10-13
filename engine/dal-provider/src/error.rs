//! Typed provider failures, their final display texts, and next-action fixes.
//!
//! Every variant stores the data it was given. [`ProviderError`] rendering
//! reads that data and never rewrites it: a server message appears as its
//! first line cut at 300 bytes on a UTF-8 boundary, and an empty message
//! renders as empty text. [`ProviderError::fix`] returns at most one
//! next-action sentence, and [`ProviderError::retryable_by_loop`] names the
//! failures an outer loop may replay after delivery. No text produced here
//! carries a token, a key, or an authorization header value.

use dal_core::Family;

mod provider;
mod reasons;

pub use provider::ProviderError;
pub use reasons::{LimitError, ResolveError, UsageCheckReason};

pub(crate) const MESSAGE_LINE_LIMIT: usize = 300;
pub(crate) const THINKING_LEVEL_NAMES: &str = "off, minimal, low, medium, high, xhigh, max";

/// Provider error codes that mean the request exceeds the model's context
/// window: `context_length_exceeded` (`OpenAI` Chat and Responses) and
/// `context_window_exceeded` (Codex). Anthropic reports an oversized prompt as
/// a plain `invalid_request_error`, so no Anthropic code is listed.
pub(crate) const CONTEXT_OVERFLOW_CODES: [&str; 2] =
    ["context_length_exceeded", "context_window_exceeded"];

pub(crate) fn family_label(family: Family) -> &'static str {
    match family {
        Family::Chat | Family::Responses => "openai",
        Family::Codex => "codex",
        Family::Anthropic => "anthropic",
    }
}

pub(crate) fn capped_message_line(message: &str) -> &str {
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

#[cfg(test)]
mod tests;
