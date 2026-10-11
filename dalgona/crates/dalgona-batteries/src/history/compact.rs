// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The `history` compactor: catalog gate, then the image pipeline.
//!
//! The compactor asks its [`ImageHost`] for the model's complete catalog
//! image profile and refuses with the exact no-image-profile notice when the
//! host has none, so the local text summary runs. The landed dal API carries
//! no catalog profile, no blob service, and no image-bearing replacement;
//! [`LandedHost`] states exactly that.

use std::num::NonZeroU64;
use std::sync::{Arc, LazyLock};

use dal_agent::ext::{
    BoxFuture, Caller, CompactError, CompactInput, Compaction, Compactor, Services,
};
use dal_core::EntryId;
use dal_ext::Font;
use tokio::sync::Semaphore;

use super::MAX_CONCURRENT_RENDERS;
use super::pipeline::{Budget, Commit, Decline, Engine, Limits, Request, SourceReader};
use super::records::LetterRecord;
use super::spans::CompactPiece;

/// Process-wide render gate: at most four concurrent renders across every
/// session. Compactors hold only a clone; the semaphore itself is static.
static PERMITS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_RENDERS)));

/// The host services the image path needs beyond [`CompactInput`].
pub(crate) trait ImageHost: Send + Sync + 'static {
    /// Returns source pieces and a journal reader for the selected span.
    fn source(
        &self,
        input: &CompactInput<'_>,
    ) -> Option<(Vec<CompactPiece>, Arc<dyn SourceReader>)>;

    /// Returns the sink that stores PNG blobs and commits letter records.
    fn sink(&self, services: &Arc<dyn Services>, caller: &Caller) -> Option<Arc<dyn Commit>>;
}

/// The host as landed in dal today.
///
/// dal supplies neither a catalog image profile nor a blob store nor an
/// image-bearing replacement to compactors, so this host reports no profile.
pub(crate) struct LandedHost;

impl ImageHost for LandedHost {
    fn source(
        &self,
        _input: &CompactInput<'_>,
    ) -> Option<(Vec<CompactPiece>, Arc<dyn SourceReader>)> {
        None
    }

    fn sink(&self, _services: &Arc<dyn Services>, _caller: &Caller) -> Option<Arc<dyn Commit>> {
        None
    }
}

/// The `history` compactor over the selected span.
pub struct HistoryCompactor {
    share: f64,
    host: Arc<dyn ImageHost>,
    engine: Engine,
}

impl HistoryCompactor {
    /// Builds a compactor for an enabled `[plugin.history]` section.
    #[must_use]
    pub(crate) fn new(share: f64) -> Self {
        Self::with_host(share, Arc::new(LandedHost), Limits::standard())
    }

    /// Builds a compactor over an explicit host and limits.
    #[must_use]
    pub(crate) fn with_host(share: f64, host: Arc<dyn ImageHost>, limits: Limits) -> Self {
        Self {
            share,
            host,
            engine: Engine::new(Arc::new(Font::embedded()), Arc::clone(&PERMITS), limits),
        }
    }
}

impl Compactor for HistoryCompactor {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        Box::pin(async move {
            let profile = input.image_profile.ok_or(Decline::NoImageProfile)?;
            let (pieces, source) = self.host.source(&input).ok_or(Decline::SourceUnavailable)?;
            let images_elsewhere = input.images_elsewhere;
            let ordinal = next_ordinal(&services, input.caller).await?;
            let budget = Budget {
                window_tokens: input.context_window,
                total_tokens: input.total_tokens,
                images_elsewhere,
                share: self.share,
            };
            let mut request = Request::new(
                input.session,
                input.covered,
                input.span,
                ordinal,
                budget,
                pieces,
                source,
            );
            if let Some(carried) = input.carried {
                request = request.with_carried(carried);
            }
            let sink = self
                .host
                .sink(&services, input.caller)
                .ok_or(Decline::CannotCommit)?;
            self.engine.run(request, profile, sink).await
        })
    }
}

/// Returns the next one-based compaction ordinal on the current path.
async fn next_ordinal(services: &Arc<dyn Services>, caller: &Caller) -> Result<u32, CompactError> {
    let bodies = services.records(caller, "letter").await?;
    let newest = EntryId::new(NonZeroU64::MAX);
    let highest = bodies
        .iter()
        .filter_map(|body| LetterRecord::decode(body, newest).ok())
        .filter_map(|record| record.compaction_ordinal())
        .max()
        .unwrap_or(0);
    Ok(highest.saturating_add(1))
}

/// A compactor that refuses every span with one fixed message.
///
/// An invalid `[plugin.history]` section installs this compactor so startup
/// continues and every compaction surfaces the exact configuration warning
/// through the normal refusal path.
pub struct FailingCompactor {
    message: Box<str>,
}

impl FailingCompactor {
    /// Builds a compactor that always fails with `message`.
    #[must_use]
    pub(crate) fn new(message: Box<str>) -> Self {
        Self { message }
    }
}

impl Compactor for FailingCompactor {
    fn compact<'a>(
        &'a self,
        _input: CompactInput<'a>,
        _services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        Box::pin(async move { Err(CompactError::fail(self.message.clone())) })
    }
}
