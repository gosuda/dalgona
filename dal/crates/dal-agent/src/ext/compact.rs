//! Compaction consumer contract: selected-span input, replacement output.
//!
//! The agent loop owns the trigger, the cut, journal commit, shrink check,
//! breaker, and cache key. It freezes one selected prefix, builds one
//! [`CompactInput`], and runs registered [`Compactor`]s in order. A
//! compactor never recuts: [`CompactInput::covered_context`] and
//! [`CompactInput::compact_params`] return the selected values only.
//!
//! Remote (native) compactors send `Purpose::Compact` with an empty system
//! and no tools, returning the provider's opaque value unchanged and
//! mapping `Unsupported` to `Ok(None)`. Summary (plain-text) compactors
//! send the same selected span with a nonempty system and
//! `compact_params().with_max_output_tokens(SUMMARY_MAX_OUTPUT_TOKENS)`.
//! Cancellation stays typed as
//! [`CompactError::Service`](CompactError::Service)
//! (`ServiceError::Cancelled`); refusals are `Fail`, never `Ok(None)`.

use std::sync::Arc;

use dal_core::CompactedHistory;
use dal_core::{ContextItem, EntryId, ModelRoute, Part, RequestParams, SessionId, Usage};

use super::{BoxFuture, Caller, ExtRecord, Services};
use crate::error::ServiceError;
pub use dal_provider::ImageProfile;

/// Plain-text output cap for summary compactors (plan constant).
pub const SUMMARY_MAX_OUTPUT_TOKENS: u32 = 4096;

/// One covered entry of the selected prefix, in journal order.
#[derive(Clone, Debug, PartialEq)]
pub struct CoveredEntry {
    /// Journal entry this context item was projected from.
    pub entry: EntryId,
    /// True when this entry starts a completed user turn.
    pub starts_user_turn: bool,
    /// Heuristic token estimate for this entry, four characters per token.
    /// Never a measured count; providers report only whole-request usage.
    pub estimated_tokens: u64,
    /// Model context content for this entry.
    pub content: ContextItem,
}

/// Borrowed compaction input. The span is selected by the caller.
#[derive(Debug)]
pub struct CompactInput<'a> {
    /// Host-minted caller; compactors never forge one.
    pub caller: &'a Caller,
    /// Resolved route to compact for; rebound never replayed.
    pub model: ModelRoute,
    /// Owning session (journal-pointer session).
    pub session: SessionId,
    /// Manual `/compact` focus text; empty for automatic runs.
    pub instructions: &'a str,
    /// Selected covered entries, in journal order.
    pub covered: &'a [CoveredEntry],
    /// Selected covered inclusive span `(first, last)`; no recut.
    pub span: (EntryId, EntryId),
    /// First retained entry after the cut; `None` when nothing is retained.
    pub first_kept: Option<EntryId>,
    /// Model context window when known; `None` disables the auto trigger.
    pub context_window: Option<u64>,
    /// Current model image limits and full-frame billing, when known.
    pub image_profile: Option<ImageProfile>,
    /// Images in the retained context outside the covered prefix.
    pub images_elsewhere: usize,
    /// Summary text carried forward from an earlier compaction.
    pub carried: Option<Box<str>>,
    /// Total projected token count for the branch.
    pub total_tokens: u64,
    /// Selected request parameters for this span.
    pub params: RequestParams,
}

impl CompactInput<'_> {
    /// Returns the selected context; never re-runs the cut.
    #[must_use]
    pub fn covered_context(&self) -> Arc<[ContextItem]> {
        self.covered
            .iter()
            .map(|entry| entry.content.clone())
            .collect()
    }

    /// Returns the selected request parameters for this span.
    #[must_use]
    pub fn compact_params(&self) -> RequestParams {
        self.params.clone()
    }

    /// Returns the first covered entry: the journal-pointer entry.
    #[must_use]
    pub fn from_entry(&self) -> EntryId {
        self.span.0
    }

    /// Returns the selected covered inclusive span.
    #[must_use]
    pub fn covered_span(&self) -> (EntryId, EntryId) {
        self.span
    }
}

/// Replacement content for one committed compaction.
#[derive(Clone, Debug, PartialEq)]
pub enum Replacement {
    /// Plain-text summary checkpoint.
    Text(Box<str>),
    /// Opaque provider-native history, bound to its route.
    Native(CompactedHistory),
    /// Image-bearing local history replacement and its journal records.
    Parts {
        /// New replacement content, in provider context order.
        parts: Vec<Part>,
        /// Extension records committed atomically with the replacement.
        letters: Vec<ExtRecord>,
        /// Token cost of all replacement parts.
        parts_tokens: u64,
    },
}

/// One committed replacement for the selected covered span.
#[derive(Clone, Debug, PartialEq)]
pub struct Compaction {
    /// Covered inclusive span `(first, last)` this replaces.
    pub span: (EntryId, EntryId),
    /// Replacement content.
    pub replacement: Replacement,
    /// Provider-reported usage for the compaction request, if any.
    pub usage: Option<Usage>,
}

impl Compaction {
    /// Builds a text-only replacement.
    #[must_use]
    pub fn text(span: (EntryId, EntryId), text: impl Into<Box<str>>, usage: Option<Usage>) -> Self {
        Self {
            span,
            replacement: Replacement::Text(text.into()),
            usage,
        }
    }

    /// Builds a native opaque replacement bound to its route.
    #[must_use]
    pub fn native(
        span: (EntryId, EntryId),
        history: CompactedHistory,
        usage: Option<Usage>,
    ) -> Self {
        Self {
            span,
            replacement: Replacement::Native(history),
            usage,
        }
    }

    /// Returns the summary text for a text replacement, if any.
    #[must_use]
    pub fn summary_text(&self) -> Option<&str> {
        match &self.replacement {
            Replacement::Text(text) => Some(text),
            Replacement::Native(_) | Replacement::Parts { .. } => None,
        }
    }

    /// Returns the opaque history for a native replacement, if any.
    #[must_use]
    pub fn history(&self) -> Option<&CompactedHistory> {
        match &self.replacement {
            Replacement::Native(history) => Some(history),
            Replacement::Text(_) | Replacement::Parts { .. } => None,
        }
    }
}

/// Compaction failure: typed service cause or refusal text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompactError {
    /// A service call failed; the typed cause is preserved.
    ///
    /// Cancellation is this variant with
    /// `ServiceError::Cancelled`: no separate variant exists.
    #[error(transparent)]
    Service(#[from] ServiceError),
    /// The compactor refused; the chain tries the next compactor.
    #[error("{0}")]
    Fail(Box<str>),
}

impl CompactError {
    /// Builds a refusal with the exact product text.
    #[must_use]
    pub fn fail(message: impl Into<Box<str>>) -> Self {
        Self::Fail(message.into())
    }

    /// Reports whether this error is a cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Service(ServiceError::Cancelled))
    }
}

/// One link in the host-run compaction chain.
pub trait Compactor: Send + Sync + 'static {
    /// Runs one attempt over the selected span. `Ok(None)` refuses and
    /// advances the chain; `Err` refuses with a notice except for
    /// cancellation, which stops the chain with no entry.
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>>;
}
