// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The `history` compactor: catalog gate, then the image pipeline.
//!
//! The compactor asks its [`ImageHost`] for the model's complete catalog
//! image profile and refuses with the exact no-image-profile notice when the
//! host has none, so the local text summary runs. [`LandedHost`] reads the
//! journal text from the covered entries, prefetches stored text, and
//! commits image parts and letter records through dal's compaction record.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use dal_agent::ext::{
    BoxFuture, Caller, CompactError, CompactInput, Compaction, Compactor, ExtRecord, Replacement,
    Services,
};
use dal_core::{EntryId, Name, Part, RawJson};
use dal_ext::Font;
use tokio::sync::Semaphore;

use super::MAX_CONCURRENT_RENDERS;
use super::pipeline::{
    Budget, Commit, Decline, Drawn, DrawnLetter, Engine, ImageProfile, KnownLetter, Limits,
    Request, Slot, SourceReader, stale,
};
use super::records::{LetterRecord, journal_input, text_blobs};
use super::selection::entry_id;
use super::spans::CompactPiece;

/// Process-wide render gate: at most four concurrent renders across every
/// session. Compactors hold only a clone; the semaphore itself is static.
static PERMITS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_RENDERS)));

/// Journal pieces and the source that reads their exact bytes.
type JournalInput = (Vec<CompactPiece>, Arc<dyn SourceReader>);

/// The host services the image path needs beyond [`CompactInput`].
pub(crate) trait ImageHost: Send + Sync + 'static {
    /// Reads the source pieces and journal bytes of the selected span.
    fn source<'a>(
        &'a self,
        input: &'a CompactInput<'_>,
        services: &'a Arc<dyn Services>,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<JournalInput, CompactError>>;

    /// Returns the sink that stores PNG blobs and commits letter records.
    fn sink(&self, services: &Arc<dyn Services>, caller: &Caller) -> Option<Arc<dyn Commit>>;
}

/// The host as landed in dal today.
///
/// The covered journal entries supply the source pieces; stored text is
/// prefetched through the blob service so it stays text. The sink commits
/// each PNG once under its digest and returns image-bearing replacement
/// parts plus the fresh letter records, atomically with the compaction.
pub(crate) struct LandedHost;

impl ImageHost for LandedHost {
    fn source<'a>(
        &'a self,
        input: &'a CompactInput<'_>,
        services: &'a Arc<dyn Services>,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<JournalInput, CompactError>> {
        Box::pin(async move {
            let mut blobs: HashMap<(u64, u32), Arc<[u8]>> = HashMap::new();
            for (key, id) in text_blobs(input.covered) {
                let bytes = services
                    .blob_get(caller, *id.as_bytes())
                    .await?
                    .ok_or(Decline::SourceUnavailable)?;
                blobs.insert(key, Arc::from(bytes));
            }
            let (pieces, source) =
                journal_input(input.covered, &blobs).ok_or(Decline::SourceUnavailable)?;
            let source: Arc<dyn SourceReader> = Arc::new(source);
            Ok((pieces, source))
        })
    }

    fn sink(&self, services: &Arc<dyn Services>, caller: &Caller) -> Option<Arc<dyn Commit>> {
        Some(Arc::new(PartsSink::new(
            Arc::new(SessionBlobs {
                services: Arc::clone(services),
                caller: caller.clone(),
            }),
            caller.ext().clone(),
        )))
    }
}

/// Stores PNG blobs under their content digest.
pub(crate) trait BlobStore: Send + Sync + 'static {
    /// Returns the digest of the stored bytes.
    fn put(&self, png: Vec<u8>) -> BoxFuture<'_, Result<[u8; 32], CompactError>>;
}

/// The session blob service as a [`BlobStore`].
struct SessionBlobs {
    services: Arc<dyn Services>,
    caller: Caller,
}

impl BlobStore for SessionBlobs {
    fn put(&self, png: Vec<u8>) -> BoxFuture<'_, Result<[u8; 32], CompactError>> {
        let put = self.services.blob_put(&self.caller, png);
        Box::pin(async move { put.await.map_err(CompactError::from) })
    }
}

/// The production sink: one blob per PNG, image parts, fresh letter records.
pub(crate) struct PartsSink {
    store: Arc<dyn BlobStore>,
    ext: Name,
}

impl PartsSink {
    /// Builds a sink over one blob store and the contributing extension.
    #[must_use]
    pub(crate) fn new(store: Arc<dyn BlobStore>, ext: Name) -> Self {
        Self { store, ext }
    }
}

