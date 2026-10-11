//! Remote history compaction for provider families that implement it.
//!
//! Each remote function performs exactly one attempt. The provider lifecycle
//! owns retry classification, credential refresh, the Codex retry cap, and the
//! request semaphore. A history is constructed only after a complete provider
//! response, so cancelling or failing an attempt cannot publish a partial one.

use dal_core::{Family, RawJson};

use crate::{error::ProviderError, lifecycle::AttemptFailure};

mod attempt;
mod bodies;
mod support;

pub(crate) use attempt::{anthropic, chat, openai_codex, openai_responses};

pub use dal_core::{CompactOutcome, CompactedHistory};

pub(crate) type AttemptResult = Result<Option<CompactedHistory>, AttemptFailure>;

/// Maximum number of Codex compaction stream retries, in addition to the first
/// attempt. The lifecycle owns and applies this cap.
pub(crate) const CODEX_STREAM_RETRIES: u32 = 2;

pub(crate) const CODEX_RETAINED_USER_TOKEN_LIMIT: usize = 64_000;
pub(crate) const CODEX_RETAINED_USER_BYTE_LIMIT: usize = CODEX_RETAINED_USER_TOKEN_LIMIT * 4;
pub(crate) const COMPACTION_TRIGGER: &[u8] = br#"{"type":"compaction_trigger"}"#;

/// Returns history items only when the request has the producing identity.
///
/// Histories stay bound to the family and model that produced them; the agent
/// loop calls this before replaying compacted items into a new request.
///
/// # Errors
/// Returns [`ProviderError::CompactionForeign`] for another family or model.
pub fn items_for<'history>(
    history: &'history CompactedHistory,
    family: Family,
    model: &str,
) -> Result<&'history [RawJson], ProviderError> {
    if family != history.family || model != history.model.as_ref() {
        return Err(ProviderError::CompactionForeign {
            bound_family: history.family,
            bound_model: history.model.to_string(),
            family,
            model: String::from(model),
        });
    }
    Ok(&history.items)
}

#[cfg(test)]
mod tests;