impl Commit for PartsSink {
    fn commit(
        &self,
        span: (EntryId, EntryId),
        drawn: Drawn,
    ) -> BoxFuture<'_, Result<Option<Compaction>, CompactError>> {
        Box::pin(async move {
            let mut parts = Vec::with_capacity(drawn.slots.len());
            for slot in &drawn.slots {
                match slot {
                    Slot::Text(text) => parts.push(Part::Text { text: text.clone() }),
                    Slot::Image(index) => {
                        let letter = &drawn.letters[*index];
                        let digest = self.store.put(letter.png.clone()).await?;
                        check_digest(letter, digest)?;
                        parts.push(Part::Image {
                            mime: "image/png".into(),
                            bytes: letter.png.clone().into(),
                        });
                    }
                }
            }
            let mut letters = Vec::new();
            for letter in &drawn.letters {
                if letter.reused {
                    continue;
                }
                letters.push(letter_record(self.ext.clone(), letter)?);
            }
            Ok(Some(Compaction {
                span,
                replacement: Replacement::Parts {
                    parts,
                    letters,
                    parts_tokens: drawn.parts_tokens,
                },
                usage: None,
            }))
        })
    }
}

/// Checks that a stored PNG landed under its recorded digest.
fn check_digest(letter: &DrawnLetter, digest: [u8; 32]) -> Result<(), CompactError> {
    let LetterRecord::Compaction { png_blob, .. } = &letter.record else {
        return Err(Decline::Inconsistent.into());
    };
    let recorded = blake3::Hash::from_hex(png_blob.as_str()).ok();
    if recorded.is_some_and(|hash| *hash.as_bytes() == digest) {
        Ok(())
    } else {
        Err(Decline::Inconsistent.into())
    }
}

/// Builds the extension record of one fresh letter.
fn letter_record(ext: Name, letter: &DrawnLetter) -> Result<ExtRecord, CompactError> {
    let body = sonic_rs::to_string(&letter.record)
        .map_err(|_| CompactError::fail("history: a letter record could not be encoded."))?;
    let body = RawJson::parse(&body)
        .map_err(|_| CompactError::fail("history: a letter record could not be encoded."))?;
    Ok(ExtRecord {
        ext,
        kind: "letter".into(),
        body,
    })
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
            let sink = self
                .host
                .sink(&services, input.caller)
                .ok_or(Decline::CannotCommit)?;
            let next_value = input
                .span
                .1
                .get()
                .checked_add(1)
                .ok_or(Decline::Inconsistent)?;
            let next = entry_id(next_value).ok_or(Decline::Inconsistent)?;
            let known = known_letters(&services, input.caller, next, &profile).await?;
            let (pieces, source) = self.host.source(&input, &services, input.caller).await?;
            let images_elsewhere = input.images_elsewhere;
            let ordinal = next_ordinal(&known);
            let budget = Budget {
                window_tokens: input.context_window,
                total_tokens: input.total_tokens,
                images_elsewhere,
                image_bytes_elsewhere: usize::try_from(input.image_bytes_elsewhere)
                    .unwrap_or(usize::MAX),
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
            )
            .with_known(known);
            if let Some(carried) = input.carried {
                request = request.with_carried(carried);
            }
            self.engine.run(request, profile, sink).await
        })
    }
}

/// Returns the next one-based compaction ordinal on the current path.
fn next_ordinal(known: &[KnownLetter]) -> u32 {
    known
        .iter()
        .filter_map(|letter| letter.record.compaction_ordinal())
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

/// Reads the stored letter table of the current path.
///
/// A record that fails to decode refuses the image path with its structural
/// error. A stale or damaged PNG keeps its record; it is listed, not reused.
async fn known_letters(
    services: &Arc<dyn Services>,
    caller: &Caller,
    next: EntryId,
    profile: &ImageProfile,
) -> Result<Vec<KnownLetter>, CompactError> {
    let bodies = services.records(caller, "letter").await?;
    let mut known = Vec::with_capacity(bodies.len());
    for body in &bodies {
        let record = LetterRecord::decode(body, next).map_err(Decline::from)?;
        if stale(&record, profile) {
            known.push(KnownLetter { record, png: None });
            continue;
        }
        let png = read_png(services, caller, &record).await?;
        known.push(KnownLetter { record, png });
    }
    Ok(known)
}

/// Reads the stored PNG of one letter and checks it against its digest.
async fn read_png(
    services: &Arc<dyn Services>,
    caller: &Caller,
    record: &LetterRecord,
) -> Result<Option<Vec<u8>>, CompactError> {
    let LetterRecord::Compaction { png_blob, .. } = record else {
        return Ok(None);
    };
    let Ok(digest) = blake3::Hash::from_hex(png_blob.as_str()) else {
        return Ok(None);
    };
    let Some(png) = services.blob_get(caller, *digest.as_bytes()).await? else {
        return Ok(None);
    };
    Ok((blake3::hash(&png).to_hex().as_str() == png_blob.as_str()).then_some(png))
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
